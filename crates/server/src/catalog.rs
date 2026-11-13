use crate::auth::{authorize, reader, ApiError, AuthState};
use app_depot_application::upload::UploadCoordinator;
use app_depot_infrastructure::catalog as storage;
use app_depot_infrastructure::upload::UploadAdapter;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::{ops::Deref, sync::Arc};

#[derive(Clone)]
pub struct CatalogState {
    pub auth: AuthState,
    pub coordinator: Arc<UploadCoordinator<UploadAdapter>>,
}

impl Deref for CatalogState {
    type Target = AuthState;
    fn deref(&self) -> &Self::Target {
        &self.auth
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppCreate {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppPatch {
    name: Option<String>,
    slug: Option<String>,
    visibility: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseCreate {
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssociationCreate {
    artifact_id: String,
}

fn key(headers: &HeaderMap) -> Result<&str, ApiError> {
    let value = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::from(storage::CatalogError::Invalid))?;
    if value.is_empty() || value.len() > 255 {
        return Err(storage::CatalogError::Invalid.into());
    }
    Ok(value)
}

fn json_response(
    status: StatusCode,
    location: Option<String>,
    etag: String,
    body: impl serde::Serialize,
) -> Result<Response, ApiError> {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| ApiError::from(storage::CatalogError::Invalid))?,
    );
    if let Some(location) = location {
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(&location)
                .map_err(|_| ApiError::from(storage::CatalogError::Invalid))?,
        );
    }
    Ok(response)
}

pub(crate) fn page<T: serde::Serialize>(
    items: &[T],
    query: &HashMap<String, String>,
    kind: &str,
    scope: &str,
    filter: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let size = query
        .get("pageSize")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| storage::CatalogError::Invalid)?
        .unwrap_or(100);
    if !(1..=1000).contains(&size) {
        return Err(storage::CatalogError::Invalid.into());
    }
    let binding = format!(
        "{:x}",
        Sha256::digest(format!("{scope}\0{filter}").as_bytes())
    );
    let offset = if let Some(cursor) = query.get("cursor") {
        let parts: Vec<&str> = cursor.split('.').collect();
        if parts.len() != 4
            || parts[0] != kind
            || parts[1] != binding
            || parts[3] != size.to_string()
        {
            return Err(storage::CatalogError::InvalidCursor.into());
        }
        let offset: usize = parts[2]
            .parse()
            .map_err(|_| storage::CatalogError::InvalidCursor)?;
        if offset == 0 || offset >= items.len() {
            return Err(storage::CatalogError::InvalidCursor.into());
        }
        offset
    } else {
        0
    };
    let end = offset.saturating_add(size).min(items.len());
    let next = (end < items.len()).then(|| format!("{kind}.{binding}.{end}.{size}"));
    Ok(Json(
        json!({"items": &items[offset..end], "nextCursor": next}),
    ))
}

async fn list_apps(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = reader(&state, &headers).await?;
    let search = query.get("q").map(String::as_str).unwrap_or("");
    let items = storage::list_apps(&state.pool, principal.as_ref(), search).await?;
    let scope = principal
        .as_ref()
        .map(|actor| actor.id.as_str())
        .unwrap_or("anonymous");
    page(&items, &query, "apps1", scope, search)
}

async fn list_releases(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = reader(&state, &headers).await?;
    let items = storage::list_releases(&state.pool, principal.as_ref(), &app_id).await?;
    let scope = format!(
        "{}:{}",
        app_id,
        principal
            .as_ref()
            .map(|actor| actor.id.as_str())
            .unwrap_or("anonymous")
    );
    page(&items, &query, "releases1", &scope, "")
}

async fn create_app(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Json(input): Json<AppCreate>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let app = storage::create_app(&state.pool, &principal, key(&headers)?, &input.name).await?;
    json_response(
        StatusCode::CREATED,
        Some(format!("/api/v1/apps/{}", app.id)),
        storage::etag("app", &app.id, 1),
        app,
    )
}

async fn get_app(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let (app, version) = storage::get_app(&state.pool, principal.as_ref(), &id).await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("app", &app.id, version),
        app,
    )
}

async fn patch_app(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<AppPatch>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let (app, version) = storage::patch_app(
        &state.pool,
        &principal,
        &id,
        validator,
        input.name.as_deref(),
        input.slug.as_deref(),
        input.visibility.as_deref(),
    )
    .await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("app", &id, version),
        app,
    )
}

async fn create_release(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Json(input): Json<ReleaseCreate>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let release = storage::create_release(
        &state.pool,
        &principal,
        key(&headers)?,
        &app_id,
        input.version.as_deref(),
    )
    .await?;
    json_response(
        StatusCode::CREATED,
        Some(format!("/api/v1/releases/{}", release.id)),
        storage::etag("release", &release.id, 1),
        release,
    )
}

