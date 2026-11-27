use crate::{
    auth::{authorize, ApiError, AuthState},
    gog,
    import_source::{ImportRoot, Scan, SourceFile, SourcePage},
};
use app_depot_application::{
    upload::{
        CancelOutcome, FileClaim, Interval, InventoryBatch, InventoryEntry, InventoryEntryInput,
        Target, UploadCoordinator, UploadCreate, UploadError,
    },
    Principal,
};
use app_depot_infrastructure::upload::UploadAdapter;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    collections::HashMap,
    convert::Infallible,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{mpsc, Semaphore},
};

#[derive(Clone)]
pub struct ImportState {
    pub auth: AuthState,
    pub coordinator: Arc<UploadCoordinator<UploadAdapter>>,
    pub roots: Arc<Vec<ImportRoot>>,
    pub inbox_path: PathBuf,
    pub active: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub permits: Arc<Semaphore>,
    pub gog_permit: Arc<Semaphore>,
    pub gog_scratch: Arc<PathBuf>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportRequest {
    root_id: String,
    path: String,
    mode: String,
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gog_review: Option<GogReview>,
    #[serde(skip)]
    store_declaration: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GogReview {
    setup: String,
    analysis_id: String,
    #[serde(default)]
    hints: gog::Hints,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry_point: Option<gog::EntryPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    disabled_prerequisites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirmed_locale: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    confirmed_bundled_locales: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnalysisRequest {
    root_id: String,
    path: String,
    setup: Option<String>,
}

pub(crate) struct CancelOnDrop(pub(crate) Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn gog_error(error: gog::Error) -> ApiError {
    let (code, title) = error.problem();
    let status = match error {
        gog::Error::Environment => StatusCode::SERVICE_UNAVAILABLE,
        gog::Error::Changed => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    ApiError::problem(status, code, title)
}

fn review_binding(
    principal: &Principal,
    root: &ImportRoot,
    path: &str,
    report: &gog::Analysis,
) -> String {
    serde_json::json!([principal.id, root.id, path, gog::analysis_id(report)]).to_string()
}

async fn analyze_gog(
    State(state): State<ImportState>,
    headers: HeaderMap,
    Json(input): Json<AnalysisRequest>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    let root = root_for(&state, &input.root_id, &principal)?.clone();
    let permit = state
        .gog_permit
        .clone()
        .try_acquire_owned()
        .map_err(|_| UploadError::OperationRunning)?;
    let flag = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(flag.clone());
    let worker_key = format!("analysis:{}", uuid::Uuid::new_v4());
    state
        .active
        .lock()
        .unwrap()
        .insert(worker_key.clone(), flag.clone());
    let worker = ActiveGuard {
        key: worker_key,
        active: state.active.clone(),
    };
    let result = tokio::task::spawn_blocking(move || {
        let _worker = worker;
        let _permit = permit;
        let candidates = gog::candidates(&root, &input.path)?;
        let setup = match input.setup {
            Some(setup) if candidates.contains(&setup) => setup,
            Some(_) => return Err(gog::Error::Source),
            None if candidates.len() == 1 => candidates[0].clone(),
            None if candidates.is_empty() => return Err(gog::Error::Source),
            None => return Ok(serde_json::json!({"candidates": candidates, "analysis": null, "analysisId": null})),
        };
        let (work, volumes) = gog::Work::prepare(&state.gog_scratch, &root, &input.path, &setup, &flag)?;
        let (analysis, paths) = work.inspect(setup, volumes, &flag, true)?;
        let proposal = work.discovery_preview(&analysis, &paths, &flag)?;
        let id = state.auth.sign_import_analysis(&review_binding(&principal, &root, &input.path, &analysis));
        Ok(serde_json::json!({"candidates": candidates, "analysis": analysis, "analysisId": id, "proposal": proposal}))
    }).await.map_err(|_| ApiError::server())?.map_err(gog_error)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootView<'a> {
    id: &'a str,
    display_name: &'a str,
    kind: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowseQuery {
    path: Option<String>,
    cursor: Option<String>,
    page_size: Option<usize>,
}

fn root_for<'a>(
    state: &'a ImportState,
    id: &str,
    principal: &Principal,
) -> Result<&'a ImportRoot, ApiError> {
    state
        .roots
        .iter()
        .find(|root| {
            root.id == id && root.available() && root.allows(&principal.id, principal.server_admin)
        })
        .ok_or_else(|| UploadError::NotFound.into())
}

async fn list_roots(
    State(state): State<ImportState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    let items: Vec<_> = state
        .roots
        .iter()
        .filter(|root| root.available() && root.allows(&principal.id, principal.server_admin))
        .map(|root| RootView {
            id: &root.id,
            display_name: &root.display_name,
            kind: root.kind,
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn admin_sources(
    State(state): State<ImportState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    if !principal.server_admin {
        return Err(UploadError::Forbidden.into());
    }
    let mut items = vec![serde_json::json!({
        "id": "inbox",
        "displayName": "App Depot Inbox",
        "kind": "Inbox",
        "containerPath": state.inbox_path,
        "available": state.roots.iter().find(|root| root.id == "inbox").is_some_and(ImportRoot::available),
        "importReadOnly": true,
    })];
    items.extend(
        state
            .roots
            .iter()
            .filter(|root| root.kind == "Configured")
            .map(|root| {
                serde_json::json!({
                    "id": root.id,
                    "displayName": root.display_name,
                    "kind": root.kind,
                    "containerPath": root.container_path,
                    "available": root.available(),
                    "importReadOnly": true,
                })
            }),
    );
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn browse(
    State(state): State<ImportState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<SourcePage>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    let root = root_for(&state, &id, &principal)?.clone();
    let path = query.path.unwrap_or_default();
    let cursor = query.cursor;
    let page_size = query.page_size.unwrap_or(100);
    tokio::task::spawn_blocking(move || root.browse(&path, cursor.as_deref(), page_size))
        .await
        .map_err(|_| UploadError::Retryable("import browser unavailable".into()))?
        .map(Json)
        .map_err(|_| UploadError::Invalid.into())
}

fn scoped_key(principal: &Principal, key: &str, purpose: &str, part: usize) -> String {
    let mut hash = Sha256::new();
    hash.update(principal.id.as_bytes());
    hash.update([0]);
    hash.update(key.as_bytes());
    hash.update([0]);
    hash.update(purpose.as_bytes());
    hash.update([0]);
    hash.update(part.to_le_bytes());
    hex::encode(hash.finalize())
}

async fn bind_request(
    state: &ImportState,
    principal: &Principal,
    key: &str,
    input: &ImportRequest,
) -> Result<Option<String>, UploadError> {
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(input).map_err(|_| UploadError::Invalid)?,
    ));
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| UploadError::Retryable(e.to_string()))?;
    sqlx::query("INSERT OR IGNORE INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'ServerImport', ?, ?, '', '{}', strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
        .bind(&principal.id).bind(key).bind(&digest).execute(&mut *tx).await.map_err(|e| UploadError::Retryable(e.to_string()))?;
    let row = sqlx::query("SELECT input_hash, result_id FROM idempotency_bindings WHERE principal_id=? AND action='ServerImport' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await.map_err(|e| UploadError::Retryable(e.to_string()))?;
    if row.get::<String, _>("input_hash") != digest {
        return Err(UploadError::IdempotencyConflict);
    }
    let session = row.get::<String, _>("result_id");
    tx.commit()
        .await
        .map_err(|e| UploadError::Retryable(e.to_string()))?;
    Ok((!session.is_empty()).then_some(session))
}

async fn emit(tx: &mpsc::Sender<String>, value: serde_json::Value) {
    let _ = tx.send(format!("data: {value}\n\n")).await;
}

struct ActiveGuard {
    key: String,
    active: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

struct ImportProgress<'a> {
    tx: &'a mpsc::Sender<String>,
    session_id: &'a str,
    files_total: usize,
    bytes_total: u64,
    files_done: usize,
    bytes_before: u64,
}

impl ImportProgress<'_> {
    async fn report(&self, copied: u64) {
        emit(
            self.tx,
            serde_json::json!({
                "phase": "Receiving",
                "sessionId": self.session_id,
                "filesTotal": self.files_total,
                "bytesTotal": self.bytes_total,
                "filesProcessed": self.files_done,
                "bytesProcessed": self.bytes_before + copied,
            }),
        )
        .await;
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.active.lock().unwrap().remove(&self.key);
    }
}

async fn start(
    State(state): State<ImportState>,
    headers: HeaderMap,
    Json(input): Json<ImportRequest>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 255)
        .ok_or(UploadError::Invalid)?
        .to_owned();
    if !matches!(
        input.mode.as_str(),
        "BareFile" | "BareTree" | "ZipImport" | "GogOfflineInstaller"
    ) {
        return Err(UploadError::UnsupportedKind.into());
    }
    match (input.mode.as_str(), input.gog_review.as_ref()) {
        ("GogOfflineInstaller", Some(review))
            if review.analysis_id.len() == 64
                && review.analysis_id.bytes().all(|b| b.is_ascii_hexdigit())
                && review.hints.valid()
                && review.disabled_prerequisites.len() <= 64
                && review
                    .disabled_prerequisites
                    .iter()
                    .all(|path| app_depot_domain::recipe::path(path))
                && (review.confirmed_bundled_locales.is_empty()
                    || (review.confirmed_locale.is_none()
                        && app_depot_domain::installation::bundled_languages(
                            &review.confirmed_bundled_locales,
                        )))
                && review.confirmed_locale.as_deref().is_none_or(|tag| {
                    tag.len() <= 80 && app_depot_domain::installation::language_id(tag)
                }) => {}
        ("GogOfflineInstaller", _) | (_, Some(_)) => return Err(UploadError::Invalid.into()),
        _ => {}
    }
    let root = root_for(&state, &input.root_id, &principal)?.clone();
    // Validate source syntax before persisting the idempotency binding.
    if !input.path.is_empty() {
        app_depot_domain::tree::validate_path(&input.path).map_err(|_| UploadError::Invalid)?;
    } else if !matches!(input.mode.as_str(), "BareTree" | "GogOfflineInstaller") {
        return Err(UploadError::Invalid.into());
    }
    let active_key = format!("{}:{key}", principal.id);
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = state.active.lock().unwrap();
        if active.contains_key(&active_key) {
            return Err(UploadError::OperationRunning.into());
        }
        active.insert(active_key.clone(), cancelled.clone());
    }
    let (tx, rx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        let _guard = ActiveGuard {
            key: active_key,
            active: state.active.clone(),
        };
        let result = run_import(&state, &root, &principal, &key, &input, &tx, &cancelled).await;
        if let Err(error) = result {
            let (code, title) = match error {
                UploadError::HashMismatch { .. } => ("hash_mismatch", "Source bytes did not match the scanned identity. Start a new import after the source is stable."),
                UploadError::ConflictingBytes => ("source_changed", "The source changed during import. Start a new import after it is stable."),
                UploadError::IdempotencyConflict => ("idempotency_conflict", "The source or request differs from this import attempt. Start a new import with a new key."),
                UploadError::SessionCancelled => ("session_cancelled", "Import cancelled."),
                UploadError::NotFound => ("source_unavailable", "The selected source is no longer available."),
                UploadError::Invalid => ("invalid_source", "The source is invalid or contains an unsupported entry."),
                UploadError::OperationRunning => ("operation_running", "An installer operation is already active. Retry when it finishes."),
                UploadError::Incomplete => ("incomplete_transfer", "Source transfer is incomplete. Retry this import."),
                UploadError::Retryable(_) => ("retryable_import_failure", "Temporary Server failure. Retry this import."),
                _ => ("import_failed", "Server import failed. Check the source and retry or start a new import."),
            };
            emit(&tx, serde_json::json!({ "phase": "Failed", "problem": { "code": code, "title": title } })).await;
        }
    });
    let body = Body::from_stream(stream::unfold(rx, |mut receiver| async {
        let body =
            match tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await {
                Ok(Some(line)) => Bytes::from(line),
                Ok(None) => return None,
                Err(_) => Bytes::from_static(b": waiting\n\n"),
            };
        Some((Ok::<Bytes, Infallible>(body), receiver))
    }));
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelRequest {
    request_key: String,
}

async fn cancel(
    State(state): State<ImportState>,
    headers: HeaderMap,
    Json(input): Json<CancelRequest>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    if input.request_key.is_empty() || input.request_key.len() > 255 {
        return Err(UploadError::Invalid.into());
    }
    let active_key = format!("{}:{}", principal.id, input.request_key);
    let active = state.active.lock().unwrap().get(&active_key).cloned();
    if let Some(active) = &active {
        active.store(true, Ordering::Relaxed);
    }
    let session: Option<String> = sqlx::query_scalar("SELECT result_id FROM idempotency_bindings WHERE principal_id=? AND action='ServerImport' AND key=?")
        .bind(&principal.id).bind(&input.request_key)
        .fetch_optional(&state.auth.pool).await.map_err(|_| UploadError::Retryable("import state unavailable".into()))?;
    if let Some(session) = session.filter(|session| !session.is_empty()) {
        return match state
            .coordinator
            .cancel(
                &principal,
                &session,
                &scoped_key(&principal, &input.request_key, "cancel", 0),
            )
            .await?
        {
            CancelOutcome::Completed(result) => Ok(Json(
                serde_json::to_value(result).map_err(|_| UploadError::Invalid)?,
            )
            .into_response()),
            CancelOutcome::Accepted {
                operation_id,
                status,
            } => Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "operationId": operation_id, "status": status })),
            )
                .into_response()),
        };
    }
    if active.is_none() {
        return Err(UploadError::NotFound.into());
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "Accepted" })),
    )
        .into_response())
}

