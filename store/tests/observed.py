"""Run against the real local Server binary using a private HTTPS upstream."""
import functools
import hashlib
import http.cookiejar
import http.server
import io
import json
import os
from pathlib import Path
import secrets
import sqlite3
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zipfile


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write(path, value):
    path.write_text(json.dumps(value))


def run(root, binary):
    upstream = root / "upstream"
    upstream.mkdir()
    (upstream / "recipes").mkdir()
    class Handler(http.server.SimpleHTTPRequestHandler):
        downloads = 0
        blocked = threading.Event()
        resume = threading.Event()
        pause_next = False

        def do_GET(self):
            if self.path == "/payload.zip":
                Handler.downloads += 1
                if Handler.pause_next:
                    Handler.pause_next = False
                    Handler.blocked.set()
                    Handler.resume.wait(15)
            super().do_GET()

        def log_message(self, *args):
            pass

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=upstream))
    origin = f"https://127.0.0.1:{httpd.server_port}"
    cert, key = root / "cert.pem", root / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1",
                    "-keyout", key, "-out", cert], check=True, stderr=subprocess.DEVNULL)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    httpd.socket = tls.wrap_socket(httpd.socket, server_side=True)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    def payload(version):
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            archive.writestr("bin/demo", version)
        content = data.getvalue()
        (upstream / "payload.zip").write_bytes(content)
        descriptor = dict(version=version, sha256=sha(content), bytes=len(content))
        write(upstream / "latest.json", descriptor)
        return descriptor

    component = payload("v1")
    recipe = dict(schema="StoreRecipeV1", revision=1, id="demo", title="Store Fixture", version="v1",
                  upstream=origin, rights="Fixture authored for this test.", availability="available",
                  platform="linux-x86_64", components=[dict(name="base", url=origin + "/payload.zip",
                  format="zip", discovery=origin + "/latest.json", sha256=component["sha256"], bytes=component["bytes"])],
                  prepare=[dict(component="base", from_prefix="", to_prefix="", include=["**"], exclude=[], on_conflict="reject")],
                  launch=dict(path="bin/demo", cwd="bin", args=[]), required=["bin/demo"])

    def index(generation, ids):
        entries = []
        for identity in ids:
            path = upstream / "recipes" / (identity + ".json")
            write(path, dict(recipe, id=identity, availability="reviewRequired" if identity=="another" else "available"))
            entries.append(dict(id=identity, sha256=sha(path.read_bytes())))
        write(upstream / "catalog.json", dict(schema="StoreCatalogV1", generation=generation, recipes=entries))

    index(1, ["demo"])
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = f"127.0.0.1:{sock.getsockname()[1]}"
    server = "http://" + address
    args = ["--namespace", str(root), "--instance", "fixture"]
    mode = "preview" if os.environ.get("STORE_BROWSER") else "dev"
    instance = Path(subprocess.check_output([binary, "init-" + mode, *args], text=True).strip())
    repo = Path(__file__).resolve().parents[2]
    def start():
        return subprocess.Popen([binary, "serve", *args, "--mode", mode, "--bind", address, "--assets", str(repo / "web/dist")],
                                env=dict(os.environ, APP_DEPOT_STORE_SOURCE=origin, CURL_CA_BUNDLE=str(cert)),
                                stdout=subprocess.PIPE, text=True)
    child = start()
    browser = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    headers = {"Content-Type": "application/json", "Origin": server}
    def api(method, path, body=None):
        request = urllib.request.Request(server + "/api/v1" + path, method=method, headers=headers,
                                         data=None if body is None else json.dumps(body).encode())
        with browser.open(request, timeout=10) as response:
            data = response.read()
        return json.loads(data) if data else None

    def rejected(status, method, path):
        try:
            api(method, path)
        except urllib.error.HTTPError as error:
            assert error.code == status, (method, path, error.code)
        else:
            raise AssertionError(f"{method} {path} unexpectedly succeeded")

    def wait_ready():
        for _ in range(100):
            try:
                api("GET", "/health/ready")
                return
            except OSError:
                time.sleep(.1)
        raise AssertionError("server readiness")

    def observation(status, version=None):
        for _ in range(150):
            row = api("GET", "/admin/store/observations")[0]
            if row["status"] == status and (version is None or row["detected"] == version):
                return row
            if row["status"] == "Failed" and status != "Failed":
                raise AssertionError(row)
            time.sleep(.15)
        raise AssertionError(row)

    try:
        wait_ready()
        for line in child.stdout:
            event = json.loads(line)
            if "token" in event:
                break
        rejected(401, "GET", "/admin/store")
        password = secrets.token_hex(20)
        api("POST", "/setup", dict(token=event["token"], username="admin", password=password))
        session = api("POST", "/auth/sessions", dict(username="admin", password=password))
        rejected(403, "POST", "/admin/store/refresh")
        headers["X-CSRF-Token"] = session["csrfToken"]
        assert api("POST", "/admin/store/refresh")["generation"] == 1
        if os.environ.get("STORE_BROWSER"):
            subprocess.run(["node", repo / "web/tests/store-browser.mjs", server], check=True,
                           env=dict(os.environ, STORE_FIXTURE_PASSWORD=password))
        else:
            api("POST", "/admin/store/demo/add")
        first = observation("Current")
        assert not first["automatic"] and first["hours"] == (8 if os.environ.get("STORE_BROWSER") else 24)
        api("PUT", "/admin/store/demo/observation",dict(hours=24,enabled=True,automatic=True))
        api("POST", "/admin/store/demo/add")
        api("POST", "/admin/store/demo/check")
        assert observation("Current")["release"] == first["release"]
        assert Handler.downloads == 1
        index(2, ["demo", "another"])
        assert len(api("POST", "/admin/store/refresh")["recipes"]) == 2
        rejected(409, "POST", "/admin/store/another/add")
        index(1, ["demo"])
        try:
            api("POST", "/admin/store/refresh")
            raise AssertionError("accepted catalog rollback")
        except urllib.error.HTTPError as error:
            assert error.code == 422
        assert api("GET", "/admin/store")["generation"] == 2
        index(3, ["demo", "another"])
        (upstream / "recipes/demo.json").write_bytes(b"{}")
        try:
            api("POST", "/admin/store/refresh")
            raise AssertionError("accepted incorrect recipe digest")
        except urllib.error.HTTPError as error:
            assert error.code == 422
        recipe["prepare"][0]["operation"] = "futureOperation"
        index(3, ["demo", "another"])
        rejected(422, "POST", "/admin/store/refresh")
        assert api("GET", "/admin/store")["generation"] == 2
        del recipe["prepare"][0]["operation"]
        index(3, ["demo", "another"])
        api("POST", "/admin/store/refresh")
        # Advance the persisted deadline in this disposable instance only.
        payload("v2")
        with sqlite3.connect(instance / "db" / "catalog.sqlite") as db:
            db.execute("UPDATE store_observations SET last_check=0")
        second = observation("Current", "v2")
        assert second["app"] == first["app"] and second["release"] != first["release"]
        plan = api("POST", "/releases/" + second["release"] + "/installation/resolve", {"platform":"linux/x86_64"})
        assert plan["launch"]["path"] == "bin/demo"
        assert api("GET", "/apps/" + first["app"])["kind"] == "standard"
        payload("v3")
        (upstream / "payload.zip").write_bytes(b"corrupt")
        api("POST", "/admin/store/demo/check")
        assert observation("Failed")["release"] == second["release"]
        payload("v3")
        Handler.pause_next = True
        api("POST", "/admin/store/demo/check")
        assert Handler.blocked.wait(10), "worker did not start the transfer"
        child.kill()
        child.wait()
        Handler.resume.set()
        child = start()
        wait_ready()
        third = observation("Current")
        assert third["release"] != second["release"]
        assert len(api("GET", "/apps/" + first["app"] + "/releases")["items"]) == 3
        api("PUT", "/admin/store/demo/observation", dict(hours=12, enabled=False, automatic=False))
        payload("v4")
        api("POST", "/admin/store/demo/check")
        row = observation("Update available", "v4")
        assert row["release"] == third["release"] and row["hours"] == 12 and not row["enabled"]
        recipe["revision"] = 2
        index(4, ["demo", "another"])
        api("POST", "/admin/store/refresh")
        assert api("GET", "/admin/store/observations")[0]["revision"] == 1
        api("POST", "/admin/store/demo/adopt")
        assert observation("Update available")["revision"] == 2
        api("POST", "/admin/store/demo/update")
        imported = observation("Current", "v4")
        assert not imported["automatic"] and imported["release"] != third["release"]
        recipe["revision"] = 3
        recipe["rights"] += " Metadata revision."
        index(5, ["demo", "another"])
        api("POST", "/admin/store/refresh")
        api("POST", "/admin/store/demo/adopt")
        assert observation("Current", "v4")["release"] == imported["release"]
        print("Catalog refresh, standard app, launch resolution, update, deduplication and restart recovery passed")
    finally:
        child.terminate()
        child.wait(timeout=30)
        httpd.shutdown()
        httpd.server_close()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as directory:
        run(Path(directory), Path(sys.argv[1]).resolve())
