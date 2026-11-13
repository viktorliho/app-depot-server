use app_depot_infrastructure::{auth, init_local, lock_local, open_catalog, LocalMode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request_full(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let host = extra
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Host"))
        .map(|(_, value)| *value)
        .unwrap_or("localhost");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    )?;
    if !token.is_empty() {
        write!(stream, "Authorization: Bearer {token}\r\n")?;
    }
    for (name, value) in extra {
        if name.eq_ignore_ascii_case("Host") {
            continue;
        }
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let marker = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(std::io::ErrorKind::InvalidData)?;
    let head =
        std::str::from_utf8(&response[..marker]).map_err(|_| std::io::ErrorKind::InvalidData)?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or(std::io::ErrorKind::InvalidData)?
        .parse()
        .map_err(|_| std::io::ErrorKind::InvalidData)?;
    Ok((status, response[marker + 4..].to_vec(), head.to_owned()))
}

fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>)> {
    request_full(addr, method, path, token, extra, body).map(|(status, body, _)| (status, body))
}

fn start(root: &std::path::Path, addr: SocketAddr) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_app-depot-server"))
        .args([
            "serve",
            "--namespace",
            root.to_str().unwrap(),
            "--instance",
            "crash",
            "--mode",
            "dev",
            "--bind",
            &addr.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((200, _)) = request(addr, "GET", "/api/v1/health/ready", "unused", &[], &[]) {
            break;
        }
        assert!(Instant::now() < deadline, "server did not become ready");
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    server
}

fn start_production(
    root: &std::path::Path,
    addr: SocketAddr,
    trusted_peer: bool,
    _bootstrap: bool,
) -> Server {
    let mut command = Command::new(env!("CARGO_BIN_EXE_app-depot-server"));
    command
        .args([
            "serve",
            "--mode",
            "production",
            "--config",
            root.join("config.json").to_str().unwrap(),
        ])
        .env("APP_DEPOT_DATA_ROOT", root.join("production"))
        .env("APP_DEPOT_ASSETS", root.join("assets"))
        .env("APP_DEPOT_BIND", addr.to_string())
        .env(
            "APP_DEPOT_TRUSTED_PROXY_PEERS",
            if trusted_peer { "127.0.0.1" } else { "" },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = command.spawn().unwrap();
    let mut server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((200, _)) = request(addr, "GET", "/api/v1/health/ready", "", &[], &[]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Production test server did not become ready"
        );
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "Production test server exited"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    server
}

#[test]
fn compose_uses_persistent_root_without_bootstrap_overlay() {
    let compose = include_str!("../../../compose.yaml");
    assert!(compose.contains("ghcr.io/viktorliho/app-depot-server:latest"));
    assert!(compose.contains("/data/production"));
    assert!(!compose.contains("APP_DEPOT_BOOTSTRAP_SECRET"));
    assert!(!compose.contains("APP_DEPOT_SESSION_KEY_FILE"));
}

#[test]
fn production_setup_and_transport_security_on_real_server() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    fs::write(dir.path().join("assets/index.html"), "<h1>App Depot</h1>").unwrap();
    fs::write(
        dir.path().join("config.json"),
        r#"{"data_root":"/invalid"}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut first = start_production(dir.path(), addr, false, false);
    let (status, body) = request(addr, "GET", "/api/v1/setup", "", &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["required"],
        true
    );
    let (status, _) = request(addr, "GET", "/api/v1/users", "", &[], &[]).unwrap();
    assert_eq!(status, 401);
    let old_token = first_setup_token(&mut first);
    drop(first);
    let mut first = start_production(dir.path(), addr, false, false);
    let token = first_setup_token(&mut first);
    assert_ne!(token, old_token);
    let invalid = br#"{"token":"0000000000000000000000000000000000000000000000000000000000000000","username":"admin","password":"safe secret phrase 12345"}"#;
    let origin = [
        ("Content-Type", "application/json"),
        ("Origin", "http://localhost"),
    ];
    let stale =
        json!({"token": old_token, "username":"admin", "password":"safe secret phrase 12345"})
            .to_string();
    assert_eq!(
        request(addr, "POST", "/api/v1/setup", "", &origin, stale.as_bytes())
            .unwrap()
            .0,
        403
    );
    assert_eq!(
        request(addr, "POST", "/api/v1/setup", "", &origin, invalid)
            .unwrap()
            .0,
        403
    );
    let setup = json!({"token": token, "username":"admin", "password":"safe secret phrase 12345"})
        .to_string();
    assert_eq!(
        request(addr, "POST", "/api/v1/setup", "", &origin, setup.as_bytes())
            .unwrap()
            .0,
        201
    );
    assert_eq!(
        request(addr, "POST", "/api/v1/setup", "", &origin, setup.as_bytes())
            .unwrap()
            .0,
        403
    );
    let login = br#"{"username":"admin","password":"safe secret phrase 12345"}"#;
    let (status, _, head) = request_full(
        addr,
        "POST",
        "/api/v1/auth/sessions",
        "",
        &[
            ("Content-Type", "application/json"),
            ("Host", "depot.lan:8787"),
            ("Origin", "http://depot.lan:8787"),
        ],
        login,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert!(head.contains("HttpOnly; SameSite=Lax"));
    assert!(!head.contains("Secure;"));
    let (_, _, head) = request_full(
        addr,
        "POST",
        "/api/v1/auth/sessions",
        "",
        &[
            ("Content-Type", "application/json"),
            ("Origin", "http://localhost"),
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "localhost"),
        ],
        login,
    )
    .unwrap();
    assert!(!head.contains("Secure;"));
    assert_eq!(
        request(
            addr,
            "POST",
            "/api/v1/auth/sessions",
            "",
            &[
                ("Content-Type", "application/json"),
                ("Origin", "https://depot.example"),
                ("X-Forwarded-Proto", "https"),
                ("X-Forwarded-Host", "depot.example")
            ],
            login
        )
        .unwrap()
        .0,
        403
    );
    let session_key = fs::read(dir.path().join("production/ops/session-key")).unwrap();
    drop(first);
    let mut second = start_production(dir.path(), addr, true, false);
    assert_eq!(
        fs::read(dir.path().join("production/ops/session-key")).unwrap(),
        session_key
    );
    assert_eq!(
        request(addr, "GET", "/api/v1/setup", "", &[], &[])
            .unwrap()
            .0,
        200
    );
    let (status, _, head) = request_full(
        addr,
        "POST",
        "/api/v1/auth/sessions",
        "",
        &[
            ("Content-Type", "application/json"),
            ("Origin", "https://depot.example"),
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "depot.example"),
        ],
        login,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert!(head.contains("Secure; HttpOnly; SameSite=Lax"));
    assert_eq!(
        request(addr, "POST", "/api/v1/setup", "", &origin, setup.as_bytes())
            .unwrap()
            .0,
        403
    );
    assert!(Command::new("kill")
        .args(["-TERM", &second.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = second.0.try_wait().unwrap() {
            assert!(exit.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn first_setup_token(server: &mut Server) -> String {
    let mut logs = String::new();
    let reader = server.0.stdout.as_mut().unwrap();
    use std::io::BufRead;
    let mut buffered = std::io::BufReader::new(reader);
    loop {
        logs.clear();
        buffered.read_line(&mut logs).unwrap();
        if let Ok(event) = serde_json::from_str::<Value>(&logs) {
            if event["event"] == "initial_setup" {
                return event["token"].as_str().unwrap().into();
            }
        }
    }
}

#[tokio::test]
async fn acknowledged_range_survives_real_process_kill() {
    let dir = tempfile::tempdir().unwrap();
    init_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let lock = lock_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let pool = open_catalog(&lock).await.unwrap();
    fs::write(dir.path().join("bootstrap"), "safe secret phrase 12345").unwrap();
    fs::write(
        dir.path().join("session-key"),
        "session signing phrase long enough 12345",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["bootstrap", "session-key"] {
            fs::set_permissions(dir.path().join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let setup_token = auth::prepare_setup(&pool).await.unwrap().unwrap();
    assert!(
        auth::complete_setup(&pool, &setup_token, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
    );
    let user = auth::verify_password(&pool, "admin", "safe secret phrase 12345")
        .await
        .unwrap()
        .unwrap();
    let (token, _) = auth::issue_token(&pool, &user.id).await.unwrap();
    let outsider = auth::create_user(
        &pool,
        &user,
        "outsider-user",
        "outsider",
        "outsider secret phrase 12345",
        false,
    )
    .await
    .unwrap();
    let (outsider_token, _) = auth::issue_token(&pool, &outsider.id).await.unwrap();
    pool.close().await;
    drop(lock);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let first = start(dir.path(), addr);
    let create = json!({"mode":"BareFile","target":{"newAppName":"Crash"},"file":{"size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"}});
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/uploads",
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "crash-create"),
        ],
        create.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload: Value = serde_json::from_slice(&body).unwrap();
    let id = upload["id"].as_str().unwrap();
    let file_id = upload["file"]["fileId"].as_str().unwrap();
    let path = format!("/api/v1/uploads/{id}/files/{file_id}");
    let (status, _) = request(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Content-Type", "application/octet-stream"),
            ("Content-Range", "bytes 0-0/1"),
        ],
        b"A",
    )
    .unwrap();
    assert_eq!(status, 204);
    drop(first);
    let second = start(dir.path(), addr);
    let (status, body) =
        request(addr, "GET", &format!("{path}/progress"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    let progress: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(progress["ranges"], json!([{"start":0,"end":1}]));
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/finalize"),
        &token,
        &[("Idempotency-Key", "crash-finalize")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["phase"], "Completed");
    let artifact = result["artifactId"].as_str().unwrap();
    let (status, bytes) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/content"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, b"A");

    let app_id = result["appId"].as_str().unwrap();
    let release_id = result["releaseId"].as_str().unwrap();
    for path in [
        format!("/api/v1/apps/{app_id}"),
        format!("/api/v1/releases/{release_id}"),
        format!("/api/v1/artifacts/{artifact}/content"),
        format!("/api/v1/releases/{release_id}/declaration-snapshot"),
    ] {
        assert_eq!(
            request(addr, "GET", &path, &outsider_token, &[], &[])
                .unwrap()
                .0,
            404,
            "{path}"
        );
    }
    let apps: Value = serde_json::from_slice(
        &request(addr, "GET", "/api/v1/apps", &outsider_token, &[], &[])
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(apps["items"], json!([]));

    let app_path = format!("/api/v1/apps/{app_id}");
    let (status, _) = request(
        addr,
        "PATCH",
        &app_path,
        &token,
        &[("Content-Type", "application/json")],
        br#"{"visibility":"Public"}"#,
    )
    .unwrap();
    assert_eq!(status, 428);
    let app_tag = format!("\"app-{app_id}-1\"");
    let (status, _) = request(
        addr,
        "PATCH",
        &app_path,
        &token,
        &[("Content-Type", "application/json"), ("If-Match", &app_tag)],
        br#"{"visibility":"Public"}"#,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        request(addr, "GET", &app_path, &outsider_token, &[], &[])
            .unwrap()
            .0,
        404
    );

    let declaration_path = format!("{app_path}/declarations");
    let declaration_tag = format!("\"declarations-{app_id}-1\"");
    let (status, body) = request(
        addr,
        "PUT",
        &declaration_path,
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &declaration_tag),
        ],
        br#"{"data":null}"#,
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(serde_json::from_slice::<Value>(&body).unwrap()["data"].is_null());
    assert_eq!(
        request(
            addr,
            "PUT",
            &declaration_path,
            &token,
            &[
                ("Content-Type", "application/json"),
                ("If-Match", &declaration_tag)
            ],
            br#"{"data":false}"#
        )
        .unwrap()
        .0,
        412
    );
    let revisions: Value = serde_json::from_slice(
        &request(
            addr,
            "GET",
            &format!("{declaration_path}/revisions"),
            &token,
            &[],
            &[],
        )
        .unwrap()
        .1,
    )
    .unwrap();
    assert_eq!(revisions["items"].as_array().unwrap().len(), 2);

    let release_path = format!("/api/v1/releases/{release_id}");
    let release_tag = format!("\"release-{release_id}-1\"");
    let (status, body) = request(
        addr,
        "POST",
        &format!("{release_path}/publish"),
        &token,
        &[
            ("Idempotency-Key", "m4-publish"),
            ("If-Match", &release_tag),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        request(addr, "GET", &app_path, &outsider_token, &[], &[])
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        request(addr, "GET", &release_path, &outsider_token, &[], &[])
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/artifacts/{artifact}/content"),
            &outsider_token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        200
    );
    let snapshot: Value = serde_json::from_slice(
        &request(
            addr,
            "GET",
            &format!("{release_path}/declaration-snapshot"),
            &outsider_token,
            &[],
            &[],
        )
        .unwrap()
        .1,
    )
    .unwrap();
    assert!(snapshot["app"]["data"].is_null());
    let channel_path = format!("{app_path}/channels/stable.v1");
    let (status, body) = request(
        addr,
        "PUT",
        &channel_path,
        &token,
        &[("Content-Type", "application/json"), ("If-None-Match", "*")],
        json!({"releaseId": release_id}).to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        request(addr, "GET", &channel_path, &outsider_token, &[], &[])
            .unwrap()
            .0,
        200
    );
    let settings_path = "/api/v1/settings";
    let (status, _) = request(
        addr,
        "PATCH",
        settings_path,
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", "\"settings-1-1\""),
        ],
        br#"{"anonymousReadEnabled":true}"#,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        request(addr, "GET", &app_path, "", &[], &[]).unwrap().0,
        200
    );

    let create = json!({"mode":"BareFile","target":{"newAppName":"HTTP ranges"},"file":{"size":4,"sha256":format!("{:x}", Sha256::digest(b"ABCD"))}});
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/uploads",
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "ranges-create"),
        ],
        create.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload: Value = serde_json::from_slice(&body).unwrap();
    let id = upload["id"].as_str().unwrap();
    let file_id = upload["file"]["fileId"].as_str().unwrap();
    let path = format!("/api/v1/uploads/{id}/files/{file_id}");
    for (range, bytes) in [("bytes 2-3/4", &b"CD"[..]), ("bytes 0-0/4", &b"A"[..])] {
        let (status, _) = request(
            addr,
            "PUT",
            &path,
            &token,
            &[
                ("Content-Type", "application/octet-stream"),
                ("Content-Range", range),
            ],
            bytes,
        )
        .unwrap();
        assert_eq!(status, 204);
    }
    let (status, body) = request(
        addr,
        "GET",
        &format!("{path}/progress?pageSize=1"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    let first_page: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(first_page["ranges"], json!([{"start":0,"end":1}]));
    let cursor = first_page["nextCursor"].as_str().unwrap();
    let (status, body) = request(
        addr,
        "GET",
        &format!("{path}/progress?pageSize=1&cursor={cursor}"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    let second_page: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(second_page["ranges"], json!([{"start":2,"end":4}]));
    let (status, _) = request(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Content-Type", "application/octet-stream"),
            ("Content-Range", "bytes 1-1/4"),
        ],
        b"B",
    )
    .unwrap();
    assert_eq!(status, 204);
    let (status, body) = request(
        addr,
        "GET",
        &format!("{path}/progress?pageSize=1&cursor={cursor}"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 409);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["code"],
        "progress_changed"
    );
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/finalize"),
        &token,
        &[("Idempotency-Key", "ranges-finalize")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let result: Value = serde_json::from_slice(&body).unwrap();
    let artifact = result["artifactId"].as_str().unwrap();
    let (status, bytes) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/content"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, b"ABCD");

    let create = json!({"mode":"BareFile","target":{"newAppName":"Interrupted write"},"file":{"size":4,"sha256":format!("{:x}", Sha256::digest(b"ABCD"))}});
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/uploads",
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "interrupted-create"),
        ],
        create.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201);
    let upload: Value = serde_json::from_slice(&body).unwrap();
    let id = upload["id"].as_str().unwrap();
    let file_id = upload["file"]["fileId"].as_str().unwrap();
    let path = format!("/api/v1/uploads/{id}/files/{file_id}");
    let mut partial_stream = TcpStream::connect(addr).unwrap();
    write!(partial_stream, "PUT {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-3/4\r\nContent-Length: 4\r\nConnection: close\r\n\r\nAB").unwrap();
    partial_stream.flush().unwrap();
    let staged = dir.path().join("crash/staging").join(id).join("file");
    let deadline = Instant::now() + Duration::from_secs(3);
    while fs::metadata(&staged).map(|meta| meta.len()).unwrap_or(0) < 2 {
        assert!(
            Instant::now() < deadline,
            "partial request did not reach staging"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(second);
    drop(partial_stream);
    let _third = start(dir.path(), addr);
    let (status, body) =
        request(addr, "GET", &format!("{path}/progress"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["ranges"],
        json!([])
    );
    let (status, _) = request(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Content-Type", "application/octet-stream"),
            ("Content-Range", "bytes 0-3/4"),
        ],
        b"ABCD",
    )
    .unwrap();
    assert_eq!(status, 204);
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/finalize"),
        &token,
        &[("Idempotency-Key", "interrupted-finalize")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["phase"],
        "Completed"
    );

    let create = json!({"mode":"BareTree","target":{"newAppName":"HTTP tree"}});
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/uploads",
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "tree-http-create"),
        ],
        create.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload: Value = serde_json::from_slice(&body).unwrap();
    let id = upload["id"].as_str().unwrap();
    let inventory = json!({"entries":[{"path":"a.txt","type":"File","size":1,"sha256":format!("{:x}", Sha256::digest(b"A")),"executable":false}]});
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/inventory"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "tree-http-inventory"),
        ],
        inventory.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let added: Value = serde_json::from_slice(&body).unwrap();
    let file_id = added["entries"][0]["fileId"].as_str().unwrap();
    let (status, _) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/inventory/complete"),
        &token,
        &[("Idempotency-Key", "tree-http-complete")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    let (status, _) = request(
        addr,
        "PUT",
        &format!("/api/v1/uploads/{id}/files/{file_id}"),
        &token,
        &[
            ("Content-Type", "application/octet-stream"),
            ("Content-Range", "bytes 0-0/1"),
        ],
        b"A",
    )
    .unwrap();
    assert_eq!(status, 204);
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/uploads/{id}/finalize"),
        &token,
        &[("Idempotency-Key", "tree-http-finalize")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let completed: Value = serde_json::from_slice(&body).unwrap();
    let artifact = completed["artifactId"].as_str().unwrap();
    assert_eq!(
        artifact,
        "adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1"
    );
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/inventory"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let artifact_inventory: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(artifact_inventory["items"][0]["path"], "a.txt");
    let stable_file_id = artifact_inventory["items"][0]["fileId"].as_str().unwrap();
    let (status, bytes) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/files/{stable_file_id}"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, b"A");

    let app_id = completed["appId"].as_str().unwrap();
    let create_release = json!({});
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/apps/{app_id}/releases"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "association-release"),
        ],
        create_release.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let release: Value = serde_json::from_slice(&body).unwrap();
    let release_id = release["id"].as_str().unwrap();
    let create_association = json!({"artifactId": artifact});
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/releases/{release_id}/artifacts"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "association-add"),
        ],
        create_association.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let association: Value = serde_json::from_slice(&body).unwrap();
    let association_id = association["id"].as_str().unwrap();
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/v1/releases/{release_id}/artifacts?pageSize=1"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["items"][0]["id"],
        association_id
    );
    let etag = format!("\"association-{association_id}-1\"");
    let (status, body) = request(
        addr,
        "DELETE",
        &format!("/api/v1/associations/{association_id}"),
        &token,
        &[
            ("Idempotency-Key", "association-remove"),
            ("If-Match", &etag),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let operation: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(operation["accessRevoked"], true);
    assert_ne!(operation["status"], "Completed");
    let operation_id = operation["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = request(
            addr,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &token,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(status, 200);
        let value: Value = serde_json::from_slice(&body).unwrap();
        if value["status"] == "Completed" {
            assert_eq!(value["physicalCleanup"], "Completed");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deletion did not complete: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/associations/{association_id}/declarations"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        404
    );
    let (status, body) = request(
        addr,
        "POST",
        &format!("/api/v1/releases/{release_id}/artifacts"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "association-add"),
        ],
        create_association.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 409, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["code"],
        "idempotency_conflict"
    );

    let registered_association = completed["associationId"].as_str().unwrap();
    let validator = format!("\"association-{registered_association}-1\"");
    let (status, body) = request(
        addr,
        "DELETE",
        &format!("/api/v1/associations/{registered_association}"),
        &token,
        &[
            ("Idempotency-Key", "tree-registration-delete"),
            ("If-Match", &validator),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let deletion: Value = serde_json::from_slice(&body).unwrap();
    let deletion_id = deletion["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = request(
            addr,
            "GET",
            &format!("/api/v1/operations/{deletion_id}"),
            &token,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(status, 200);
        let value: Value = serde_json::from_slice(&body).unwrap();
        if value["status"] == "Completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registration deletion did not complete: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/uploads/{id}"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        404
    );
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/uploads",
        &token,
        &[
            ("Content-Type", "application/json"),
            ("Idempotency-Key", "tree-http-create"),
        ],
        create.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 409, "{}", String::from_utf8_lossy(&body));

    let (status, body) = request(
        addr,
        "DELETE",
        &format!("/api/v1/releases/{release_id}"),
        &token,
        &[
            ("Idempotency-Key", "tree-release-delete"),
            ("If-Match", &format!("\"release-{release_id}-1\"")),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let release_delete: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(release_delete["accessRevoked"], true);
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/releases/{release_id}"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        404
    );
    let (status, body) = request(
        addr,
        "DELETE",
        &format!("/api/v1/apps/{app_id}"),
        &token,
        &[
            ("Idempotency-Key", "tree-app-delete"),
            ("If-Match", &format!("\"app-{app_id}-2\"")),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let app_delete: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(app_delete["accessRevoked"], true);
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/apps/{app_id}"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        404
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/operations/{}", app_delete["id"].as_str().unwrap()),
            &outsider_token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        404
    );
    let (status, body) =
        request(addr, "GET", "/api/v1/admin/diagnostics", &token, &[], &[]).unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let diagnostics: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(diagnostics["readiness"], "Ready");
    assert!(diagnostics["requestFailures"].as_u64().unwrap() > 0);
    assert_eq!(
        request(
            addr,
            "GET",
            "/api/v1/admin/diagnostics",
            &outsider_token,
            &[],
            &[]
        )
        .unwrap()
        .0,
        403
    );

    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/backups",
        &outsider_token,
        &[("Idempotency-Key", "outsider-backup")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/backups",
        &token,
        &[("Idempotency-Key", "http-backup")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let backup: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(backup["backupOutputAvailable"], false);
    let backup_operation_id = backup["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let completed_backup = loop {
        let (status, body) = request(
            addr,
            "GET",
            &format!("/api/v1/operations/{backup_operation_id}"),
            &token,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let value: Value = serde_json::from_slice(&body).unwrap();
        if value["status"] == "Completed" {
            break value;
        }
        assert_ne!(value["status"], "Failed", "{value}");
        assert!(
            Instant::now() < deadline,
            "backup did not complete: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(completed_backup["backupOutputAvailable"], true);
    let backup_id = completed_backup["backupId"].as_str().unwrap();
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/v1/backups/{backup_id}"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let manifest: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(manifest["formatVersion"], 1);
    let member = manifest["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["kind"] == "SqliteSnapshot")
        .unwrap();
    let entry_id = member["entryId"].as_str().unwrap();
    let member_path = format!("/api/v1/backups/{backup_id}/entries/{entry_id}");
    let (status, bytes) = request(
        addr,
        "GET",
        &member_path,
        &token,
        &[("Range", "bytes=0-15")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 206);
    assert_eq!(bytes.len(), 16);
    assert_eq!(
        request(addr, "GET", &member_path, &outsider_token, &[], &[])
            .unwrap()
            .0,
        403
    );
    let (status, body) = request(addr, "GET", "/api/v1/backups", &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["items"][0]["id"], backup_id);
}