async fn run_import(
    state: &ImportState,
    root: &ImportRoot,
    principal: &Principal,
    key: &str,
    input: &ImportRequest,
    tx: &mpsc::Sender<String>,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), UploadError> {
    let _permit = state
        .permits
        .acquire()
        .await
        .map_err(|_| UploadError::Retryable("import worker unavailable".into()))?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(UploadError::SessionCancelled);
    }
    let existing = bind_request(state, principal, key, input).await?;
    if let Some(id) = &existing {
        if let Some(result) = state.coordinator.result(principal, id).await? {
            emit(
                tx,
                serde_json::json!({ "phase": result.phase, "sessionId": id, "result": result }),
            )
            .await;
            return Ok(());
        }
    }
    emit(tx, serde_json::json!({ "phase": "Scanning" })).await;
    let _staged;
    let transfer_root;
    let scan;
    let manifest;
    if let Some(review) = &input.gog_review {
        let _permit = state
            .gog_permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| UploadError::OperationRunning)?;
        let scratch = state.gog_scratch.clone();
        let source = root.clone();
        let auth = state.auth.clone();
        let reviewing_principal = principal.clone();
        let path = input.path.clone();
        let flag = cancelled.clone();
        let review = review.clone();
        emit(tx, serde_json::json!({"phase":"ExtractingInstaller"})).await;
        let result = tokio::task::spawn_blocking(move || {
            let (work, volumes) =
                gog::Work::prepare(&scratch, &source, &path, &review.setup, &flag)?;
            let (report, listed) = work.inspect(review.setup, volumes, &flag, false)?;
            if !auth.verify_import_analysis(
                &review_binding(&reviewing_principal, &source, &path, &report),
                &review.analysis_id,
            ) {
                return Err(gog::Error::Changed);
            }
            let (root, scan) = work.extract(&report, &listed, &flag)?;
            let proposal = gog::Work::discovery_import(&report, &root, &scan, &flag);
            if flag.load(Ordering::Relaxed) {
                return Err(gog::Error::Cancelled);
            }
            let mut manifest =
                gog::declaration(report, &review.hints, &scan, review.entry_point.as_ref())?;
            let source = &mut manifest["appDepot"]["gogOfflineInstaller"];
            source["detected"]["installationDiscovery"] = serde_json::json!(proposal);
            if !review.disabled_prerequisites.is_empty() {
                source["user"]["disabledPrerequisites"] =
                    serde_json::json!(review.disabled_prerequisites);
            }
            if let Some(locale) = review.confirmed_locale {
                source["user"]["confirmedLocale"] = serde_json::json!(locale);
            }
            if !review.confirmed_bundled_locales.is_empty() {
                source["user"]["confirmedBundledLocales"] =
                    serde_json::json!(review.confirmed_bundled_locales);
            }
            Ok((work, root, scan, manifest))
        })
        .await
        .map_err(|_| UploadError::Retryable("extractor worker unavailable".into()))?;
        match result {
            Ok((work, extracted, inventory, metadata)) => {
                _staged = work;
                transfer_root = extracted;
                scan = inventory;
                manifest = Some(metadata);
            }
            Err(error) => {
                let (code, title) = error.problem();
                emit(tx, serde_json::json!({ "phase": "Failed", "problem": { "code": code, "title": title } })).await;
                return Ok(());
            }
        }
    } else {
        transfer_root = root.clone();
        let scan_root = root.clone();
        let path = input.path.clone();
        let mode = input.mode.clone();
        let scan_flag = cancelled.clone();
        scan = tokio::task::spawn_blocking(move || {
            scan_root.scan_with_cancel(&path, &mode, &scan_flag)
        })
        .await
        .map_err(|_| UploadError::Retryable("source scan unavailable".into()))?
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::Interrupted => UploadError::SessionCancelled,
            std::io::ErrorKind::InvalidData => UploadError::ConflictingBytes,
            _ => UploadError::Invalid,
        })?;
        manifest = input.store_declaration.clone();
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(UploadError::SessionCancelled);
    }
    let upload_mode = if manifest.is_some() {
        "BareTree"
    } else {
        &input.mode
    };
    let create = UploadCreate {
        mode: upload_mode.into(),
        target: input.target.clone(),
        file: (upload_mode != "BareTree").then(|| FileClaim {
            size: scan.files[0].size,
            sha256: scan.files[0].sha256.clone(),
        }),
    };
    let session = state
        .coordinator
        .create(principal, &scoped_key(principal, key, "create", 0), &create)
        .await?;
    let updated = sqlx::query("UPDATE idempotency_bindings SET result_id=? WHERE principal_id=? AND action='ServerImport' AND key=? AND (result_id='' OR result_id=?)")
        .bind(&session.id).bind(&principal.id).bind(key).bind(&session.id)
        .execute(&state.auth.pool).await.map_err(|e| UploadError::Retryable(e.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(UploadError::IdempotencyConflict);
    }
    if let Some(manifest) = &manifest {
        if session.phase == "Receiving" {
            app_depot_infrastructure::upload::bind_import_declaration(
                &state.auth.pool,
                principal,
                &session.id,
                manifest,
            )
            .await?;
        }
    }
    emit(tx, serde_json::json!({ "phase": "Receiving", "sessionId": session.id, "filesTotal": scan.files.len(), "bytesTotal": scan.total_bytes, "filesProcessed": 0, "bytesProcessed": 0 })).await;
    if session.phase != "Receiving" {
        if let Some(result) = state.coordinator.result(principal, &session.id).await? {
            emit(tx, serde_json::json!({ "phase": result.phase, "sessionId": session.id, "result": result })).await;
            return Ok(());
        }
        return Err(UploadError::OperationRunning);
    }
    let assigned = if upload_mode == "BareTree" {
        prepare_inventory(state, principal, key, &session.id, &scan).await?
    } else {
        HashMap::from([(String::new(), session.file().file_id.clone())])
    };
    let mut bytes_done = 0u64;
    for (index, file) in scan.files.iter().enumerate() {
        let file_id = assigned
            .get(&file.logical_path)
            .ok_or(UploadError::Invalid)?;
        let mut progress = ImportProgress {
            tx,
            session_id: &session.id,
            files_total: scan.files.len(),
            bytes_total: scan.total_bytes,
            files_done: index,
            bytes_before: bytes_done,
        };
        transfer_file(
            state,
            &transfer_root,
            principal,
            file_id,
            file,
            &progress,
            cancelled,
        )
        .await?;
        bytes_done += file.size;
        if index % 20 == 0 || index + 1 == scan.files.len() {
            progress.files_done = index + 1;
            progress.report(file.size).await;
        }
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(UploadError::SessionCancelled);
    }
    emit(
        tx,
        serde_json::json!({ "phase": "Verifying", "sessionId": session.id }),
    )
    .await;
    let finalize_key = scoped_key(principal, key, "finalize", 0);
    let coordinator = state.coordinator.clone();
    let observed_principal = principal.clone();
    let observed_id = session.id.clone();
    let observed_tx = tx.clone();
    let observer = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        let mut reported = String::from("Verifying");
        loop {
            interval.tick().await;
            if let Ok(current) = coordinator.get(&observed_principal, &observed_id).await {
                if current.phase != reported
                    && matches!(
                        current.phase.as_str(),
                        "Verifying" | "Importing" | "Committing"
                    )
                {
                    reported = current.phase.clone();
                    emit(
                        &observed_tx,
                        serde_json::json!({ "phase": reported, "sessionId": observed_id }),
                    )
                    .await;
                }
            }
        }
    });
    let result = state
        .coordinator
        .finalize(principal, &session.id, &finalize_key)
        .await;
    observer.abort();
    let result = result?;
    emit(
        tx,
        serde_json::json!({ "phase": result.phase, "sessionId": session.id, "result": result }),
    )
    .await;
    Ok(())
}