async fn get_release(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let (release, version) = storage::get_release(&state.pool, principal.as_ref(), &id).await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("release", &release.id, version),
        release,
    )
}

async fn patch_release(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let object = input.as_object().ok_or(storage::CatalogError::Invalid)?;
    if object.keys().any(|key| key != "version") {
        return Err(storage::CatalogError::Invalid.into());
    }
    let version_patch = match object.get("version") {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(value)) => Some(Some(value.as_str())),
        _ => return Err(storage::CatalogError::Invalid.into()),
    };
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let (release, version) =
        storage::patch_release(&state.pool, &principal, &id, validator, version_patch).await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("release", &id, version),
        release,
    )
}

async fn publish_release(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
    let (release, version) =
        storage::publish_release(&state.pool, &principal, key(&headers)?, &id, validator).await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("release", &release.id, version),
        release,
    )
}

async fn list_associations(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(release_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let items = storage::list_associations(&state.pool, principal.as_ref(), &release_id).await?;
    let scope = format!(
        "{}:{}",
        release_id,
        principal
            .as_ref()
            .map(|actor| actor.id.as_str())
            .unwrap_or("anonymous")
    );
    Ok(page(&items, &query, "associations1", &scope, "")?.into_response())
}

async fn create_association(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(release_id): Path<String>,
    Json(input): Json<AssociationCreate>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let association = storage::create_association(
        &state.pool,
        &principal,
        key(&headers)?,
        &release_id,
        &input.artifact_id,
    )
    .await?;
    json_response(
        StatusCode::CREATED,
        Some(format!("/api/v1/associations/{}", association.id)),
        storage::etag("association", &association.id, 1),
        association,
    )
}

async fn get_association(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state, &headers).await?;
    let (association, version) =
        storage::get_association(&state.pool, principal.as_ref(), &id).await?;
    json_response(
        StatusCode::OK,
        None,
        storage::etag("association", &id, version),
        association,
    )
}

async fn delete_association(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let operation_id = state
        .coordinator
        .serialize_catalog_mutation(storage::delete_association(
            &state.pool,
            &principal,
            key(&headers)?,
            &id,
            validator,
        ))
        .await?;
    state.trigger_cleanup();
    deletion_response(&state, &operation_id, "DeleteAssociation").await
}

async fn deletion_response(
    state: &CatalogState,
    operation_id: &str,
    kind: &str,
) -> Result<Response, ApiError> {
    let row = sqlx::query("SELECT status, physical_cleanup FROM deletion_operations WHERE id=?")
        .bind(operation_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(storage::CatalogError::Sql)?;
    let (status, physical) = if let Some(row) = row {
        (
            sqlx::Row::try_get::<String, _>(&row, "status").map_err(storage::CatalogError::Sql)?,
            sqlx::Row::try_get::<String, _>(&row, "physical_cleanup")
                .map_err(storage::CatalogError::Sql)?,
        )
    } else {
        ("Completed".to_owned(), "Completed".to_owned())
    };
    let mut response = (
        StatusCode::ACCEPTED,
        Json(json!({"id":operation_id,"kind":kind,"status":status,"accessRevoked":true,"physicalCleanup":physical})),
    )
        .into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/operations/{operation_id}"))
            .map_err(|_| ApiError::from(storage::CatalogError::Invalid))?,
    );
    Ok(response)
}

async fn delete_app(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let operation_id = state
        .coordinator
        .serialize_catalog_mutation(storage::delete_app(
            &state.pool,
            &principal,
            key(&headers)?,
            &id,
            validator,
        ))
        .await?;
    state.trigger_cleanup();
    deletion_response(&state, &operation_id, "DeleteApp").await
}

async fn delete_release(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let operation_id = state
        .coordinator
        .serialize_catalog_mutation(storage::delete_release(
            &state.pool,
            &principal,
            key(&headers)?,
            &id,
            validator,
        ))
        .await?;
    state.trigger_cleanup();
    deletion_response(&state, &operation_id, "DeleteRelease").await
}

pub fn router(state: CatalogState) -> Router {
    Router::new()
        .route("/apps", get(list_apps).post(create_app))
        .route(
            "/apps/{app_id}",
            get(get_app).patch(patch_app).delete(delete_app),
        )
        .route(
            "/apps/{app_id}/releases",
            get(list_releases).post(create_release),
        )
        .route(
            "/releases/{release_id}",
            get(get_release).patch(patch_release).delete(delete_release),
        )
        .route("/releases/{release_id}/publish", post(publish_release))
        .route(
            "/releases/{release_id}/artifacts",
            get(list_associations).post(create_association),
        )
        .route(
            "/associations/{association_id}",
            get(get_association).delete(delete_association),
        )
        .with_state(state)
}
