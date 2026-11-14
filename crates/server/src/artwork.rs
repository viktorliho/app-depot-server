use crate::auth::{authorize, reader, ApiError, AuthState};
use crate::download::serve_blob;
use app_depot_infrastructure::{artwork as storage, catalog::CatalogError};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct ArtworkState {
    pub auth: AuthState,
    pub root: Arc<PathBuf>,
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn list(
    State(state): State<ArtworkState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    let items = storage::list(&state.auth.pool, principal.as_ref(), &app_id).await?;
    Ok(Json(serde_json::json!({"items": items})).into_response())
}

async fn metadata(
    State(state): State<ArtworkState>,
    headers: HeaderMap,
    Path((app_id, role)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    let item = storage::get(&state.auth.pool, principal.as_ref(), &app_id, &role).await?;
    let mut response = Json(&item).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&item.etag).map_err(|_| ApiError::server())?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn content(
    State(state): State<ArtworkState>,
    headers: HeaderMap,
    Path((app_id, role)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    let item = storage::get(&state.auth.pool, principal.as_ref(), &app_id, &role).await?;
    let mut response = serve_blob(&state.root, &item.digest, item.size, &headers).await?;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&item.mime_type).map_err(|_| ApiError::server())?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn put(
    State(state): State<ArtworkState>,
    headers: HeaderMap,
    Path((app_id, role)): Path<(String, String)>,
    body: Body,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    let key = header_value(&headers, header::HeaderName::from_static("idempotency-key"))
        .ok_or(CatalogError::Invalid)?;
    let if_match = header_value(&headers, header::IF_MATCH);
    let if_none = header_value(&headers, header::IF_NONE_MATCH);
    if if_match.is_some() && if_none.is_some() {
        return Err(CatalogError::Invalid.into());
    }
    if (if_none.is_some() && if_none != Some("*")) || if_match == Some("*") {
        return Err(CatalogError::StaleEtag.into());
    }
    let validator = if_none.or(if_match);
    let bytes = to_bytes(body, storage::MAX_IMAGE_BYTES + 1)
        .await
        .map_err(|_| CatalogError::ImageTooLarge)?;
    let item = storage::put(
        &state.auth.pool,
        &state.root,
        &principal,
        key,
        &app_id,
        &role,
        validator,
        &bytes,
    )
    .await?;
    state.auth.trigger_cleanup();
    let mut response = (StatusCode::OK, Json(&item)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&item.etag).map_err(|_| ApiError::server())?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn delete(
    State(state): State<ArtworkState>,
    headers: HeaderMap,
    Path((app_id, role)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    storage::delete(
        &state.auth.pool,
        &principal,
        &app_id,
        &role,
        header_value(&headers, header::IF_MATCH),
    )
    .await?;
    state.auth.trigger_cleanup();
    Ok(StatusCode::NO_CONTENT)
}

pub fn router(state: ArtworkState) -> Router {
    Router::new()
        .route("/apps/{app_id}/artwork", get(list))
        .route(
            "/apps/{app_id}/artwork/{role}",
            get(metadata).put(put).delete(delete),
        )
        .route("/apps/{app_id}/artwork/{role}/content", get(content))
        .with_state(state)
}