pub(crate) async fn import_store_tree(
    state: &ImportState,
    principal: &Principal,
    root: ImportRoot,
    target: Target,
    key: &str,
    declaration: serde_json::Value,
    progress: &mpsc::Sender<String>,
) -> Result<app_depot_application::upload::FinalizationResult, UploadError> {
    let request = ImportRequest {
        root_id: "store".into(),
        path: String::new(),
        mode: "BareTree".into(),
        target,
        gog_review: None,
        store_declaration: Some(declaration),
    };
    // A restored or expired upload has an immutable terminal result. Derive
    // the next attempt from that exact session so a restart follows the same chain.
    let mut attempt = key.to_owned();
    while let Some(session) = bind_request(state, principal, &attempt, &request).await? {
        match state.coordinator.result(principal, &session).await? {
            Some(result) if result.phase == "Completed" => return Ok(result),
            Some(result) if matches!(result.phase.as_str(), "Failed" | "Cancelled") => {
                attempt = hex::encode(Sha256::digest(format!("{key}:{session}")));
            }
            _ => break,
        }
    }
    run_import(
        state,
        &root,
        principal,
        &attempt,
        &request,
        progress,
        &Arc::new(AtomicBool::new(false)),
    )
    .await?;
    let session = bind_request(state, principal, &attempt, &request)
        .await?
        .ok_or(UploadError::Invalid)?;
    state
        .coordinator
        .result(principal, &session)
        .await?
        .filter(|r| r.phase == "Completed")
        .ok_or(UploadError::Invalid)
}

