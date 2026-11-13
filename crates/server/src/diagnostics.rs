use crate::auth::{authorize, ApiError, AuthState};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method},
    middleware::Next,
    response::Response,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct Sample {
    at: Instant,
    latency_ms: f64,
    failed: bool,
}

pub struct Metrics {
    requests: Mutex<VecDeque<Sample>>,
    active_transfers: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            requests: Mutex::new(VecDeque::new()),
            active_transfers: AtomicU64::new(0),
        }
    }

    fn summarize(&self) -> (u64, f64) {
        let mut samples = self.requests.lock().unwrap();
        let cutoff = Instant::now() - Duration::from_secs(300);
        while samples.front().is_some_and(|sample| sample.at < cutoff) {
            samples.pop_front();
        }
        let failures = samples.iter().filter(|sample| sample.failed).count() as u64;
        let mut latency: Vec<f64> = samples.iter().map(|sample| sample.latency_ms).collect();
        latency.sort_by(|a, b| a.total_cmp(b));
        let p95 = if latency.is_empty() {
            0.0
        } else {
            latency[((latency.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)]
        };
        (failures, p95)
    }
}

pub async fn observe(
    State(metrics): State<Arc<Metrics>>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let method = request.method().as_str().to_owned();
    let requested_operation = request
        .uri()
        .path()
        .strip_prefix("/api/v1/operations/")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .map(str::to_owned);
    let transfer = request.method() == Method::PUT
        && request.uri().path().contains("/uploads/")
        && request.uri().path().contains("/files/");
    if transfer {
        metrics.active_transfers.fetch_add(1, Ordering::AcqRel);
    }
    let started = Instant::now();
    let mut response = next.run(request).await;
    if transfer {
        metrics.active_transfers.fetch_sub(1, Ordering::AcqRel);
    }
    let mut samples = metrics.requests.lock().unwrap();
    samples.push_back(Sample {
        at: Instant::now(),
        latency_ms: started.elapsed().as_secs_f64() * 1000.0,
        failed: response.status().is_client_error() || response.status().is_server_error(),
    });
    drop(samples);
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    let operation_id = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| location.strip_prefix("/api/v1/operations/"))
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .map(str::to_owned)
        .or(requested_operation);
    response
        .headers_mut()
        .insert("x-request-id", HeaderValue::from_str(&request_id).unwrap());
    let mut event = json!({"severity":if response.status().is_server_error() {"error"} else {"info"},
        "event":"http_request","requestId":request_id,"method":method,
        "status":response.status().as_u16(),"latencyMs":latency_ms});
    if let Some(operation_id) = operation_id {
        event["operationId"] = json!(operation_id);
    }
    println!("{event}");
    response
}

#[derive(Clone)]
pub struct DiagnosticsState {
    pub auth: AuthState,
    pub metrics: Arc<Metrics>,
    pub root: PathBuf,
}

async fn get_diagnostics(
    State(state): State<DiagnosticsState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    if !principal.server_admin {
        return Err(app_depot_infrastructure::catalog::CatalogError::Forbidden.into());
    }
    let (failures, p95) = state.metrics.summarize();
    let active_uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM upload_sessions WHERE phase IN ('Receiving','Verifying','Importing','Committing')")
        .fetch_one(&state.auth.pool).await.map_err(app_depot_infrastructure::catalog::CatalogError::Sql)?;
    let pending_cleanup: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM cleanup_obligations WHERE state!='Completed') + (SELECT count(*) FROM deletion_operations WHERE status!='Completed')")
        .fetch_one(&state.auth.pool).await.map_err(app_depot_infrastructure::catalog::CatalogError::Sql)?;
    let retryable_cleanup: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM cleanup_obligations WHERE state='RetryableFailure') + (SELECT count(*) FROM deletion_operations WHERE physical_cleanup='RetryableFailure')")
        .fetch_one(&state.auth.pool).await.map_err(app_depot_infrastructure::catalog::CatalogError::Sql)?;
    let backups_running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM backup_operations WHERE status IN ('Accepted','Running')",
    )
    .fetch_one(&state.auth.pool)
    .await
    .map_err(app_depot_infrastructure::catalog::CatalogError::Sql)?;
    let backups_failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM backup_operations WHERE status='Failed'")
            .fetch_one(&state.auth.pool)
            .await
            .map_err(app_depot_infrastructure::catalog::CatalogError::Sql)?;
    let available = fs2::available_space(&state.root).map_err(|_| ApiError::server())?;
    let total = fs2::total_space(&state.root).map_err(|_| ApiError::server())?;
    let remaining = if total == 0 {
        0.0
    } else {
        available as f64 / total as f64
    };
    let pressure = if remaining < 0.05 {
        "Critical"
    } else if remaining < 0.15 {
        "Warning"
    } else {
        "Normal"
    };
    Ok(Json(
        json!({"readiness":"Ready","recovery":"Idle","migration":"Idle","storagePressure":pressure,
        "windowSeconds":300,"requestFailures":failures,"requestLatencyMsP95":p95,
        "activeUploads":active_uploads,"activeTransfers":state.metrics.active_transfers.load(Ordering::Acquire),
        "pendingCleanup":pending_cleanup,"retryableCleanup":retryable_cleanup,
        "backupsRunning":backups_running,"backupsFailed":backups_failed}),
    ))
}

pub fn router(state: DiagnosticsState) -> Router {
    Router::new()
        .route("/admin/diagnostics", get(get_diagnostics))
        .with_state(state)
}
