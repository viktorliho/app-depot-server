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
import shutil
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
                if Handler.downloads == 1 and os.environ.get("STORE_BROWSER"):
                    for _ in range(300):
                        if (root/"browser-resume").exists():
                            break
                        time.sleep(.1)
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
    recipe = dict(schema="StoreRecipeV2", revision=1, id="demo", title="Store Fixture", version="v1",
                  upstream=origin, rights="Redistribution permission unconfirmed.", availability="available",
                  platform="linux-x86_64", components=[dict(name="base", url=origin + "/payload.zip",
                  format="zip", discovery=origin + "/latest.json", sha256=component["sha256"], bytes=component["bytes"])],
                  prepare=[dict(component="base", from_prefix="", to_prefix="", include=["**"], exclude=[], on_conflict="reject")],
                  launch=dict(path="bin/demo", cwd="bin", args=[]), required=["bin/demo"])

    def index(generation, ids):
        entries = []
        for identity in ids:
            path = upstream / "recipes" / (identity + ".json")
            product = {k: recipe[k] for k in ("revision","title","upstream","rights","components","schema")}
            builds = [dict(platform=p,version=recipe["version"],components=["base"],prepare=recipe["prepare"],
                           launch=recipe["launch"], required=recipe["required"]) for p in ("linux/x86_64","windows/x86_64","macos/aarch64")]
            if recipe["revision"] >= 4:
                windows_component = dict(recipe["components"][0],name="windows",url=origin+"/windows.zip",discovery=origin+"/windows.json")
                product["components"] = [*recipe["components"],windows_component]
                builds[1] = dict(builds[1],components=["windows"],prepare=[dict(step,component="windows") for step in recipe["prepare"]])
            write(path, dict(product, id=identity, steamId=7654321, publisher="Local test",summary="Synthetic Store application", builds=builds,
                             availability="reviewRequired" if identity=="another" else "available"))
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
    # Intercept only the synthetic rate-limit identity; all Store operations still
    # use the system curl and the private HTTPS upstream.
    tools_dir = root / "tools"
    tools_dir.mkdir()
    curl = tools_dir / "curl"
    curl.write_text("#!" + sys.executable + "\n" + """
import os, sys
if sys.argv[-1].startswith("https://store.steampowered.com/api/appdetails?appids=900000"):
    with open(os.environ["FIXTURE_RATE_COUNTER"], "a") as counter:
        counter.write("x")
    sys.stderr.write("429")
    sys.exit(22)
os.execv(os.environ["FIXTURE_REAL_CURL"],[os.environ["FIXTURE_REAL_CURL"],*sys.argv[1:]])
""")
    curl.chmod(0o700)
    def start():
        return subprocess.Popen([binary, "serve", *args, "--mode", mode, "--bind", address, "--assets", str(repo / "web/dist")],
                                env=dict(os.environ, APP_DEPOT_STORE_SOURCE=origin, CURL_CA_BUNDLE=str(cert),
                                         FIXTURE_REAL_CURL=shutil.which("curl"), FIXTURE_RATE_COUNTER=str(root/"rate-count"),
                                         PATH=str(tools_dir)+os.pathsep+os.environ["PATH"]),
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

    def rejected(status, method, path, body=None):
        try:
            api(method, path, body)
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
        rejected(401, "GET", "/admin/metadata-providers")
        password = secrets.token_hex(20)
        api("POST", "/setup", dict(token=event["token"], username="admin", password=password))
        session = api("POST", "/auth/sessions", dict(username="admin", password=password))
        rejected(403, "POST", "/admin/store/refresh")
        headers["X-CSRF-Token"] = session["csrfToken"]
        assert api("POST", "/admin/store/refresh")["generation"] == 1
        # Public automated runs seed the same bounded provider cache that a successful
        # fetch populates. No network request or external account is needed.
        provider_id = 7654321
        details_url = f"https://store.steampowered.com/api/appdetails?appids={provider_id}&l=english&cc=us"
        cdn = f"https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{provider_id}/"
        # Generate a CRC-valid RGBA PNG with standard library codecs.
        import struct
        import zlib
        def chunk(tag, data):
            return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
        image = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
                 + chunk(b"IDAT", zlib.compress(bytes([0, 25, 50, 75, 255]))) + chunk(b"IEND", b""))
        response = {str(provider_id): {"data": {"steam_appid": provider_id, "name": "Provider title",
                    "short_description": "Fixture description", "developers": ["Fixture developer"]}}}
        with sqlite3.connect(instance / "db" / "catalog.sqlite") as db:
            db.executemany("INSERT INTO provider_cache(resource,body,expires) VALUES(?,?,unixepoch()+3600)",
                           [(details_url,json.dumps(response).encode()),(cdn+"library_600x900.jpg",image),
                            (cdn+"header.jpg",image),(cdn+"library_hero.jpg",b"invalid"),(cdn+"logo.png",b"")])
        assert api("GET", "/admin/metadata-providers") == {"steam":True,"steamGridDb":False,"hasKey":False}
        assert api("GET", f"/metadata/steam/{provider_id}")["canonicalTitle"] == "Provider title"
        assert api("GET", f"/metadata/steam/search?q={provider_id}")[0]["name"] == "Store Fixture"
        with browser.open(server + f"/api/v1/metadata/steam/{provider_id}/artwork/steam.grid.vertical") as preview:
            assert preview.read() == image
        rejected(503, "GET", f"/metadata/steam/{provider_id}/artwork/steam.hero")
        api("PUT","/admin/metadata-providers",{"key":"fixture-only","steam":False})
        assert api("GET","/admin/metadata-providers") == {"steam":False,"steamGridDb":False,"hasKey":True}
        assert api("GET", f"/metadata/steam/{provider_id}")["canonicalTitle"] == "Store Fixture"
        api("PUT","/admin/metadata-providers",{"key":"","steam":True})
        headers["Idempotency-Key"] = "manual-enrichment-fixture"
        manual = api("POST", "/apps", {"name":"Operator title"})
        del headers["Idempotency-Key"]
        manual_path = "/apps/" + manual["id"]
        api("POST", manual_path+"/metadata/apply", {"id":provider_id,"fields":["summary"],"artwork":["steam.grid.vertical"]})
        assert api("GET",manual_path)["name"] == "Operator title"
        assert api("GET",manual_path+"/declarations")["data"]["appDepot"]["summary"] == "Fixture description"
        original_cover = api("GET",manual_path+"/artwork/steam.grid.vertical")
        api("POST",manual_path+"/metadata/apply",{"id":provider_id,"fields":["developer"],"artwork":[]})
        assert api("GET",manual_path+"/artwork/steam.grid.vertical")["etag"] == original_cover["etag"]
        for target, version in [("windows/x86_64","2.0"),("linux/aarch64","1.8"),("macos/aarch64","1.5")]:
            headers["Idempotency-Key"] = "manual-" + target
            release = api("POST",manual_path+"/releases",{"version":version,"buildPlatform":target})
            del headers["Idempotency-Key"]
            assert release["appId"] == manual["id"] and release["buildPlatform"] == target
            scoped = api("GET",manual_path+"/releases?platform="+target)["items"]
            assert [r["id"] for r in scoped] == [release["id"]]
        rejected(503, "GET", "/metadata/steam/9000001")
        rejected(503, "GET", "/metadata/steam/9000002")
        assert (root/"rate-count").read_text() == "x"
        if os.environ.get("STORE_BROWSER"):
            subprocess.run(["node", repo / "web/tests/store-browser.mjs", server], check=True,
                           env=dict(os.environ, STORE_FIXTURE_PASSWORD=password, STORE_FIXTURE_RESUME=str(root/"browser-resume"), STORE_FIXTURE_MANUAL_APP=manual["id"]))
        else:
            api("POST", "/admin/store/demo/add", {"platforms":["linux/x86_64"]})
        first = observation("Current")
        assert not first["automatic"] and first["hours"] == (8 if os.environ.get("STORE_BROWSER") else 24)
        api("PUT", "/admin/store/demo@linux-x86_64/observation",dict(hours=24,enabled=True,automatic=True))
        api("POST", "/admin/store/demo/add", {"platforms":["linux/x86_64"]})
        api("POST", "/admin/store/demo@linux-x86_64/check")
        assert observation("Current")["release"] == first["release"]
        assert Handler.downloads == 1
        index(2, ["demo", "another"])
        assert len(api("POST", "/admin/store/refresh")["recipes"]) == 2
        rejected(409, "POST", "/admin/store/another/add", {"platforms":["linux/x86_64"]})
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
        for field, invalid in [("schema", "StoreRecipeV999"), ("required", ["../outside"])]:
            original = recipe[field]
            recipe[field] = invalid
            index(3, ["demo", "another"])
            rejected(422, "POST", "/admin/store/refresh")
            assert api("GET", "/admin/store")["generation"] == 2
            recipe[field] = original
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
        app = api("GET", "/apps/" + first["app"])
        assert app["kind"] == "standard" and app["visibility"] == "Private"
        payload("v3")
        (upstream / "payload.zip").write_bytes(b"corrupt")
        api("POST", "/admin/store/demo@linux-x86_64/check")
        assert observation("Failed")["release"] == second["release"]
        payload("v3")
        Handler.pause_next = True
        api("POST", "/admin/store/demo@linux-x86_64/check")
        assert Handler.blocked.wait(10), "worker did not start the transfer"
        child.kill()
        child.wait()
        Handler.resume.set()
        child = start()
        wait_ready()
        third = observation("Current")
        assert third["release"] != second["release"]
        assert len(api("GET", "/apps/" + first["app"] + "/releases")["items"]) == 3
        api("PUT", "/admin/store/demo@linux-x86_64/observation", dict(hours=12, enabled=False, automatic=False))
        payload("v4")
        api("POST", "/admin/store/demo@linux-x86_64/check")
        row = observation("Update available", "v4")
        assert row["release"] == third["release"] and row["hours"] == 12 and not row["enabled"]
        recipe["revision"] = 2
        index(4, ["demo", "another"])
        api("POST", "/admin/store/refresh")
        assert api("GET", "/admin/store/observations")[0]["revision"] == 1
        api("POST", "/admin/store/demo@linux-x86_64/adopt")
        assert observation("Update available")["revision"] == 2
        api("POST", "/admin/store/demo@linux-x86_64/update")
        imported = observation("Current", "v4")
        assert not imported["automatic"] and imported["release"] != third["release"]
        recipe["revision"] = 3
        recipe["rights"] += " Metadata revision."
        index(5, ["demo", "another"])
        api("POST", "/admin/store/refresh")
        api("POST", "/admin/store/demo@linux-x86_64/adopt")
        assert observation("Current", "v4")["release"] == imported["release"]
        api("POST", "/admin/store/demo/add", {"platforms":["windows/x86_64","macos/aarch64"]})
        for _ in range(250):
            builds=api("GET","/admin/store/observations")
            if len(builds)==3 and all(b["status"]=="Current" for b in builds):break
            time.sleep(.1)
        assert len(builds)==3 and all(b["status"]=="Current" for b in builds), builds
        assert {b["app"] for b in builds}=={first["app"]}
        for b in builds:
            items=api("GET","/apps/"+first["app"]+"/releases?order=published&platform="+b["platform"])["items"]
            assert items[0]["id"]==b["release"] and all(x["buildPlatform"]==b["platform"] for x in items)
            assert api("POST","/releases/"+b["release"]+"/installation/resolve",{"platform":b["platform"]})["launch"]["path"]=="bin/demo"
        before={b["platform"]:b["release"] for b in builds}
        with zipfile.ZipFile(upstream/"windows.zip","w") as fixture:
            fixture.writestr("bin/demo","independent Windows update")
        data=(upstream/"windows.zip").read_bytes()
        write(upstream/"windows.json", dict(version="win-5",sha256=sha(data),bytes=len(data)))
        recipe["revision"]=4
        index(6,["demo","another"])
        api("POST","/admin/store/refresh")
        api("PUT","/admin/store/demo/observation",dict(enabled=True,hours=24,automatic=True))
        api("POST","/admin/store/demo@windows-x86_64/adopt")
        for _ in range(200):
            builds=api("GET","/admin/store/observations")
            win=next(b for b in builds if b["platform"]=="windows/x86_64")
            if win["status"]=="Current" and win["currentVersion"]=="win-5":break
            time.sleep(.1)
        assert win["currentVersion"]=="win-5" and win["release"]!=before[win["platform"]],win
        assert all(b["release"]==before[b["platform"]] for b in builds if b["platform"]!=win["platform"])
        print("Catalog refresh, standard app, launch resolution, update, deduplication and restart recovery passed")
    finally:
        child.terminate()
        child.wait(timeout=30)
        httpd.shutdown()
        httpd.server_close()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as directory:
        run(Path(directory), Path(sys.argv[1]).resolve())