async fn prepare_inventory(
    state: &ImportState,
    principal: &Principal,
    key: &str,
    session_id: &str,
    scan: &Scan,
) -> Result<HashMap<String, String>, UploadError> {
    let mut entries: Vec<InventoryEntryInput> = scan
        .directories
        .iter()
        .map(|path| InventoryEntryInput::Directory { path: path.clone() })
        .collect();
    entries.extend(scan.files.iter().map(|file| InventoryEntryInput::File {
        path: file.logical_path.clone(),
        size: file.size,
        sha256: file.sha256.clone(),
        executable: file.executable,
    }));
    entries.sort_by(|a, b| a.path().as_bytes().cmp(b.path().as_bytes()));
    let session = state.coordinator.get(principal, session_id).await?;
    if !session.inventory_complete {
        for (index, chunk) in entries.chunks(1000).enumerate() {
            state
                .coordinator
                .append_inventory(
                    principal,
                    session_id,
                    &scoped_key(principal, key, "inventory", index),
                    &InventoryBatch {
                        entries: chunk.to_vec(),
                    },
                )
                .await?;
        }
        state
            .coordinator
            .complete_inventory(
                principal,
                session_id,
                &scoped_key(principal, key, "complete", 0),
            )
            .await?;
    }
    let actual = state.coordinator.inventory(principal, session_id).await?;
    if actual.len() != entries.len() {
        return Err(UploadError::IdempotencyConflict);
    }
    let mut assigned = HashMap::new();
    for (claim, saved) in entries.iter().zip(actual.iter()) {
        match (claim, saved) {
            (InventoryEntryInput::Directory { path: a }, InventoryEntry::Directory { path: b })
                if a == b => {}
            (
                InventoryEntryInput::File {
                    path: a,
                    size: asize,
                    sha256: ahash,
                    executable: ax,
                },
                InventoryEntry::File {
                    path: b,
                    size: bsize,
                    sha256: bhash,
                    executable: bx,
                    file_id,
                },
            ) if a == b && asize == bsize && ahash == bhash && ax == bx => {
                assigned.insert(a.clone(), file_id.clone());
            }
            _ => return Err(UploadError::IdempotencyConflict),
        }
    }
    Ok(assigned)
}

