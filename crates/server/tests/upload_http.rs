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

fn response_etag(head: &str) -> String {
    head.lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("etag"))
                .map(|(_, value)| value.trim().to_owned())
        })
        .expect("response ETag")
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

#[tokio::test]
async fn app_artwork_is_verified_private_shared_and_restorable() {
    const PNG: &[u8] = include_bytes!("fixtures/artwork.png");
    const JPEG: &[u8] = include_bytes!("fixtures/artwork.jpg");
    const WEBP: &[u8] = include_bytes!("fixtures/artwork.webp");
    const ICO: &[u8] = include_bytes!("fixtures/artwork-multi.ico");
    let dir = tempfile::tempdir().unwrap();
    init_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let lock = lock_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let pool = open_catalog(&lock).await.unwrap();
    let setup = auth::prepare_setup(&pool).await.unwrap().unwrap();
    assert!(
        auth::complete_setup(&pool, &setup, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
    );
    let admin = auth::verify_password(&pool, "admin", "safe secret phrase 12345")
        .await
        .unwrap()
        .unwrap();
    let (token, _) = auth::issue_token(&pool, &admin.id).await.unwrap();
    let outsider = auth::create_user(
        &pool,
        &admin,
        "reader",
        "reader",
        "safe secret phrase 12345",
        false,
    )
    .await
    .unwrap();
    let (reader_token, _) = auth::issue_token(&pool, &outsider.id).await.unwrap();
    pool.close().await;
    drop(lock);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let server = start(dir.path(), addr);
    let create_app = |name: &str, key: &str| -> String {
        let (status, body) = request(
            addr,
            "POST",
            "/api/v1/apps",
            &token,
            &[
                ("Idempotency-Key", key),
                ("Content-Type", "application/json"),
            ],
            json!({"name":name}).to_string().as_bytes(),
        )
        .unwrap();
        assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice::<Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let app = create_app("Artwork Draft", "artwork-app-1");
    let second = create_app("Shared bytes", "artwork-app-2");
    let path = format!("/api/v1/apps/{app}/artwork/icon");
    let (status, body, head) = request_full(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Idempotency-Key", "artwork-png"),
            ("If-None-Match", "*"),
            ("Content-Type", "image/jpeg"),
        ],
        PNG,
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let first: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(first["mimeType"], "image/png");
    assert_eq!(first["width"], 2);
    assert_eq!(first["height"], 3);
    let first_tag = response_etag(&head);
    let (status, _, replay_head) = request_full(
        addr,
        "PUT",
        &path,
        &token,
        &[("Idempotency-Key", "artwork-png"), ("If-None-Match", "*")],
        PNG,
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(response_etag(&replay_head), first_tag);
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[("Idempotency-Key", "artwork-png"), ("If-None-Match", "*")],
            JPEG
        )
        .unwrap()
        .0,
        409
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[("Idempotency-Key", "bad-if-match-star"), ("If-Match", "*")],
            JPEG
        )
        .unwrap()
        .0,
        412
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[
                ("Idempotency-Key", "bad-if-none-etag"),
                ("If-None-Match", &first_tag)
            ],
            JPEG
        )
        .unwrap()
        .0,
        412
    );
    let (status, bytes, head) =
        request_full(addr, "GET", &format!("{path}/content"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, PNG);
    assert!(head
        .to_ascii_lowercase()
        .contains("content-type: image/png"));
    assert!(head
        .to_ascii_lowercase()
        .contains("x-content-type-options: nosniff"));
    assert_eq!(
        request(addr, "GET", &format!("{path}/content"), "", &[], &[])
            .unwrap()
            .0,
        401
    );
    drop(server);
    let lock = lock_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let pool = open_catalog(&lock).await.unwrap();
    let interrupted = "11111111-1111-4111-8111-111111111111";
    sqlx::query("INSERT INTO artwork_claims(id, digest, created_at) VALUES(?, ?, 1)")
        .bind(interrupted)
        .bind(format!("{:x}", Sha256::digest(JPEG)))
        .execute(&pool)
        .await
        .unwrap();
    fs::write(
        dir.path()
            .join("crash/ops")
            .join(format!("artwork-{interrupted}")),
        JPEG,
    )
    .unwrap();
    pool.close().await;
    drop(lock);
    let server = start(dir.path(), addr);
    assert_eq!(
        request(addr, "GET", &format!("{path}/content"), &token, &[], &[])
            .unwrap()
            .1,
        PNG
    );
    assert!(!dir
        .path()
        .join("crash/ops")
        .join(format!("artwork-{interrupted}"))
        .exists());
    assert_eq!(
        request(addr, "GET", &path, &reader_token, &[], &[])
            .unwrap()
            .0,
        404
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{path}/content"),
            &reader_token,
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
            "PUT",
            &path,
            &reader_token,
            &[
                ("Idempotency-Key", "outsider-artwork"),
                ("If-Match", &first_tag)
            ],
            JPEG
        )
        .unwrap()
        .0,
        404
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[("Idempotency-Key", "bad-artwork"), ("If-Match", &first_tag)],
            b"not an image"
        )
        .unwrap()
        .0,
        422
    );
    let truncated = &PNG[..PNG.len() / 2];
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[
                ("Idempotency-Key", "unsupported-artwork"),
                ("If-Match", &first_tag)
            ],
            include_bytes!("fixtures/artwork.gif")
        )
        .unwrap()
        .0,
        422
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[
                ("Idempotency-Key", "truncated-artwork"),
                ("If-Match", &first_tag)
            ],
            truncated
        )
        .unwrap()
        .0,
        422
    );
    let second_path = format!("/api/v1/apps/{second}/artwork/custom.banner");
    assert_eq!(
        request(
            addr,
            "PUT",
            &second_path,
            &token,
            &[
                ("Idempotency-Key", "shared-artwork"),
                ("If-None-Match", "*")
            ],
            PNG
        )
        .unwrap()
        .0,
        200
    );
    let (status, body, head) = request_full(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Idempotency-Key", "artwork-jpeg"),
            ("If-Match", &first_tag),
        ],
        JPEG,
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["mimeType"],
        "image/jpeg"
    );
    let jpeg_tag = response_etag(&head);
    let (status, bytes, image_head) =
        request_full(addr, "GET", &format!("{path}/content"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, JPEG);
    assert!(image_head
        .to_ascii_lowercase()
        .contains("content-type: image/jpeg"));
    assert_ne!(jpeg_tag, first_tag);
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[
                ("Idempotency-Key", "stale-artwork"),
                ("If-Match", &first_tag)
            ],
            WEBP
        )
        .unwrap()
        .0,
        412
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{second_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        PNG
    );
    let (status, body, head) = request_full(
        addr,
        "PUT",
        &path,
        &token,
        &[("Idempotency-Key", "artwork-webp"), ("If-Match", &jpeg_tag)],
        WEBP,
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["mimeType"],
        "image/webp"
    );
    let webp_tag = response_etag(&head);
    let (status, bytes, image_head) =
        request_full(addr, "GET", &format!("{path}/content"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, WEBP);
    assert!(image_head
        .to_ascii_lowercase()
        .contains("content-type: image/webp"));
    let (status, body, head) = request_full(
        addr,
        "PUT",
        &path,
        &token,
        &[
            ("Idempotency-Key", "artwork-ico"),
            ("If-Match", &webp_tag),
            ("Content-Type", "image/jpeg"),
        ],
        ICO,
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let ico: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(ico["mimeType"], "image/png");
    assert_eq!(ico["width"], 64);
    assert_eq!(ico["height"], 64);
    let ico_tag = response_etag(&head);
    let (status, normalized, image_head) =
        request_full(addr, "GET", &format!("{path}/content"), &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    assert!(normalized.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(ico["digest"], format!("{:x}", Sha256::digest(&normalized)));
    assert!(image_head
        .to_ascii_lowercase()
        .contains("content-type: image/png"));
    assert_eq!(
        request(
            addr,
            "PUT",
            &format!("/api/v1/apps/{app}/artwork/steam.hero"),
            &token,
            &[("Idempotency-Key", "ico-not-icon"), ("If-None-Match", "*")],
            ICO
        )
        .unwrap()
        .0,
        422
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &path,
            &token,
            &[("Idempotency-Key", "malformed-ico"), ("If-Match", &ico_tag)],
            include_bytes!("fixtures/artwork-malformed.ico")
        )
        .unwrap()
        .0,
        422
    );
    assert_eq!(
        request(
            addr,
            "DELETE",
            &path,
            &token,
            &[("If-Match", &jpeg_tag)],
            &[]
        )
        .unwrap()
        .0,
        412
    );
    assert_eq!(
        request(
            addr,
            "DELETE",
            &path,
            &token,
            &[("If-Match", &ico_tag)],
            &[]
        )
        .unwrap()
        .0,
        204
    );
    assert_eq!(
        request(addr, "GET", &path, &token, &[], &[]).unwrap().0,
        404
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{second_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        PNG
    );
    let (status, body) = request(
        addr,
        "POST",
        "/api/v1/backups",
        &token,
        &[("Idempotency-Key", "artwork-backup")],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let operation: Value = serde_json::from_slice(&body).unwrap();
    let operation_id = operation["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let backup_id = loop {
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
        let state: Value = serde_json::from_slice(&body).unwrap();
        if state["status"] == "Completed" {
            break state["backupId"].as_str().unwrap().to_owned();
        }
        assert_ne!(state["status"], "Failed", "{state}");
        assert!(Instant::now() < deadline, "backup timed out");
        std::thread::sleep(Duration::from_millis(50));
    };
    let backup_dir = dir.path().join("crash/backups").join(&backup_id);
    let manifest: Value =
        serde_json::from_slice(&fs::read(backup_dir.join("manifest.json")).unwrap()).unwrap();
    let png_digest = format!("{:x}", Sha256::digest(PNG));
    assert!(manifest["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|member| member["sha256"] == png_digest));
    let app_path = format!("/api/v1/apps/{second}");
    let (status, _, app_head) = request_full(addr, "GET", &app_path, &token, &[], &[]).unwrap();
    assert_eq!(status, 200);
    let app_tag = response_etag(&app_head);
    let (status, body) = request(
        addr,
        "DELETE",
        &app_path,
        &token,
        &[
            ("Idempotency-Key", "delete-artwork-app"),
            ("If-Match", &app_tag),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
    let deletion: Value = serde_json::from_slice(&body).unwrap();
    let deletion_id = deletion["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, body) = request(
            addr,
            "GET",
            &format!("/api/v1/operations/{deletion_id}"),
            &token,
            &[],
            &[],
        )
        .unwrap();
        let status: Value = serde_json::from_slice(&body).unwrap();
        if status["status"] == "Completed" {
            break;
        }
        assert_ne!(status["status"], "Failed", "{status}");
        assert!(
            Instant::now() < deadline,
            "artwork deletion timed out: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    for bytes in [PNG, JPEG, WEBP] {
        let digest = format!("{:x}", Sha256::digest(bytes));
        assert!(
            !dir.path()
                .join("crash/cas")
                .join(&digest[..2])
                .join(&digest)
                .exists(),
            "unreferenced artwork CAS object remains: {digest}"
        );
    }
    let normalized_digest = ico["digest"].as_str().unwrap();
    assert!(!dir
        .path()
        .join("crash/cas")
        .join(&normalized_digest[..2])
        .join(normalized_digest)
        .exists());

    let logo_path = format!("/api/v1/apps/{app}/artwork/logo");
    let steam_logo_path = format!("/api/v1/apps/{app}/artwork/steam.logo");
    assert_eq!(
        request(
            addr,
            "PUT",
            &logo_path,
            &token,
            &[
                ("Idempotency-Key", "canonical-logo"),
                ("If-None-Match", "*")
            ],
            PNG
        )
        .unwrap()
        .0,
        200
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{steam_logo_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        PNG
    );
    assert_eq!(
        request(
            addr,
            "PUT",
            &steam_logo_path,
            &token,
            &[
                ("Idempotency-Key", "legacy-steam-logo"),
                ("If-None-Match", "*")
            ],
            JPEG
        )
        .unwrap()
        .0,
        200
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{steam_logo_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        JPEG
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{logo_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        PNG
    );
    let (_, artwork_list) = request(
        addr,
        "GET",
        &format!("/api/v1/apps/{app}/artwork"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    let artwork_list: Value = serde_json::from_slice(&artwork_list).unwrap();
    assert_eq!(artwork_list["items"].as_array().unwrap().len(), 2);
    drop(server);
    let restored = tempfile::tempdir().unwrap();
    init_local(restored.path(), "crash", LocalMode::Dev).unwrap();
    app_depot_infrastructure::restore::restore(&backup_dir, &restored.path().join("crash"), true)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let restored_addr = listener.local_addr().unwrap();
    drop(listener);
    let restored_server = start(restored.path(), restored_addr);
    assert_eq!(
        request(
            restored_addr,
            "GET",
            &format!("{second_path}/content"),
            &token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        PNG
    );
    assert_eq!(
        request(restored_addr, "GET", &path, &token, &[], &[])
            .unwrap()
            .0,
        404
    );
    drop(restored_server);
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
    let artwork_path = format!("{app_path}/artwork/steam.hero");
    let artwork_bytes = include_bytes!("fixtures/artwork.png");
    assert_eq!(
        request(
            addr,
            "PUT",
            &artwork_path,
            &token,
            &[
                ("Idempotency-Key", "public-artwork"),
                ("If-None-Match", "*")
            ],
            artwork_bytes
        )
        .unwrap()
        .0,
        200
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{artwork_path}/content"),
            &outsider_token,
            &[],
            &[]
        )
        .unwrap()
        .1,
        artwork_bytes
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{artwork_path}/content"),
            "",
            &[],
            &[]
        )
        .unwrap()
        .1,
        artwork_bytes
    );
    let (_, _, public_artwork_head) =
        request_full(addr, "GET", &artwork_path, &outsider_token, &[], &[]).unwrap();
    let public_artwork_tag = response_etag(&public_artwork_head);
    assert_eq!(
        request(
            addr,
            "PUT",
            &artwork_path,
            &outsider_token,
            &[
                ("Idempotency-Key", "reader-cannot-edit-artwork"),
                ("If-Match", &public_artwork_tag)
            ],
            include_bytes!("fixtures/artwork.jpg")
        )
        .unwrap()
        .0,
        403
    );
    let (_, _, settings_head) = request_full(addr, "GET", settings_path, &token, &[], &[]).unwrap();
    let settings_tag = response_etag(&settings_head);
    assert_eq!(
        request(
            addr,
            "PATCH",
            settings_path,
            &token,
            &[
                ("Content-Type", "application/json"),
                ("If-Match", &settings_tag)
            ],
            br#"{"anonymousReadEnabled":false}"#
        )
        .unwrap()
        .0,
        200
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{artwork_path}/content"),
            "",
            &[],
            &[]
        )
        .unwrap()
        .0,
        401
    );
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("{artwork_path}/content"),
            &outsider_token,
            &[],
            &[]
        )
        .unwrap()
        .0,
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
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/inventory?parent=&pageSize=1"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["items"][0]["path"],
        "a.txt"
    );
    let (status, _) = request(
        addr,
        "GET",
        &format!("/api/v1/artifacts/{artifact}/inventory?parent=../bad"),
        &token,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(status, 400);
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
    let assess = |data: Value| {
        let (status, body) = request(
            addr,
            "POST",
            &format!("/api/v1/associations/{association_id}/entry-points/validate"),
            &token,
            &[("Content-Type", "application/json")],
            json!({"data":data}).to_string().as_bytes(),
        )
        .unwrap();
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice::<Value>(&body).unwrap()
    };
    let entry_data = json!({"other":"preserved","appDepot":{"entryPoints":[
        {"id":"primary","label":"Script","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"]},
        {"id":"secondary","target":{"type":"TreeFile","path":"a.txt"}}
    ]}});
    assert_eq!(assess(entry_data.clone())["status"], "Valid");
    assert_eq!(assess(entry_data.clone())["launchStatus"], "Absent");
    let native = json!({"appDepot":{"entryPoints":[
        {"id":"client","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"","arguments":["--windowed",""]}},
        {"id":"server","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1"}}
    ]}});
    let assessed = assess(native);
    assert_eq!(assessed["status"], "Valid");
    assert_eq!(assessed["launchStatus"], "Valid");
    assert_eq!(assessed["selectionRequiredFor"], json!(["linux/x86_64"]));
    assert_eq!(
        assessed["items"][0]["effectivePlatforms"],
        json!(["linux/x86_64"])
    );
    let release_etag = format!("\"declarations-{release_id}-1\"");
    let (status, _) = request(
        addr,
        "PUT",
        &format!("/api/v1/releases/{release_id}/declarations"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &release_etag),
        ],
        json!({"data":{"appDepot":{"platforms":["linux/aarch64"]}}})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"x64","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1"}}]}})
        )["items"][0]["launchReason"],
        "NoPlatformIntersection"
    );
    let release_etag = format!("\"declarations-{release_id}-2\"");
    let (status, _) = request(
        addr,
        "PUT",
        &format!("/api/v1/releases/{release_id}/declarations"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &release_etag),
        ],
        json!({"data":{}}).to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"bad-dir","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"../outside"}}]}})
        )["items"][0]["launchReason"],
        "MalformedWorkingDirectory"
    );
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"future","target":{"type":"TreeFile","path":"a.txt"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV2"}}]}})
        )["items"][0]["launchStatus"],
        "Unsupported"
    );
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"bad","target":{"type":"TreeFile","path":"../etc/passwd"}}]}})
        )["items"][0]["status"],
        "Invalid"
    );
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"missing","target":{"type":"TreeFile","path":"missing.txt"}}]}})
        )["items"][0]["status"],
        "Missing"
    );
    assert_eq!(
        assess(
            json!({"appDepot":{"entryPoints":[{"id":"wrong-kind","target":{"type":"ArtifactFile"}}]}})
        )["items"][0]["status"],
        "Invalid"
    );
    let (status, _) = request(
        addr,
        "POST",
        &format!("/api/v1/associations/{association_id}/entry-points/validate"),
        "",
        &[("Content-Type", "application/json")],
        json!({"data":entry_data}).to_string().as_bytes(),
    )
    .unwrap();
    assert!(
        status == 401 || status == 404,
        "private validation leaked with status {status}"
    );
    let declaration_etag = format!("\"declarations-{association_id}-1\"");
    let (status, body) = request(
        addr,
        "PUT",
        &format!("/api/v1/associations/{association_id}/declarations"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &declaration_etag),
        ],
        json!({"data":entry_data}).to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["data"]["other"],
        "preserved"
    );
    let (status, _) = request(
        addr,
        "PUT",
        &format!("/api/v1/associations/{association_id}/declarations"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &declaration_etag),
        ],
        json!({"data":null}).to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 412);
    let unresolved = json!({"appDepot":{"entryPoints":[{"id":"future","target":{"type":"TreeFile","path":"missing.txt"},"extension":{"kept":true}}]},"other":"kept","largeInteger":u64::MAX});
    let next_etag = format!("\"declarations-{association_id}-2\"");
    let (status, body) = request(
        addr,
        "PUT",
        &format!("/api/v1/associations/{association_id}/declarations"),
        &token,
        &[
            ("Content-Type", "application/json"),
            ("If-Match", &next_etag),
        ],
        json!({"data":unresolved}).to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["data"]["appDepot"]["entryPoints"][0]
            ["extension"]["kept"],
        true
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["data"]["largeInteger"],
        json!(u64::MAX)
    );
    assert_eq!(assess(unresolved)["items"][0]["status"], "Missing");
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

