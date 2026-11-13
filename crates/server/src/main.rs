use app_depot_infrastructure::{
    init_local, lock_local, lock_production, open_catalog, reset_dev, seed_dev, LocalMode,
};
use axum::{
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tower_http::services::{ServeDir, ServeFile};

mod auth;
mod backups;
mod catalog;
mod channels;
mod config;
mod declarations;
mod diagnostics;
mod download;
mod upload;

fn usage() -> ! {
    eprintln!("usage: app-depot-server <init-dev|init-preview|reset-dev|seed-dev|serve> --namespace ABSOLUTE_DIRECTORY --instance NAME [--mode dev|preview] [--bind ADDR] [--assets DIRECTORY] [--bootstrap-secret-file FILE] [--session-key-file FILE] [--upload-idle-seconds SECONDS]\n       app-depot-server serve --mode production [--config JSON_FILE] (APP_DEPOT_DATA_ROOT and secret sources required)\n       app-depot-server restore --backup-dir DIRECTORY --data-root ABSOLUTE_DIRECTORY [--replace]\n       app-depot-server diagnose --data-root ABSOLUTE_DIRECTORY");
    std::process::exit(2)
}

fn argument(args: &[String], key: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].clone())
}

fn location(args: &[String]) -> (PathBuf, String) {
    let namespace = PathBuf::from(argument(args, "--namespace").unwrap_or_else(|| usage()));
    if !namespace.is_absolute() {
        usage();
    }
    let instance = argument(args, "--instance").unwrap_or_else(|| usage());
    (namespace, instance)
}

fn validate_production_args(args: &[String]) -> Result<(), &'static str> {
    if args.len() < 3 || (args.len() - 1) % 2 != 0 {
        return Err("invalid Production arguments");
    }
    let mut mode = false;
    let mut config = false;
    for pair in args[1..].chunks_exact(2) {
        if pair[1].starts_with("--") {
            return Err("missing Production option value");
        }
        match pair[0].as_str() {
            "--mode" if !mode && pair[1] == "production" => mode = true,
            "--config" if !config => config = true,
            _ => return Err("unknown or duplicate Production option"),
        }
    }
    if !mode {
        return Err("Production mode required");
    }
    Ok(())
}

async fn live() -> Json<Value> {
    Json(json!({"status": "Live"}))
}
async fn ready(State(flag): State<Arc<AtomicBool>>) -> Response {
    if flag.load(Ordering::Acquire) {
        (StatusCode::OK, Json(json!({"status": "Ready"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "NotReady"})),
        )
            .into_response()
    }
}

async fn gate(
    State(flag): State<Arc<AtomicBool>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if flag.load(Ordering::Acquire) {
        next.run(request).await
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, [(axum::http::header::CONTENT_TYPE, "application/problem+json")], Json(json!({"type":"urn:app-depot:problem:retryable_failure","title":"Not ready","status":503,"code":"retryable_failure"}))).into_response()
    }
}