async fn transfer_file(
    state: &ImportState,
    root: &ImportRoot,
    principal: &Principal,
    file_id: &str,
    claim: &SourceFile,
    progress: &ImportProgress<'_>,
    cancelled: &AtomicBool,
) -> Result<(), UploadError> {
    if claim.size == 0 {
        return Ok(());
    }
    let source = root
        .open_file(&claim.source_path)
        .map_err(|_| UploadError::NotFound)?;
    let before = source.metadata().map_err(|_| UploadError::NotFound)?;
    if !claim.matches_metadata(&before) {
        return Err(UploadError::ConflictingBytes);
    }
    let mut source = tokio::fs::File::from_std(source);
    let ranges = state
        .coordinator
        .progress(principal, progress.session_id, file_id)
        .await?
        .ranges;
    let mut acknowledged: u64 = ranges.iter().map(|range| range.end - range.start).sum();
    let mut cursor = 0u64;
    for saved in ranges.iter().chain(std::iter::once(&Interval {
        start: claim.size,
        end: claim.size,
    })) {
        while cursor < saved.start {
            if cancelled.load(Ordering::Relaxed) {
                return Err(UploadError::SessionCancelled);
            }
            let end = (cursor + 4 * 1024 * 1024).min(saved.start);
            source
                .seek(std::io::SeekFrom::Start(cursor))
                .await
                .map_err(|_| UploadError::NotFound)?;
            let mut body = (&mut source).take(end - cursor);
            state
                .coordinator
                .receive(
                    principal,
                    progress.session_id,
                    file_id,
                    Interval { start: cursor, end },
                    &mut body,
                )
                .await?;
            acknowledged += end - cursor;
            if claim.size > 8 * 1024 * 1024 {
                progress.report(acknowledged).await;
            }
            cursor = end;
        }
        cursor = saved.end;
    }
    let after = source.metadata().await.map_err(|_| UploadError::NotFound)?;
    if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
        return Err(UploadError::ConflictingBytes);
    }
    Ok(())
}

pub fn router(state: ImportState) -> Router {
    Router::new()
        .route("/admin/import-sources", get(admin_sources))
        .route("/server-imports/roots", get(list_roots))
        .route("/server-imports/roots/{id}/entries", get(browse))
        .route("/server-imports/gog/analysis", post(analyze_gog))
        .route("/server-imports", post(start).delete(cancel))
        .with_state(state)
}