struct RecipeHttp {
    addr: SocketAddr,
    token: String,
}

impl RecipeHttp {
    fn json(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        input: Option<Value>,
        expected: u16,
    ) -> (Value, String) {
        let mut headers = headers.to_vec();
        headers.push(("Content-Type", "application/json"));
        let bytes = input.map(|v| v.to_string()).unwrap_or_default();
        let (status, body, head) = request_full(
            self.addr,
            method,
            &format!("/api/v1{path}"),
            &self.token,
            &headers,
            bytes.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            status,
            expected,
            "{method} {path}: {}",
            String::from_utf8_lossy(&body)
        );
        (serde_json::from_slice(&body).unwrap(), head)
    }

    fn create(&self, path: &str, body: Value) -> String {
        let key = uuid::Uuid::new_v4().to_string();
        self.json("POST", path, &[("Idempotency-Key", &key)], Some(body), 201)
            .0["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn publish(&self, id: &str, expected: u16) -> Value {
        let path = format!("/releases/{id}");
        let etag = response_etag(&self.json("GET", &path, &[], None, 200).1);
        self.json(
            "POST",
            &format!("{path}/publish"),
            &[
                ("Idempotency-Key", &format!("publish-{id}")),
                ("If-Match", &etag),
            ],
            None,
            expected,
        )
        .0
    }

    fn save_recipe(&self, id: &str, recipe: Value, expected: u16) -> Value {
        let path = format!("/releases/{id}/recipe");
        let etag = response_etag(&self.json("GET", &path, &[], None, 200).1);
        self.json("PUT", &path, &[("If-Match", &etag)], Some(recipe), expected)
            .0
    }

    fn resolve(&self, id: &str, expected: u16) -> Value {
        self.json(
            "GET",
            &format!("/releases/{id}/recipe/lock?platform=linux%2Fx86_64"),
            &[],
            None,
            expected,
        )
        .0
    }

    fn file_bytes(&self, upload: &str, file: &Value, bytes: &[u8]) {
        if file["transferRequired"] != false {
            let file_id = file["fileId"].as_str().unwrap();
            let status = request(
                self.addr,
                "PUT",
                &format!("/api/v1/uploads/{upload}/files/{file_id}"),
                &self.token,
                &[
                    ("Content-Type", "application/octet-stream"),
                    (
                        "Content-Range",
                        &format!("bytes 0-{}/{}", bytes.len() - 1, bytes.len()),
                    ),
                ],
                bytes,
            )
            .unwrap()
            .0;
            assert_eq!(status, 204);
        }
    }

    fn upload_fixture(&self, release: &str, bytes: &[u8], tree: bool) -> Value {
        let mode = if tree { "BareTree" } else { "BareFile" };
        let digest = format!("{:x}", Sha256::digest(bytes));
        let mut input = json!({"mode":mode,"target":{"releaseId":release}});
        if !tree {
            input["file"] = json!({"size":bytes.len(),"sha256":digest});
        }
        let (upload, _) = self.json(
            "POST",
            "/uploads",
            &[("Idempotency-Key", &format!("upload-{release}"))],
            Some(input),
            201,
        );
        let id = upload["id"].as_str().unwrap();
        if tree {
            let inventory = self.json("POST", &format!("/uploads/{id}/inventory"), &[("Idempotency-Key", "recipe-fixture-inventory")], Some(json!({"entries":[
                {"type":"Directory","path":"data"},
                {"type":"Directory","path":"data/empty"},
                {"type":"File","path":"data/resources.dat","size":bytes.len(),"sha256":digest,"executable":false}
            ]})), 200).0;
            self.json(
                "POST",
                &format!("/uploads/{id}/inventory/complete"),
                &[("Idempotency-Key", "recipe-fixture-complete")],
                None,
                200,
            );
            self.file_bytes(id, &inventory["entries"][2], bytes);
        } else {
            self.file_bytes(id, &upload["file"], bytes);
        }
        self.json(
            "POST",
            &format!("/uploads/{id}/finalize"),
            &[("Idempotency-Key", &format!("finalize-{release}"))],
            None,
            200,
        )
        .0
    }

    fn label_artifact(&self, fixture: &Value, label: &str) {
        let association = fixture["associationId"].as_str().unwrap();
        let path = format!("/associations/{association}/declarations");
        let tag = response_etag(&self.json("GET", &path, &[], None, 200).1);
        self.json(
            "PUT",
            &path,
            &[("If-Match", &tag)],
            Some(json!({"data":{"appDepot":{"label":label}}})),
            200,
        );
    }

    fn public(&self, app: &str) {
        let path = format!("/apps/{app}");
        let tag = response_etag(&self.json("GET", &path, &[], None, 200).1);
        self.json(
            "PATCH",
            &path,
            &[("If-Match", &tag)],
            Some(json!({"visibility":"Public"})),
            200,
        );
    }
}

fn exercise_standard_installation(
    owner: &RecipeHttp,
    reader: &RecipeHttp,
    release: &str,
    association: &Value,
) -> (String, Value, Value) {
    let path = format!("/releases/{release}/installation/resolve");
    let doc = format!("/releases/{release}/declarations");
    let install = json!({
        "version":1,
        "payload":[{"associationId":association,"source":"data","destination":"base"}],
        "options":[
            {"id":"language","name":"Game language","language":true,"required":true,"choices":[
                {"id":"en","name":"English","payload":[]},
                {"id":"fr","name":"French","payload":[{"associationId":association,"source":"data/resources.dat","destination":"text/fr.dat"}]}
            ]},
            {"id":"components","name":"Components","multiple":true,"choices":[
                {"id":"bonus","name":"Bonus","payload":[{"associationId":association,"source":"data/resources.dat","destination":"bonus.dat"}]}
            ]}
        ],
        "prerequisites":[
            {"id":"first","name":"First","associationId":association,"path":"data/resources.dat","platforms":["windows/x86_64"],
             "evidence":"synthetic fixture","arguments":["one argument"]},
            {"id":"second","name":"Second","associationId":association,"path":"data/resources.dat","platforms":["windows/x86_64"],
             "evidence":"synthetic fixture","optional":true,"when":{"optionId":"components","choiceId":"bonus"},"successExitCodes":[0,3]}
        ],
        "unresolved":["Unknown original installer action"]
    });
    let previous = response_etag(&owner.json("GET", &doc, &[], None, 200).1);
    for (field, value, code) in [
        ("destination", json!("../outside"), "installation_invalid"),
        (
            "associationId",
            json!("00000000-0000-0000-0000-000000000001"),
            "installation_source",
        ),
    ] {
        let mut invalid = install.clone();
        invalid["payload"][0][field] = value;
        let problem = owner
            .json(
                "PUT",
                &doc,
                &[("If-Match", &previous)],
                Some(json!({"data":{"appDepot":{"installation":invalid}}})),
                422,
            )
            .0;
        assert_eq!(problem["code"], code);
    }
    let data = json!({"data":{"appDepot":{"installation":install},"exampleExtension":true}});
    let saved = owner.json(
        "PUT",
        &doc,
        &[("If-Match", &previous)],
        Some(data.clone()),
        200,
    );
    owner.json("PUT", &doc, &[("If-Match", &previous)], Some(data), 412);
    let missing = reader
        .json(
            "POST",
            &path,
            &[],
            Some(json!({"platform":"windows/x86_64"})),
            422,
        )
        .0;
    assert_eq!(missing["code"], "installation_selection");
    let choices = json!({"language":["fr"],"components":["bonus"]});
    let request = json!({"platform":"windows/x86_64","revisionId":saved.0["revisionId"],"selections":choices});
    let plan = reader
        .json("POST", &path, &[], Some(request.clone()), 200)
        .0;
    assert_eq!(plan["revisionId"], saved.0["revisionId"]);
    assert_eq!(plan["directories"], json!(["base", "base/empty", "text"]));
    assert_eq!(plan["files"].as_object().unwrap().len(), 3);
    assert_eq!(
        plan["prerequisites"][0]["arguments"],
        json!(["one argument"])
    );
    assert_eq!(plan["prerequisites"][1]["id"], "second");
    assert_eq!(plan["prerequisites"][1]["successExitCodes"], json!([0, 3]));
    assert_eq!(
        plan["unresolved"],
        json!(["Unknown original installer action"])
    );
    let mut collision = install.clone();
    collision["payload"].as_array_mut().unwrap().push(json!({"associationId":association,"source":"data/resources.dat","destination":"base/resources.dat"}));
    let preview = json!({"platform":"windows/x86_64","manifest":collision,"selections":choices});
    reader.json("POST", &path, &[], Some(preview.clone()), 404);
    assert_eq!(
        owner.json("POST", &path, &[], Some(preview), 422).0["code"],
        "installation_collision"
    );

    let mut single = install;
    single["options"][0]["choices"] = json!([{"id":"en","name":"English","payload":[]}]);
    single["options"][0]["defaults"] = json!(["en"]);
    owner.json(
        "PUT",
        &doc,
        &[("If-Match", &response_etag(&saved.1))],
        Some(json!({"data":{"appDepot":{"installation":single}}})),
        200,
    );
    let default_plan = reader
        .json(
            "POST",
            &path,
            &[],
            Some(json!({"platform":"windows/x86_64"})),
            200,
        )
        .0;
    assert_eq!(default_plan["selections"]["language"], json!(["en"]));
    assert_eq!(default_plan["prerequisites"].as_array().unwrap().len(), 1);
    assert_eq!(
        reader
            .json("POST", &path, &[], Some(request.clone()), 200)
            .0,
        plan
    );
    (path, request, plan)
}

#[tokio::test]
async fn derived_recipe_lifecycle_resolves_synthetic_artifacts_and_preserves_permissions() {
    let dir = tempfile::tempdir().unwrap();
    init_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let lock = lock_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let pool = open_catalog(&lock).await.unwrap();
    let setup = auth::prepare_setup(&pool).await.unwrap().unwrap();
    let password = "fixture secret phrase 12345";
    assert!(auth::complete_setup(&pool, &setup, "admin", password)
        .await
        .unwrap());
    let admin = auth::verify_password(&pool, "admin", password)
        .await
        .unwrap()
        .unwrap();
    let reader = auth::create_user(&pool, &admin, "test-reader", "reader", password, false)
        .await
        .unwrap();
    let (admin_token, _) = auth::issue_token(&pool, &admin.id).await.unwrap();
    let (reader_token, _) = auth::issue_token(&pool, &reader.id).await.unwrap();
    sqlx::query("UPDATE server_settings SET anonymous_read_enabled=1 WHERE id=1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    drop(lock);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let server = start(dir.path(), addr);
    let owner = RecipeHttp {
        addr,
        token: admin_token,
    };
    let reader = RecipeHttp {
        addr,
        token: reader_token,
    };
    let anonymous = RecipeHttp {
        addr,
        token: String::new(),
    };

    let game = owner.create("/apps", json!({"name":"Synthetic Windows game"}));
    assert_eq!(
        owner
            .json("GET", &format!("/apps/{game}"), &[], None, 200)
            .0["kind"],
        "standard"
    );
    let game_release = owner.create(
        &format!("/apps/{game}/releases"),
        json!({"version":"game-data"}),
    );
    let game_content = owner.upload_fixture(&game_release, b"synthetic game assets", true);
    owner.publish(&game_release, 200);
    owner.public(&game);
    let (install_path, install_request, install_plan) = exercise_standard_installation(
        &owner,
        &reader,
        &game_release,
        &game_content["associationId"],
    );

    let port = owner.create(
        "/apps",
        json!({"name":"Synthetic native source port","kind":"standard"}),
    );
    let first = owner.create(
        &format!("/apps/{port}/releases"),
        json!({"version":"port-1"}),
    );
    let second = owner.create(
        &format!("/apps/{port}/releases"),
        json!({"version":"port-2"}),
    );
    let first_content = owner.upload_fixture(&first, b"first native fixture", false);
    owner.label_artifact(&first_content, "Native executable");
    let file_association = &first_content["associationId"];
    let launch_path = format!(
        "/associations/{}/declarations",
        file_association.as_str().unwrap()
    );
    let launch_tag = response_etag(&owner.json("GET", &launch_path, &[], None, 200).1);
    let launch = owner.json("PUT",&launch_path,&[("If-Match",&launch_tag)],Some(json!({"data":{"appDepot":{
        "label":"Native executable",
        "entryPoints":[{"id":"start","platforms":["linux/x86_64"],"target":{"type":"ArtifactFile"},
            "launch":{"method":"NativePortableV1","arguments":["--test"]}}]
    }}})),200).0;

    let declaration_path = format!("/releases/{first}/declarations");
    let etag = response_etag(&owner.json("GET", &declaration_path, &[], None, 200).1);
    owner.json(
        "PUT",
        &declaration_path,
        &[("If-Match", &etag)],
        Some(
            json!({"data":{"appDepot":{"installation":{"version":1,"payload":[
                {"associationId":file_association,"destination":"bin/port"}
            ],"launch":{"associationId":file_association,"revisionId":launch["revisionId"],
                "entryPointId":"start","destination":"bin/port"}}}}}),
        ),
        200,
    );
    let install_endpoint = format!("/releases/{first}/installation/resolve");
    let file_install_request = json!({"platform":"linux/x86_64"});
    let plan = owner
        .json(
            "POST",
            &install_endpoint,
            &[],
            Some(file_install_request.clone()),
            200,
        )
        .0;
    assert_eq!(
        plan["files"]["bin/port"]["artifactId"],
        first_content["artifactId"]
    );
    assert_eq!(plan["files"]["bin/port"]["fileId"], Value::Null);
    assert_eq!(plan["files"]["bin/port"]["executable"], true);
    assert_eq!(plan["launch"]["path"], "bin/port");
    assert_eq!(plan["launch"]["workingDirectory"], "bin");
    assert_eq!(plan["launch"]["arguments"], json!(["--test"]));
    reader.json(
        "POST",
        &install_endpoint,
        &[],
        Some(file_install_request),
        404,
    );

    owner.publish(&first, 200);
    let channel = format!("/apps/{port}/channels/stable");
    let channel_etag = response_etag(
        &owner
            .json(
                "PUT",
                &channel,
                &[("If-None-Match", "*")],
                Some(json!({"releaseId":first})),
                201,
            )
            .1,
    );

    let derived = owner.create(
        "/apps",
        json!({"name":"Synthetic native game edition","kind":"derived"}),
    );
    assert_eq!(
        owner
            .json("GET", &format!("/apps/{derived}"), &[], None, 200)
            .0["kind"],
        "derived"
    );
    let release = owner.create(
        &format!("/apps/{derived}/releases"),
        json!({"version":"edition-1"}),
    );
    assert_eq!(owner.publish(&release, 422)["code"], "recipe_required");
    let root = format!("/releases/{release}/recipe");
    let before = owner.json("GET", &root, &[], None, 200);
    assert!(before.0.is_null());
    let original_etag = response_etag(&before.1);
    let recipe = json!({
        "version":1,"dependencies":{
            "base":{"appId":game,"selector":{"type":"release","releaseId":game_release}},
            "port":{"appId":port,"selector":{"type":"channel","name":"stable"}}
        },"targets":{"linux/x86_64":{
            "operations":[{"dependency":"base","source":"data","destination":"assets"},
                {"dependency":"port","associationLabel":"Native executable","destination":"bin/game","executable":true}],
            "launch":{"path":"bin/game","workingDirectory":"assets","arguments":["--literal","a b"]}
        }}
    });
    let mut invalid = recipe.clone();
    invalid["version"] = json!(2);
    assert_eq!(
        owner.save_recipe(&release, invalid, 422)["code"],
        "unsupported_recipe_version"
    );
    let mut invalid = recipe.clone();
    invalid["dependencies"]["base"]["appId"] = json!("synthetic-windows-game");
    assert_eq!(
        owner.save_recipe(&release, invalid, 422)["code"],
        "invalid_recipe"
    );
    let mut invalid = recipe.clone();
    invalid["targets"]["linux/x86_64"]["operations"][0]["destination"] = json!("data/../escape");
    assert_eq!(
        owner.save_recipe(&release, invalid, 422)["code"],
        "invalid_recipe"
    );
    let mut missing = recipe.clone();
    missing["dependencies"]["base"]["selector"]["releaseId"] =
        json!(uuid::Uuid::new_v4().to_string());
    owner.save_recipe(&release, missing, 200);
    assert_eq!(
        owner.publish(&release, 422)["code"],
        "dependency_unavailable"
    );
    let mut mismatch = recipe.clone();
    mismatch["dependencies"]["port"]["platforms"] = json!(["windows/x86_64"]);
    owner.save_recipe(&release, mismatch, 200);
    assert_eq!(owner.resolve(&release, 422)["code"], "platform_mismatch");

    let mut conflict = recipe.clone();
    conflict["targets"]["linux/x86_64"]["operations"].as_array_mut().unwrap()
        .push(json!({"dependency":"base","source":"data/resources.dat","destination":"bin/game","executable":true}));
    owner.save_recipe(&release, conflict.clone(), 200);
    assert_eq!(owner.resolve(&release, 422)["code"], "recipe_collision");
    conflict["targets"]["linux/x86_64"]["operations"][2]["collision"] = json!("replaceFile");
    owner.save_recipe(&release, conflict, 200);
    assert_eq!(
        owner.resolve(&release, 200)["files"]["bin/game"]["artifactId"],
        game_content["artifactId"]
    );

    owner.save_recipe(&release, recipe.clone(), 200);
    assert_eq!(
        owner
            .json(
                "PUT",
                &root,
                &[("If-Match", &original_etag)],
                Some(recipe.clone()),
                412
            )
            .0["code"],
        "stale_etag"
    );
    owner.publish(&release, 200);
    let document = format!("/releases/{release}/declarations");
    let etag = response_etag(&owner.json("GET", &document, &[], None, 200).1);
    owner.json(
        "PUT",
        &document,
        &[("If-Match", &etag)],
        Some(json!({"data":{"appDepot":{"installation":{"version":1,"recipePayload":true}}}})),
        200,
    );
    let installation = owner
        .json(
            "POST",
            &format!("/releases/{release}/installation/resolve"),
            &[],
            Some(json!({"platform":"linux/x86_64"})),
            200,
        )
        .0;
    assert_eq!(installation["recipe"], owner.resolve(&release, 200));

    owner.public(&derived);
    assert_eq!(
        owner.save_recipe(&release, recipe.clone(), 409)["code"],
        "immutable_release"
    );
    let stored = owner.json("GET", &root, &[], None, 200).0;
    let early = owner.resolve(&release, 200);
    assert_eq!(early, owner.resolve(&release, 200));
    assert_eq!(early["releases"][&release]["dependencies"]["port"], first);
    assert_eq!(
        early["directories"],
        json!(["assets", "assets/empty", "bin"])
    );
    assert_eq!(
        early["files"]["assets/resources.dat"]["associationId"],
        game_content["associationId"]
    );
    assert_eq!(early["launch"]["arguments"], json!(["--literal", "a b"]));
    assert_eq!(
        reader.resolve(&release, 422)["code"],
        "dependency_unavailable"
    );
    assert_eq!(
        reader.json("GET", &root, &[], None, 422).0["code"],
        "dependency_unavailable"
    );
    owner.public(&port);
    assert_eq!(anonymous.resolve(&release, 200), early);

    let second_content = owner.upload_fixture(&second, b"second native fixture", false);
    owner.label_artifact(&second_content, "Native executable");
    owner.publish(&second, 200);
    owner.json(
        "PUT",
        &channel,
        &[("If-Match", &channel_etag)],
        Some(json!({"releaseId":second})),
        200,
    );
    let moved = owner.resolve(&release, 200);
    assert_ne!(
        early["files"]["bin/game"]["sha256"],
        moved["files"]["bin/game"]["sha256"]
    );
    assert_eq!(
        moved["files"]["bin/game"]["artifactId"],
        second_content["artifactId"]
    );
    assert_eq!(owner.json("GET", &root, &[], None, 200).0, stored);
    let old = early["files"]["bin/game"]["artifactId"].as_str().unwrap();
    assert_eq!(
        request(
            addr,
            "GET",
            &format!("/api/v1/artifacts/{old}/content"),
            "",
            &[],
            &[]
        )
        .unwrap()
        .1,
        b"first native fixture"
    );

    let wrapper = owner.create(
        "/apps",
        json!({"name":"Synthetic assembled wrapper","kind":"derived"}),
    );
    let top = owner.create(&format!("/apps/{wrapper}/releases"), json!({}));
    let transitive = json!({"version":1,"dependencies":{
        "child":{"appId":derived,"selector":{"type":"latest"}}
    },"targets":{"linux/x86_64":{
        "operations":[{"dependency":"child","destination":"edition"}],
        "launch":{"path":"edition/bin/game"}
    }}});
    owner.save_recipe(&top, transitive, 200);
    owner.publish(&top, 200);
    let wrapped = owner.resolve(&top, 200);
    assert_eq!(wrapped["releases"][&top]["dependencies"]["child"], release);
    assert_eq!(
        wrapped["files"]["edition/bin/game"],
        moved["files"]["bin/game"]
    );

    let cycle = owner.create(&format!("/apps/{derived}/releases"), json!({}));
    let mut cyclic = recipe.clone();
    cyclic["dependencies"]["port"] =
        json!({"appId":wrapper,"selector":{"type":"release","releaseId":top}});
    cyclic["targets"]["linux/x86_64"]["operations"][1]
        .as_object_mut()
        .unwrap()
        .remove("associationLabel");
    owner.save_recipe(&cycle, cyclic, 200);
    assert_eq!(owner.publish(&cycle, 422)["code"], "dependency_cycle");

    let latest = owner.create(
        &format!("/apps/{derived}/releases"),
        json!({"version":"edition-latest"}),
    );
    let mut latest_recipe = recipe.clone();
    latest_recipe["dependencies"]["port"]["selector"] = json!({"type":"latest"});
    owner.save_recipe(&latest, latest_recipe.clone(), 200);
    assert_eq!(
        owner.resolve(&latest, 200)["releases"][&latest]["dependencies"]["port"],
        second
    );
    latest_recipe["dependencies"]["other-port"] =
        json!({"appId":port,"selector":{"type":"release","releaseId":first}});
    latest_recipe["targets"]["linux/x86_64"]["operations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"dependency":"other-port","destination":"old-port"}));
    owner.save_recipe(&latest, latest_recipe, 200);
    assert_eq!(owner.resolve(&latest, 422)["code"], "dependency_conflict");

    let multiple = owner.create(
        &format!("/apps/{port}/releases"),
        json!({"version":"port-multiple"}),
    );
    let extra = owner.upload_fixture(&multiple, b"distinct native executable fixture", false);
    owner.label_artifact(&extra, "Native executable");
    let alternate = owner
        .json(
            "POST",
            &format!("/releases/{multiple}/artifacts"),
            &[("Idempotency-Key", "recipe-multiple-associations")],
            Some(json!({"artifactId":game_content["artifactId"]})),
            201,
        )
        .0;
    owner.label_artifact(
        &json!({"associationId":alternate["id"]}),
        "Native executable",
    );
    owner.publish(&multiple, 200);

    let mut ambiguous = recipe.clone();
    ambiguous["dependencies"]["port"]["selector"] = json!({"type":"latest"});
    owner.save_recipe(&latest, ambiguous.clone(), 200);
    assert_eq!(owner.resolve(&latest, 422)["code"], "recipe_association");
    let op = &mut ambiguous["targets"]["linux/x86_64"]["operations"][1];
    op.as_object_mut().unwrap().remove("associationLabel");
    op["associationId"] = extra["associationId"].clone();
    owner.save_recipe(&latest, ambiguous, 200);
    assert_eq!(
        owner.resolve(&latest, 200)["files"]["bin/game"]["artifactId"],
        extra["artifactId"]
    );

    drop(server);
    let _restarted = start(dir.path(), addr);
    assert_eq!(
        reader
            .json("POST", &install_path, &[], Some(install_request), 200)
            .0,
        install_plan
    );
    assert_eq!(owner.resolve(&top, 200), wrapped);
    assert_eq!(owner.json("GET", &root, &[], None, 200).0, stored);
    let second_path = format!("/releases/{second}");
    let second_etag = response_etag(&owner.json("GET", &second_path, &[], None, 200).1);
    owner.json(
        "DELETE",
        &second_path,
        &[
            ("If-Match", &second_etag),
            ("Idempotency-Key", "delete-recipe-dependency"),
        ],
        None,
        202,
    );
    assert_eq!(
        owner.resolve(&release, 422)["code"],
        "dependency_unavailable"
    );
    assert_eq!(owner.json("GET", &root, &[], None, 200).0, stored);
}

fn installer_stream(
    addr: SocketAddr,
    token: &str,
    key: &str,
    body: &Value,
) -> impl Iterator<Item = Value> {
    use std::io::{BufRead, BufReader};

    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let body = body.to_string();
    write!(stream, "POST /api/v1/server-imports HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut lines = BufReader::new(stream).lines();
    assert!(lines.next().unwrap().unwrap().starts_with("HTTP/1.1 200"));
    lines.map_while(Result::ok).filter_map(|line| {
        line.strip_prefix("data: ")
            .map(|data| serde_json::from_str(data).unwrap())
    })
}

#[tokio::test]
#[ignore = "requires innoextract 1.9, Linux Landlock and GOG_TEST_FIXTURES"]
async fn gog_import_recovers_from_interruption_rejects_changed_source_and_commits_manifest() {
    let fixtures = std::path::PathBuf::from(std::env::var_os("GOG_TEST_FIXTURES").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let managed = init_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let lock = lock_local(dir.path(), "crash", LocalMode::Dev).unwrap();
    let pool = open_catalog(&lock).await.unwrap();
    let setup = auth::prepare_setup(&pool).await.unwrap().unwrap();
    let password = "disposable installer test password";
    assert!(auth::complete_setup(&pool, &setup, "admin", password)
        .await
        .unwrap());
    let admin = auth::verify_password(&pool, "admin", password)
        .await
        .unwrap()
        .unwrap();
    let (token, _) = auth::issue_token(&pool, &admin.id).await.unwrap();
    pool.close().await;
    drop(lock);

    let source = managed.parent().unwrap().join("inbox/install");
    fs::create_dir_all(&source).unwrap();
    for entry in fs::read_dir(fixtures.join("multi")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), source.join(entry.file_name())).unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let server = start(dir.path(), addr);
    let http = RecipeHttp { addr, token };
    let analysis_path = "/server-imports/gog/analysis";
    let body = json!({"rootId":"inbox","path":"install"});
    fs::copy(source.join("spanning.exe"), source.join("other.exe")).unwrap();
    let ambiguous = http
        .json("POST", analysis_path, &[], Some(body.clone()), 200)
        .0;
    assert_eq!(
        ambiguous["candidates"],
        json!(["other.exe", "spanning.exe"])
    );
    assert!(ambiguous["analysisId"].is_null());
    fs::remove_file(source.join("other.exe")).unwrap();
    let report = http.json("POST", analysis_path, &[], Some(body), 200).0;
    assert_eq!(report["analysis"]["volumes"].as_array().unwrap().len(), 3);
    let response = http.create("/apps", json!({"name":"Synthetic import target"}));
    let release = http.create(
        &format!("/apps/{response}/releases"),
        json!({"version":"1"}),
    );
    let input = json!({"rootId":"inbox","path":"install","mode":"GogOfflineInstaller",
        "target":{"releaseId":release},"gogReview":{"setup":"spanning.exe","analysisId":report["analysisId"],"hints":{"gameVersion":"fixture-1"}}});

    // Trailing bytes leave this parser's setup table intact but change the reviewed digest.
    let setup = source.join("spanning.exe");
    fs::OpenOptions::new()
        .append(true)
        .open(&setup)
        .unwrap()
        .write_all(b"changed after review")
        .unwrap();
    let failure = installer_stream(addr, &http.token, "changed-installer", &input)
        .last()
        .unwrap();
    assert_eq!(failure["problem"]["code"], "source_changed");
    fs::copy(fixtures.join("multi/spanning.exe"), setup).unwrap();

    let mut cancelled = installer_stream(addr, &http.token, "cancel-installer", &input);
    assert!(cancelled.any(|event| event["phase"] == "ExtractingInstaller"));
    http.json(
        "DELETE",
        "/server-imports",
        &[],
        Some(json!({"requestKey":"cancel-installer"})),
        202,
    );
    let terminal = cancelled.last().unwrap();
    assert_eq!(terminal["problem"]["code"], "session_cancelled");
    assert_eq!(fs::read_dir(managed.join("gog-work")).unwrap().count(), 0);

    let mut interrupted = installer_stream(addr, &http.token, "restart-installer", &input);
    assert!(interrupted.any(|event| event["phase"] == "ExtractingInstaller"));
    drop(server);
    drop(interrupted);
    let _restarted = start(dir.path(), addr);
    let result = installer_stream(addr, &http.token, "restart-installer", &input)
        .last()
        .unwrap();
    assert_eq!(result["phase"], "Completed", "{result}");
    let saved = result["result"].clone();
    let association = saved["associationId"].as_str().unwrap();
    let manifest = http
        .json(
            "GET",
            &format!("/associations/{association}/declarations"),
            &[],
            None,
            200,
        )
        .0;
    assert_eq!(
        manifest["data"]["appDepot"]["gogOfflineInstaller"]["user"]["gameVersion"],
        "fixture-1"
    );
    assert_eq!(
        http.json("GET", &format!("/releases/{release}"), &[], None, 200)
            .0["state"],
        "Draft"
    );
    let replay = installer_stream(addr, &http.token, "restart-installer", &input)
        .last()
        .unwrap();
    assert_eq!(replay["result"], saved);
    for entry in fs::read_dir(fixtures.join("multi")).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            fs::read(source.join(entry.file_name())).unwrap(),
            fs::read(entry.path()).unwrap()
        );
    }

    assert!(manifest["data"]["appDepot"].get("entryPoints").is_none());
    let launch_source = source.with_file_name("launch");
    fs::create_dir(&launch_source).unwrap();
    fs::copy(
        fixtures.join("launch/setup.exe"),
        launch_source.join("setup.exe"),
    )
    .unwrap();
    let launch_report = http
        .json(
            "POST",
            analysis_path,
            &[],
            Some(json!({"rootId":"inbox","path":"launch"})),
            200,
        )
        .0;
    let candidate = launch_report["analysis"]["candidateExecutables"][0]
        .as_str()
        .unwrap();
    let working_directory = candidate
        .rsplit_once('/')
        .map_or("docs".into(), |(parent, _)| format!("{parent}/docs"));
    let mut launch_input = json!({"rootId":"inbox","path":"launch","mode":"GogOfflineInstaller",
        "target":{"releaseId":release},"gogReview":{"setup":"setup.exe","analysisId":launch_report["analysisId"],
        "hints":{"candidateExecutable":"missing.exe"},
        "entryPoint":{"platform":"windows/x86_64","workingDirectory":working_directory,"arguments":["--windowed mode",""]}}});
    let result = installer_stream(addr, &http.token, "missing-launch-file", &launch_input)
        .last()
        .unwrap();
    assert_eq!(result["problem"]["code"], "invalid_gog_launch");
    launch_input["gogReview"]["hints"]["candidateExecutable"] = json!(candidate);

    let mut unsafe_directory = launch_input.clone();
    unsafe_directory["gogReview"]["entryPoint"]["workingDirectory"] = json!("../outside");
    let result = installer_stream(
        addr,
        &http.token,
        "unsafe-launch-directory",
        &unsafe_directory,
    )
    .last()
    .unwrap();
    assert_eq!(result["problem"]["code"], "invalid_gog_launch");

    let declarations = format!("/releases/{release}/declarations");
    let set_platforms = |platform: &str| {
        let etag = response_etag(&http.json("GET", &declarations, &[], None, 200).1);
        http.json(
            "PUT",
            &declarations,
            &[("If-Match", &etag)],
            Some(json!({
                "data":{"appDepot":{"platforms":[platform]}}
            })),
            200,
        );
    };
    set_platforms("linux/x86_64");
    let result = installer_stream(addr, &http.token, "invalid-launch-platform", &launch_input)
        .last()
        .unwrap();
    assert_eq!(
        result["result"]["problem"]["code"], "gog_launch_platform",
        "{result}"
    );
    let artifacts = format!("/releases/{release}/artifacts");
    assert_eq!(
        http.json("GET", &artifacts, &[], None, 200).0["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    set_platforms("windows/x86_64");
    let result = installer_stream(addr, &http.token, "valid-launch", &launch_input)
        .last()
        .unwrap();
    assert_eq!(result["phase"], "Completed", "{result}");
    let launch_association = result["result"]["associationId"].as_str().unwrap();
    let launch_document = http
        .json(
            "GET",
            &format!("/associations/{launch_association}/declarations"),
            &[],
            None,
            200,
        )
        .0;
    let entry = &launch_document["data"]["appDepot"]["entryPoints"][0];
    assert_eq!(entry["id"], "gog-launch");
    assert_eq!(entry["platforms"], json!(["windows/x86_64"]));
    assert_eq!(entry["target"], json!({"type":"TreeFile","path":candidate}));
    assert_eq!(
        entry["launch"],
        json!({"method":"NativePortableV1","workingDirectory":working_directory,"arguments":["--windowed mode",""]})
    );
    let check = http
        .json(
            "POST",
            &format!("/associations/{launch_association}/entry-points/validate"),
            &[],
            Some(json!({"data":launch_document["data"]})),
            200,
        )
        .0;
    assert_eq!(check["launchStatus"], "Valid", "{check}");

    let candidate_release = http.create(
        &format!("/apps/{response}/releases"),
        json!({"version":"candidate-only"}),
    );
    launch_input["target"]["releaseId"] = json!(candidate_release);
    launch_input["gogReview"]
        .as_object_mut()
        .unwrap()
        .remove("entryPoint");
    let result = installer_stream(addr, &http.token, "candidate-only", &launch_input)
        .last()
        .unwrap();
    assert_eq!(result["phase"], "Completed", "{result}");
    assert_eq!(
        result["result"]["artifactId"],
        http.json(
            "GET",
            &format!("/associations/{launch_association}"),
            &[],
            None,
            200
        )
        .0["artifactId"]
    );
    let association = result["result"]["associationId"].as_str().unwrap();
    let candidate_document = http
        .json(
            "GET",
            &format!("/associations/{association}/declarations"),
            &[],
            None,
            200,
        )
        .0;
    let block = &candidate_document["data"]["appDepot"];
    assert!(block.get("entryPoints").is_none());
    assert_eq!(
        block["gogOfflineInstaller"]["user"]["candidateExecutable"],
        candidate
    );
    let seed = http
        .json(
            "GET",
            &format!("/releases/{candidate_release}/declarations"),
            &[],
            None,
            200,
        )
        .0;
    assert_eq!(
        seed["data"]["appDepot"]["installation"]["payload"][0]["associationId"],
        association
    );
    assert!(seed["data"]["appDepot"]["installation"]
        .get("prerequisites")
        .is_none());
    let plan = http
        .json(
            "POST",
            &format!("/releases/{candidate_release}/installation/resolve"),
            &[],
            Some(json!({"platform":"windows/x86_64"})),
            200,
        )
        .0;
    assert!(plan["prerequisites"].as_array().unwrap().is_empty());
    assert_eq!(plan["selections"], json!({}));
    assert_eq!(plan["launch"], Value::Null);
}
