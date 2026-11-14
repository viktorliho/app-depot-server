use crate::{
    auth::{authorize, reader, ApiError, AuthState},
    catalog::page,
};
use app_depot_domain::tree::validate_path;
use app_depot_infrastructure::{
    catalog::{etag, CatalogError},
    declarations as storage,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{HashMap, HashSet};

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

// This is a read-only assessment of the optional convention. Declaration PUT remains
// lossless for arbitrary JSON, including malformed or future convention values.
async fn validate_entry_points(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let principal = reader(&state, &headers).await?;
    if !input
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key("data"))
    {
        return Err(CatalogError::Invalid.into());
    }
    storage::get(&state.pool, principal.as_ref(), "Association", &id).await?;
    let artifact = sqlx::query("SELECT f.id, f.kind FROM associations x JOIN artifacts f ON f.id=x.artifact_id WHERE x.id=?")
        .bind(&id).fetch_optional(&state.pool).await.map_err(CatalogError::from)?
        .ok_or(CatalogError::NotFound)?;
    let artifact_id: String = artifact.get("id");
    let kind: String = artifact.get("kind");
    let block = input.get("data").and_then(|data| data.get("appDepot"));
    if block.is_some_and(|value| !value.is_object()) {
        return Ok(Json(json!({"status":"Unsupported","items":[]})));
    }
    let entries = block.and_then(|value| value.get("entryPoints"));
    let Some(entries) = entries else {
        return Ok(Json(json!({"status":"Absent","items":[]})));
    };
    let Some(entries) = entries.as_array() else {
        return Ok(Json(json!({"status":"Unsupported","items":[]})));
    };
    if entries.len() > 100 {
        return Ok(Json(json!({"status":"Unsupported","items":[]})));
    }
    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let id_value = entry.get("id").and_then(Value::as_str).unwrap_or("");
        let label_ok = entry.get("label").is_none_or(|value| {
            value.as_str().is_some_and(|s| {
                !s.is_empty() && s.chars().count() <= 160 && !s.chars().any(char::is_control)
            })
        });
        let platforms_ok = entry.get("platforms").is_none_or(|value| {
            value.as_array().is_some_and(|items| {
                let mut names = HashSet::new();
                items.len() <= 32
                    && items.iter().all(|item| {
                        item.as_str().is_some_and(|s| {
                            !s.is_empty()
                                && s.chars().count() <= 80
                                && !s.chars().any(char::is_control)
                                && names.insert(s)
                        })
                    })
            })
        });
        let mut status = "Valid";
        if id_value.is_empty()
            || id_value.len() > 80
            || !id_value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !seen.insert(id_value.to_owned())
            || !label_ok
            || !platforms_ok
        {
            status = "Invalid";
        } else if let Some(target) = entry.get("target") {
            match target.get("type").and_then(Value::as_str) {
                Some("ArtifactFile") if kind == "File" => {}
                Some("TreeFile") if kind == "Tree" => {
                    if let Some(path) = target.get("path").and_then(Value::as_str) {
                        if validate_path(path).is_err() {
                            status = "Invalid";
                        } else {
                            let found: Option<String> = sqlx::query_scalar("SELECT entry_type FROM artifact_entries WHERE artifact_id=? AND path=?")
                                .bind(&artifact_id).bind(path).fetch_optional(&state.pool).await.map_err(CatalogError::from)?;
                            status = match found.as_deref() {
                                Some("File") => "Valid",
                                Some("Directory") => "NotFile",
                                _ => "Missing",
                            };
                        }
                    } else {
                        status = "Invalid";
                    }
                }
                Some("ArtifactFile" | "TreeFile") => status = "Invalid",
                _ => status = "Unsupported",
            }
        } else {
            status = "Invalid";
        }
        results.push(json!({"index":index,"id":id_value,"status":status}));
    }
    let status = if results.iter().all(|item| item["status"] == "Valid") {
        "Valid"
    } else {
        "Issues"
    };
    Ok(Json(json!({"status":status,"items":results})))
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
            "/associations/{association_id}/entry-points/validate",
            post(validate_entry_points),
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
