use crate::{
    auth::{authorize, ApiError, AuthState},
    catalog::page,
    download::serve_file,
};
use app_depot_infrastructure::{backups as storage, catalog::CatalogError};
use axum::{
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct BackupState {
    pub auth: AuthState,
    pub root: Arc<PathBuf>,
    pub boundary: Arc<RwLock<()>>,
}

impl From<storage::BackupError> for ApiError {
    fn from(error: storage::BackupError) -> Self {
        match error {
            storage::BackupError::Forbidden => CatalogError::Forbidden.into(),
            storage::BackupError::NotFound | storage::BackupError::Unavailable => {
                CatalogError::NotFound.into()
            }
            storage::BackupError::Invalid => CatalogError::Invalid.into(),
            storage::BackupError::IdempotencyConflict => CatalogError::IdempotencyConflict.into(),
            storage::BackupError::Sql(_)
            | storage::BackupError::Io(_)
            | storage::BackupError::Json(_) => ApiError::server(),
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn create(
    State(state): State<BackupState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !body.is_empty() {
        return Err(CatalogError::Invalid.into());
    }
    let actor = authorize(&state.auth, &headers, true).await?;
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(CatalogError::Invalid)?;
    let (operation, created) = storage::create(&state.auth.pool, &state.root, &actor, key).await?;
    if created {
        let pool = state.auth.pool.clone();
        let root = state.root.clone();
        let boundary = state.boundary.clone();
        let operation_id = operation.id.clone();
        tokio::spawn(async move {
            if storage::run(&pool, &root, &boundary, &operation_id)
                .await
                .is_err()
            {
                eprintln!("{{\"severity\":\"error\",\"event\":\"backup_failed\"}}");
            }
        });
    }
    let mut response = (StatusCode::ACCEPTED, Json(operation.clone())).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/operations/{}", operation.id))
            .map_err(|_| CatalogError::Invalid)?,
    );
    Ok(no_store(response))
}

async fn list(
    State(state): State<BackupState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state.auth, &headers, false).await?;
    let items = storage::completed(&state.auth.pool, &state.root, &actor).await?;
    Ok(no_store(
        page(&items, &query, "backups1", &actor.id, "")?.into_response(),
    ))
}

async fn manifest(
    State(state): State<BackupState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state.auth, &headers, false).await?;
    let manifest = storage::manifest(&state.auth.pool, &state.root, &actor, &id).await?;
    Ok(no_store(Json(manifest).into_response()))
}

async fn member(
    State(state): State<BackupState>,
    headers: HeaderMap,
    Path((id, entry_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state.auth, &headers, false).await?;
    let (path, member) =
        storage::member(&state.auth.pool, &state.root, &actor, &id, &entry_id).await?;
    let response = serve_file(
        path,
        member.size,
        format!("\"backup-{}\"", member.sha256),
        &headers,
    )
    .await?;
    Ok(no_store(response))
}

pub fn router(state: BackupState) -> Router {
    Router::new()
        .route("/backups", post(create).get(list))
        .route("/backups/{backup_id}", get(manifest))
        .route("/backups/{backup_id}/entries/{entry_id}", get(member))
        .layer(middleware::from_fn(
            |request: Request, next: Next| async move { no_store(next.run(request).await) },
        ))
        .with_state(state)
}
