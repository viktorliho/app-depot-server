use crate::auth::{authorize, ApiError, AuthState};
use app_depot_application::upload::{
    CancelOutcome, Interval, InventoryBatch, InventoryEntry, UploadCoordinator, UploadCreate,
    UploadError,
};
use app_depot_infrastructure::upload::UploadAdapter;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures_util::TryStreamExt;
use sqlx::Row;
use std::{collections::HashMap, io, path::PathBuf, sync::Arc};
use tokio_util::io::StreamReader;

#[derive(Clone)]
pub struct UploadApiState {
    pub auth: AuthState,
    pub coordinator: Arc<UploadCoordinator<UploadAdapter>>,
    pub root: Arc<PathBuf>,
}

fn key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .ok_or_else(|| UploadError::Invalid.into())
}

fn content_range(headers: &HeaderMap, expected_total: u64) -> Result<Interval, ApiError> {
    let raw = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or(UploadError::InvalidRange)?;
    let value = raw
        .strip_prefix("bytes ")
        .ok_or(UploadError::InvalidRange)?;
    let (bounds, total) = value.split_once('/').ok_or(UploadError::InvalidRange)?;
    let (start, last) = bounds.split_once('-').ok_or(UploadError::InvalidRange)?;
    let start: u64 = start.parse().map_err(|_| UploadError::InvalidRange)?;
    let last: u64 = last.parse().map_err(|_| UploadError::InvalidRange)?;
    let total: u64 = total.parse().map_err(|_| UploadError::InvalidRange)?;
    if total != expected_total || start > last || last >= total {
        return Err(UploadError::InvalidRange.into());
    }
    Ok(Interval {
        start,
        end: last + 1,
    })
}

async fn create(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Json(input): Json<UploadCreate>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    let session = state
        .coordinator
        .create(&principal, key(&headers)?, &input)
        .await?;
    let mut response = (StatusCode::CREATED, Json(session.clone())).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/uploads/{}", session.id))
            .map_err(|_| UploadError::Invalid)?,
    );
    Ok(response)
}

async fn get_upload(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<app_depot_application::upload::UploadSession>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    Ok(Json(state.coordinator.get(&principal, &id).await?))
}

async fn get_inventory(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<app_depot_application::upload::InventoryPage>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    let page_size = query
        .get("pageSize")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| UploadError::Invalid)?;
    Ok(Json(
        state
            .coordinator
            .inventory_page(
                &principal,
                &id,
                page_size,
                query.get("cursor").map(String::as_str),
            )
            .await?,
    ))
}

async fn append_inventory(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(batch): Json<InventoryBatch>,
) -> Result<Json<app_depot_application::upload::InventoryBatchResult>, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    Ok(Json(
        state
            .coordinator
            .append_inventory(&principal, &id, key(&headers)?, &batch)
            .await?,
    ))
}

async fn complete_inventory(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<app_depot_application::upload::UploadSession>, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    Ok(Json(
        state
            .coordinator
            .complete_inventory(&principal, &id, key(&headers)?)
            .await?,
    ))
}

async fn receive(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path((id, file_id)): Path<(String, String)>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    let session = state.coordinator.get(&principal, &id).await?;
    let size = if session.mode == "BareTree" {
        state
            .coordinator
            .inventory(&principal, &id)
            .await?
            .iter()
            .find_map(|entry| match entry {
                InventoryEntry::File {
                    file_id: candidate,
                    size,
                    ..
                } if candidate == &file_id => Some(*size),
                _ => None,
            })
            .ok_or(UploadError::NotFound)?
    } else {
        if session.file().file_id != file_id {
            return Err(UploadError::NotFound.into());
        }
        session.file().size
    };
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(UploadError::Invalid.into());
    }
    let range = content_range(&headers, size)?;
    let stream = body.into_data_stream().map_err(io::Error::other);
    let mut reader = StreamReader::new(stream);
    state
        .coordinator
        .receive(&principal, &id, &file_id, range, &mut reader)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn progress(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path((id, file_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<app_depot_application::upload::UploadProgress>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    let page_size = query
        .get("pageSize")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| UploadError::Invalid)?;
    Ok(Json(
        state
            .coordinator
            .progress_page(
                &principal,
                &id,
                &file_id,
                page_size,
                query.get("cursor").map(String::as_str),
            )
            .await?,
    ))
}

async fn finalize(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<app_depot_application::upload::FinalizationResult>, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    Ok(Json(
        state
            .coordinator
            .finalize(&principal, &id, key(&headers)?)
            .await?,
    ))
}

async fn cancel(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, true).await?;
    match state
        .coordinator
        .cancel(&principal, &id, key(&headers)?)
        .await?
    {
        CancelOutcome::Completed(result) => Ok(Json(result).into_response()),
        CancelOutcome::Accepted {
            operation_id,
            status,
        } => {
            let mut response = (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({"id": operation_id, "kind": "CancelUpload", "status": status})),
            ).into_response();
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&format!("/api/v1/operations/{operation_id}"))
                    .map_err(|_| UploadError::Invalid)?,
            );
            Ok(response)
        }
    }
}