async fn boundary_gate(
    State(lock): State<Arc<tokio::sync::RwLock<()>>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let _guard = lock.read().await;
    next.run(request).await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or_else(|| usage());
    if command == "restore" {
        let backup = PathBuf::from(argument(&args, "--backup-dir").unwrap_or_else(|| usage()));
        let root = PathBuf::from(argument(&args, "--data-root").unwrap_or_else(|| usage()));
        app_depot_infrastructure::restore::restore(
            &backup,
            &root,
            args.iter().any(|arg| arg == "--replace"),
        )
        .await?;
        println!("{{\"status\":\"Restored\"}}");
        return Ok(());
    }
    if command == "diagnose" {
        let root = PathBuf::from(argument(&args, "--data-root").unwrap_or_else(|| usage()));
        let diagnostic = app_depot_infrastructure::restore::diagnose(&root).await?;
        println!("{diagnostic}");
        if diagnostic["status"] != "Healthy" {
            std::process::exit(1);
        }
        return Ok(());
    }
    let production =
        command == "serve" && argument(&args, "--mode").as_deref() == Some("production");
    if production {
        validate_production_args(&args)?;
    }
    let (namespace, instance) = if production {
        (PathBuf::new(), String::new())
    } else {
        location(&args)
    };
    match command {
        "init-dev" => {
            println!(
                "{}",
                init_local(&namespace, &instance, LocalMode::Dev)?.display()
            );
        }
        "init-preview" => {
            println!(
                "{}",
                init_local(&namespace, &instance, LocalMode::Preview)?.display()
            );
        }
        "reset-dev" => {
            reset_dev(&namespace, &instance)?;
        }
        "seed-dev" => {
            let lock = lock_local(&namespace, &instance, LocalMode::Dev)?;
            let pool = open_catalog(&lock).await?;
            seed_dev(&pool).await?;
            pool.close().await;
        }
        "serve" => {
            let production_config = if production {
                Some(config::production(
                    argument(&args, "--config")
                        .as_deref()
                        .map(std::path::Path::new),
                )?)
            } else {
                None
            };
            let mode = match argument(&args, "--mode").as_deref() {
                Some("dev") => LocalMode::Dev,
                Some("preview") => LocalMode::Preview,
                Some("production") => LocalMode::Production,
                _ => usage(),
            };
            let lock = if let Some(config) = &production_config {
                lock_production(&config.data_root)?
            } else {
                lock_local(&namespace, &instance, mode)?
            };
            let pool = open_catalog(&lock).await?;
            let auth_state = if let Some(config) = &production_config {
                match &config.bootstrap_secret {
                    Some(config::SecretSource::File(path)) => {
                        app_depot_infrastructure::auth::bootstrap_admin(&pool, Some(path)).await?
                    }
                    Some(config::SecretSource::Environment(value)) => {
                        app_depot_infrastructure::auth::bootstrap_admin_value(&pool, Some(value))
                            .await?
                    }
                    None => {
                        app_depot_infrastructure::auth::bootstrap_admin_value(&pool, None).await?
                    }
                }
                match &config.session_key {
                    config::SecretSource::File(path) => auth::AuthState::new(pool.clone(), path)?,
                    config::SecretSource::Environment(value) => {
                        auth::AuthState::from_key(pool.clone(), value.as_bytes().to_vec())?
                    }
                }
            } else {
                let bootstrap = argument(&args, "--bootstrap-secret-file").map(PathBuf::from);
                app_depot_infrastructure::auth::bootstrap_admin(&pool, bootstrap.as_deref())
                    .await?;
                let session_key = PathBuf::from(
                    argument(&args, "--session-key-file")
                        .ok_or("--session-key-file is required")?,
                );
                auth::AuthState::new(pool.clone(), &session_key)?
            };
            let coordinator = Arc::new(app_depot_application::upload::UploadCoordinator::new(
                app_depot_infrastructure::upload::UploadAdapter::new(
                    pool.clone(),
                    lock.path.clone(),
                ),
            ));
            let idle_seconds: i64 = if let Some(config) = &production_config {
                config.upload_idle_seconds
            } else {
                argument(&args, "--upload-idle-seconds")
                    .unwrap_or_else(|| "86400".into())
                    .parse()?
            };
            if idle_seconds < 1 {
                return Err("--upload-idle-seconds must be positive".into());
            }
            let upload_state = upload::UploadApiState {
                auth: auth_state.clone(),
                coordinator: coordinator.clone(),
                root: Arc::new(lock.path.clone()),
            };
            let download_state = download::DownloadState {
                auth: auth_state.clone(),
                root: Arc::new(lock.path.clone()),
            };
            let bind: SocketAddr = if let Some(config) = &production_config {
                config.bind
            } else {
                argument(&args, "--bind")
                    .unwrap_or_else(|| "127.0.0.1:8787".into())
                    .parse()?
            };
            let ready_flag = Arc::new(AtomicBool::new(false));
            let cleanup_signal = auth_state.cleanup_signal();
            let metrics = Arc::new(diagnostics::Metrics::new());
            let backup_boundary = Arc::new(tokio::sync::RwLock::new(()));
            let api = auth::router(auth_state.clone())
                .merge(backups::router(backups::BackupState {
                    auth: auth_state.clone(),
                    root: Arc::new(lock.path.clone()),
                    boundary: backup_boundary.clone(),
                }))
                .merge(catalog::router(catalog::CatalogState {
                    auth: auth_state.clone(),
                    coordinator: coordinator.clone(),
                }))
                .merge(declarations::router(auth_state.clone()))
                .merge(diagnostics::router(diagnostics::DiagnosticsState {
                    auth: auth_state.clone(),
                    metrics: metrics.clone(),
                    root: lock.path.clone(),
                }))
                .merge(channels::router(auth_state))
                .merge(upload::router(upload_state))
                .merge(download::router(download_state))
                .layer(middleware::from_fn_with_state(
                    metrics,
                    diagnostics::observe,
                ))
                .layer(middleware::from_fn_with_state(
                    backup_boundary.clone(),
                    boundary_gate,
                ))
                .layer(middleware::from_fn_with_state(ready_flag.clone(), gate));
            let trusted_proxy_peers = production_config
                .as_ref()
                .map(|config| config.trusted_proxy_peers.clone())
                .unwrap_or_default();
            let api = api.layer(middleware::from_fn_with_state(
                Arc::new(trusted_proxy_peers),
                auth::trusted_origin,
            ));
            let mut app = Router::new()
                .route("/api/v1/health/live", get(live))
                .route("/api/v1/health/ready", get(ready))
                .with_state(ready_flag.clone())
                .nest("/api/v1", api);
            if mode != LocalMode::Dev {
                let assets = if let Some(config) = &production_config {
                    config.assets.clone()
                } else {
                    PathBuf::from(argument(&args, "--assets").ok_or("Preview requires --assets")?)
                };
                if !assets.join("index.html").is_file() {
                    return Err("built Preview assets missing".into());
                }
                app = app.fallback_service(
                    ServeDir::new(&assets)
                        .not_found_service(ServeFile::new(assets.join("index.html"))),
                );
            }
            let listener = tokio::net::TcpListener::bind(bind).await?;
            let flag_for_recovery = ready_flag.clone();
            let backup_pool = pool.clone();
            let backup_root = lock.path.clone();
            let (worker_shutdown_tx, mut worker_shutdown_rx) = tokio::sync::watch::channel(false);
            let mut worker_task = tokio::spawn(async move {
                let recovered = async {
                    app_depot_infrastructure::backups::recover(&backup_pool, &backup_root).await?;
                    coordinator
                        .recover()
                        .await
                        .map_err(|_| app_depot_infrastructure::backups::BackupError::Unavailable)?;
                    Ok::<(), app_depot_infrastructure::backups::BackupError>(())
                }
                .await;
                match recovered {
                    Ok(()) => {
                        flag_for_recovery.store(true, Ordering::Release);
                        println!("{{\"event\":\"ready\"}}");
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(30));
                        interval.tick().await;
                        loop {
                            tokio::select! {
                                _ = interval.tick() => {},
                                _ = cleanup_signal.notified() => {},
                                _ = worker_shutdown_rx.changed() => break,
                            }
                            let cutoff = app_depot_infrastructure::auth::now() - idle_seconds;
                            let _boundary = backup_boundary.read().await;
                            if coordinator.maintenance(cutoff).await.is_err() {
                                eprintln!("{{\"severity\":\"error\",\"event\":\"upload_maintenance_failed\"}}");
                            }
                        }
                    }
                    Err(_) => eprintln!(
                        "{{\"severity\":\"error\",\"event\":\"mandatory_recovery_failed\"}}"
                    ),
                }
            });
            println!(
                "{{\"event\":\"listening\",\"bind\":\"{}\"}}",
                listener.local_addr()?
            );
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let mut server_task = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
            });
            tokio::select! {
                result = &mut server_task => { result??; }
                _ = shutdown_signal() => {
                    ready_flag.store(false, Ordering::Release);
                    let _ = worker_shutdown_tx.send(true);
                    let _ = shutdown_tx.send(());
                    match tokio::time::timeout(std::time::Duration::from_secs(30), &mut server_task).await {
                        Ok(result) => { result??; }
                        Err(_) => {
                            server_task.abort();
                            let _ = server_task.await;
                            eprintln!("{{\"severity\":\"error\",\"event\":\"graceful_shutdown_timeout\"}}");
                        }
                    }
                }
            }
            let _ = worker_shutdown_tx.send(true);
            if tokio::time::timeout(std::time::Duration::from_secs(30), &mut worker_task)
                .await
                .is_err()
            {
                worker_task.abort();
                let _ = worker_task.await;
                eprintln!("{{\"severity\":\"error\",\"event\":\"worker_shutdown_timeout\"}}");
            }
            if tokio::time::timeout(std::time::Duration::from_secs(30), pool.close())
                .await
                .is_err()
            {
                eprintln!("{{\"severity\":\"error\",\"event\":\"sqlite_shutdown_timeout\"}}");
            }
            drop(lock);
        }
        _ => usage(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_production_args;

    #[test]
    fn production_cli_rejects_unknown_and_duplicate_options() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert!(validate_production_args(&args(&["serve", "--mode", "production"])).is_ok());
        assert!(validate_production_args(&args(&[
            "serve",
            "--mode",
            "production",
            "--bind",
            "0.0.0.0:1"
        ]))
        .is_err());
        assert!(validate_production_args(&args(&[
            "serve",
            "--mode",
            "production",
            "--mode",
            "production"
        ]))
        .is_err());
    }
}
