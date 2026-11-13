use crate::{
    auth::{authorize, reader, ApiError, AuthState},
    catalog::page,
};
use app_depot_infrastructure::{
    catalog::{etag, CatalogError},
    declarations as storage,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::Value;
use std::collections::HashMap;

fn response(body: impl serde::Serialize, etag: &str) -> Result<Response, ApiError> {
    let mut response = (StatusCode::OK, Json(body)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag).map_err(|_| CatalogError::Invalid)?,
    );
    Ok(response)
}

async fn current(
    state: AuthState,
    headers: HeaderMap,
    scope: &str,
    id: &str,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let (document, version) = storage::get(&state.pool, principal.as_ref(), scope, id).await?;
    response(document, &etag("declarations", id, version))
}

async fn replace(
    state: AuthState,
    headers: HeaderMap,
    scope: &str,
    id: &str,
    input: Value,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let object = input.as_object().ok_or(CatalogError::Invalid)?;
    if object.len() != 1 || !object.contains_key("data") {
        return Err(CatalogError::Invalid.into());
    }
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let (document, version) = storage::put(
        &state.pool,
        &principal,
        scope,
        id,
        validator,
        &object["data"],
    )
    .await?;
    response(document, &etag("declarations", id, version))
}

async fn revisions(
    state: AuthState,
    headers: HeaderMap,
    scope: &str,
    id: &str,
    query: HashMap<String, String>,
) -> Result<Json<Value>, ApiError> {
    let principal = reader(&state, &headers).await?;
    let items = storage::revisions(&state.pool, principal.as_ref(), scope, id).await?;
    let bound = format!(
        "{scope}:{id}:{}",
        principal
            .as_ref()
            .map(|actor| actor.id.as_str())
            .unwrap_or("anonymous")
    );
    page(&items, &query, "revisions1", &bound, "")
}

async fn revision(
    state: AuthState,
    headers: HeaderMap,
    scope: &str,
    id: &str,
    revision_id: &str,
) -> Result<Json<storage::Revision>, ApiError> {
    let principal = reader(&state, &headers).await?;
    Ok(Json(
        storage::revision(&state.pool, principal.as_ref(), scope, id, revision_id).await?,
    ))
}

async fn app_current(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    current(state, headers, "App", &id).await
}
async fn app_replace(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> Result<Response, ApiError> {
    replace(state, headers, "App", &id, input).await
}
async fn app_revisions(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    revisions(state, headers, "App", &id, query).await
}
async fn app_revision(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((id, revision_id)): Path<(String, String)>,
) -> Result<Json<storage::Revision>, ApiError> {
    revision(state, headers, "App", &id, &revision_id).await
}

async fn release_current(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    current(state, headers, "Release", &id).await
}
async fn release_replace(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> Result<Response, ApiError> {
    replace(state, headers, "Release", &id, input).await
}
async fn release_revisions(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    revisions(state, headers, "Release", &id, query).await
}
async fn release_revision(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((id, revision_id)): Path<(String, String)>,
) -> Result<Json<storage::Revision>, ApiError> {
    revision(state, headers, "Release", &id, &revision_id).await
}

async fn association_current(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    current(state, headers, "Association", &id).await
}
async fn association_replace(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> Result<Response, ApiError> {
    replace(state, headers, "Association", &id, input).await
}
async fn association_revisions(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    revisions(state, headers, "Association", &id, query).await
}
async fn association_revision(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((id, revision_id)): Path<(String, String)>,
) -> Result<Json<storage::Revision>, ApiError> {
    revision(state, headers, "Association", &id, &revision_id).await
}

async fn snapshot(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let (value, validator) = storage::snapshot(&state.pool, principal.as_ref(), &id).await?;
    response(value, &validator)
}

pub fn router(state: AuthState) -> Router {
    Router::new()
        .route(
            "/apps/{app_id}/declarations",
            get(app_current).put(app_replace),
        )
        .route("/apps/{app_id}/declarations/revisions", get(app_revisions))
        .route(
            "/apps/{app_id}/declarations/revisions/{revision_id}",
            get(app_revision),
        )
        .route(
            "/releases/{release_id}/declarations",
            get(release_current).put(release_replace),
        )
        .route(
            "/releases/{release_id}/declarations/revisions",
            get(release_revisions),
        )
        .route(
            "/releases/{release_id}/declarations/revisions/{revision_id}",
            get(release_revision),
        )
        .route(
            "/associations/{association_id}/declarations",
            get(association_current).put(association_replace),
        )
        .route(
            "/associations/{association_id}/declarations/revisions",
            get(association_revisions),
        )
        .route(
            "/associations/{association_id}/declarations/revisions/{revision_id}",
            get(association_revision),
        )
        .route("/releases/{release_id}/declaration-snapshot", get(snapshot))
        .with_state(state)
}