async fn result(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    if let Some(result) = state.coordinator.result(&principal, &id).await? {
        return Ok(Json(result).into_response());
    }
    let session = state.coordinator.get(&principal, &id).await?;
    let status = if session.phase == "Receiving" {
        "Accepted"
    } else {
        "Running"
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"id": session.operation_id, "kind":"Finalize", "status":status})),
    )
        .into_response())
}

async fn operation(
    State(state): State<UploadApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = authorize(&state.auth, &headers, false).await?;
    let is_backup: i64 = sqlx::query_scalar("SELECT count(*) FROM backup_operations WHERE id=?")
        .bind(&id)
        .fetch_one(&state.auth.pool)
        .await
        .map_err(|error| UploadError::Retryable(error.to_string()))?;
    if is_backup != 0 {
        let operation = app_depot_infrastructure::backups::operation(
            &state.auth.pool,
            &state.root,
            &principal,
            &id,
        )
        .await?;
        return Ok(Json(
            serde_json::to_value(operation).map_err(|_| ApiError::server())?,
        ));
    }
    if let Some(deletion) = sqlx::query(
        "SELECT owner_id, kind, status, physical_cleanup FROM deletion_operations WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|error| UploadError::Retryable(error.to_string()))?
    {
        let owner: String = deletion.get("owner_id");
        if !principal.may_manage(&owner) {
            return Err(UploadError::NotFound.into());
        }
        return Ok(Json(
            serde_json::json!({"id":id,"kind":deletion.get::<String, _>("kind"),
            "status":deletion.get::<String, _>("status"),"accessRevoked":true,
            "physicalCleanup":deletion.get::<String, _>("physical_cleanup")}),
        ));
    }
    let row = sqlx::query(
        "SELECT owner_id, kind, status, result_json, problem_json FROM operations WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|error| UploadError::Retryable(error.to_string()))?;
    let Some(row) = row else {
        let deleted =
            sqlx::query("SELECT owner_id, status FROM association_delete_operations WHERE id=?")
                .bind(&id)
                .fetch_optional(&state.auth.pool)
                .await
                .map_err(|error| UploadError::Retryable(error.to_string()))?
                .ok_or(UploadError::NotFound)?;
        let owner: String = deleted.get("owner_id");
        if !principal.may_manage(&owner) {
            return Err(UploadError::NotFound.into());
        }
        return Ok(Json(
            serde_json::json!({"id":id,"kind":"DeleteAssociation","status":deleted.get::<String, _>("status")}),
        ));
    };
    let owner: String = row.get("owner_id");
    if !principal.may_manage(&owner) {
        return Err(UploadError::NotFound.into());
    }
    let result: Option<String> = row.get("result_json");
    let problem: Option<String> = row.get("problem_json");
    let mut output = serde_json::json!({"id":id,"kind":row.get::<String, _>("kind"),"status":row.get::<String, _>("status")});
    if let Some(result) = result {
        output["result"] = serde_json::from_str(&result)
            .map_err(|error| UploadError::Retryable(error.to_string()))?;
    }
    if let Some(problem) = problem {
        output["problem"] = serde_json::from_str(&problem)
            .map_err(|error| UploadError::Retryable(error.to_string()))?;
    }
    Ok(Json(output))
}

pub fn router(state: UploadApiState) -> Router {
    Router::new()
        .route("/uploads", post(create))
        .route("/uploads/{session_id}", get(get_upload).delete(cancel))
        .route(
            "/uploads/{session_id}/inventory",
            get(get_inventory).post(append_inventory),
        )
        .route(
            "/uploads/{session_id}/inventory/complete",
            post(complete_inventory),
        )
        .route("/uploads/{session_id}/files/{file_id}", put(receive))
        .route(
            "/uploads/{session_id}/files/{file_id}/progress",
            get(progress),
        )
        .route("/uploads/{session_id}/finalize", post(finalize))
        .route("/uploads/{session_id}/result", get(result))
        .route(
            "/operations/{operation_id}",
            get(operation).layer(middleware::from_fn(
                |request: axum::extract::Request, next: Next| async move {
                    let mut response = next.run(request).await;
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                    response
                },
            )),
        )
        .with_state(state)
}
