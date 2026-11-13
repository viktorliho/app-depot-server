use crate::{
    auth::{authorize, reader, ApiError, AuthState},
    catalog::page,
};
use app_depot_infrastructure::channels as storage;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelPut {
    release_id: String,
}

async fn list(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = reader(&state, &headers).await?;
    let items = storage::list(&state.pool, actor.as_ref(), &app_id).await?;
    let scope = format!(
        "{}:{}",
        app_id,
        actor
            .as_ref()
            .map(|value| value.id.as_str())
            .unwrap_or("anonymous")
    );
    page(&items, &query, "channels1", &scope, "")
}

async fn get_channel(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((app_id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let actor = reader(&state, &headers).await?;
    let (channel, validator) = storage::get(&state.pool, actor.as_ref(), &app_id, &name).await?;
    let mut response = Json(channel).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&validator).map_err(|_| {
            ApiError::from(app_depot_infrastructure::catalog::CatalogError::Invalid)
        })?,
    );
    Ok(response)
}

async fn put_channel(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((app_id, name)): Path<(String, String)>,
    Json(input): Json<ChannelPut>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &headers, true).await?;
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    let (channel, validator, created) = storage::put(
        &state.pool,
        &actor,
        &app_id,
        &name,
        &input.release_id,
        if_match,
        if_none_match,
    )
    .await?;
    let mut response = (
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(channel),
    )
        .into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&validator).map_err(|_| {
            ApiError::from(app_depot_infrastructure::catalog::CatalogError::Invalid)
        })?,
    );
    if created {
        let segment: String = name
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    (byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect();
        let location = format!("/api/v1/apps/{app_id}/channels/{segment}");
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(&location).map_err(|_| {
                ApiError::from(app_depot_infrastructure::catalog::CatalogError::Invalid)
            })?,
        );
    }
    Ok(response)
}

async fn delete_channel(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((app_id, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize(&state, &headers, true).await?;
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    storage::delete(&state.pool, &actor, &app_id, &name, if_match).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn history(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path((app_id, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = authorize(&state, &headers, false).await?;
    let items = storage::history(&state.pool, &actor, &app_id, &name).await?;
    page(
        &items,
        &query,
        "channelmoves1",
        &format!("{app_id}:{name}:{}", actor.id),
        "",
    )
}

pub fn router(state: AuthState) -> Router {
    Router::new()
        .route("/apps/{app_id}/channels", get(list))
        .route(
            "/apps/{app_id}/channels/{name}",
            get(get_channel).put(put_channel).delete(delete_channel),
        )
        .route("/apps/{app_id}/channels/{name}/history", get(history))
        .with_state(state)
}
