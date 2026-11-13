use crate::auth::{reader, ApiError, AuthState};
use app_depot_domain::{FileArtifactId, TreeArtifactId};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use sqlx::Row;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct DownloadState {
    pub auth: AuthState,
    pub root: Arc<PathBuf>,
}

fn problem(status: StatusCode, code: &str, title: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "application/problem+json")], Json(json!({"type":format!("urn:app-depot:problem:{code}"),"title":title,"status":status.as_u16(),"code":code}))).into_response()
}

fn range(raw: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = raw.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if value.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = value.split_once('-') else {
        return Ok(None);
    };
    if size == 0 {
        return Err(());
    }
    if first.is_empty() {
        let suffix: u64 = last.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(Some((size.saturating_sub(suffix), size)));
    }
    let start: u64 = first.parse().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if last.is_empty() {
        size
    } else {
        let inclusive: u64 = last.parse().map_err(|_| ())?;
        if inclusive < start {
            return Err(());
        }
        inclusive.saturating_add(1).min(size)
    };
    Ok(Some((start, end)))
}

async fn content(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    let parsed = FileArtifactId::parse(&id)
        .ok_or_else(|| ApiError::from(app_depot_application::upload::UploadError::NotFound))?;
    let digest = parsed.as_str().strip_prefix("adfile:sha256:").unwrap();
    let row = sqlx::query("SELECT c.size FROM artifacts f JOIN cas_objects c ON c.digest=f.digest JOIN associations x ON x.artifact_id=f.id JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE f.id=? AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND r.state='Published')) LIMIT 1")
        .bind(&id).bind(principal.as_ref().map(|actor| actor.id.as_str()).unwrap_or("")).bind(principal.as_ref().map(|actor| actor.server_admin as i64).unwrap_or(0)).fetch_optional(&state.auth.pool).await
        .map_err(|_| ApiError::from(app_depot_application::upload::UploadError::Retryable("database unavailable".into())))?
        .ok_or_else(|| ApiError::from(app_depot_application::upload::UploadError::NotFound))?;
    let size = row.get::<i64, _>("size") as u64;
    serve_blob(&state.root, digest, size, &headers).await
}

async fn artifact(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    if FileArtifactId::parse(&id).is_none() && TreeArtifactId::parse(&id).is_none() {
        return Err(app_depot_application::upload::UploadError::NotFound.into());
    }
    let row = sqlx::query("SELECT f.kind, f.size FROM artifacts f JOIN associations x ON x.artifact_id=f.id JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE f.id=? AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND r.state='Published')) LIMIT 1")
        .bind(&id).bind(principal.as_ref().map(|actor| actor.id.as_str()).unwrap_or("")).bind(principal.as_ref().map(|actor| actor.server_admin as i64).unwrap_or(0)).fetch_optional(&state.auth.pool).await
        .map_err(|error| app_depot_application::upload::UploadError::Retryable(error.to_string()))?
        .ok_or(app_depot_application::upload::UploadError::NotFound)?;
    let kind: String = row.get("kind");
    let mut response =
        Json(json!({"id":id,"kind":kind,"size":row.get::<i64, _>("size") as u64})).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"artifact-{id}\"")).unwrap(),
    );
    Ok(response)
}

async fn inventory(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    if TreeArtifactId::parse(&id).is_none() {
        return Err(app_depot_application::upload::UploadError::NotFound.into());
    }
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts f JOIN associations x ON x.artifact_id=f.id JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE f.id=? AND f.kind='Tree' AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND r.state='Published'))")
        .bind(&id).bind(principal.as_ref().map(|actor| actor.id.as_str()).unwrap_or("")).bind(principal.as_ref().map(|actor| actor.server_admin as i64).unwrap_or(0)).fetch_one(&state.auth.pool).await
        .map_err(|error| app_depot_application::upload::UploadError::Retryable(error.to_string()))?;
    if visible == 0 {
        return Err(app_depot_application::upload::UploadError::NotFound.into());
    }
    let size = query
        .get("pageSize")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| app_depot_application::upload::UploadError::Invalid)?
        .unwrap_or(100);
    if !(1..=1000).contains(&size) {
        return Err(app_depot_application::upload::UploadError::Invalid.into());
    }
    let rows = sqlx::query("SELECT path, entry_type, file_id, size, sha256, executable FROM artifact_entries WHERE artifact_id=? ORDER BY path COLLATE BINARY")
        .bind(&id).fetch_all(&state.auth.pool).await.map_err(|error| app_depot_application::upload::UploadError::Retryable(error.to_string()))?;
    let offset = if let Some(cursor) = query.get("cursor") {
        let parts: Vec<&str> = cursor.split('.').collect();
        if parts.len() != 5
            || parts[0] != "a1"
            || parts[1] != id
            || parts[2]
                != principal
                    .as_ref()
                    .map(|actor| actor.id.as_str())
                    .unwrap_or("anonymous")
            || parts[4] != size.to_string()
        {
            return Err(app_depot_application::upload::UploadError::InvalidCursor.into());
        }
        let offset: usize = parts[3]
            .parse()
            .map_err(|_| app_depot_application::upload::UploadError::InvalidCursor)?;
        if offset == 0 || offset >= rows.len() {
            return Err(app_depot_application::upload::UploadError::InvalidCursor.into());
        }
        offset
    } else {
        0
    };
    let end = offset.saturating_add(size).min(rows.len());
    let mut items = Vec::new();
    for row in &rows[offset..end] {
        let path: String = row.get("path");
        if row.get::<String, _>("entry_type") == "Directory" {
            items.push(json!({"type":"Directory","path":path}));
        } else {
            items.push(json!({"type":"File","path":path,
                "fileId":row.get::<Option<String>, _>("file_id").ok_or_else(|| app_depot_application::upload::UploadError::Retryable("artifact file ID missing".into()))?,
                "size":row.get::<Option<i64>, _>("size").ok_or_else(|| app_depot_application::upload::UploadError::Retryable("artifact size missing".into()))? as u64,
                "sha256":row.get::<Option<String>, _>("sha256").ok_or_else(|| app_depot_application::upload::UploadError::Retryable("artifact digest missing".into()))?,
                "executable":row.get::<Option<i64>, _>("executable").ok_or_else(|| app_depot_application::upload::UploadError::Retryable("artifact executable missing".into()))? != 0}));
        }
    }
    let next_cursor = (end < rows.len()).then(|| {
        format!(
            "a1.{id}.{}.{end}.{size}",
            principal
                .as_ref()
                .map(|actor| actor.id.as_str())
                .unwrap_or("anonymous")
        )
    });
    let mut response = Json(json!({"items":items,"nextCursor":next_cursor})).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"artifact-{id}\"")).unwrap(),
    );
    Ok(response)
}

async fn tree_file(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path((id, file_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let principal = reader(&state.auth, &headers).await?;
    if TreeArtifactId::parse(&id).is_none() {
        return Err(app_depot_application::upload::UploadError::NotFound.into());
    }
    let row = sqlx::query("SELECT e.sha256, e.size FROM artifact_entries e JOIN artifacts f ON f.id=e.artifact_id JOIN associations x ON x.artifact_id=f.id JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE e.artifact_id=? AND e.file_id=? AND e.entry_type='File' AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND r.state='Published')) LIMIT 1")
        .bind(&id).bind(&file_id).bind(principal.as_ref().map(|actor| actor.id.as_str()).unwrap_or("")).bind(principal.as_ref().map(|actor| actor.server_admin as i64).unwrap_or(0))
        .fetch_optional(&state.auth.pool).await.map_err(|error| app_depot_application::upload::UploadError::Retryable(error.to_string()))?
        .ok_or(app_depot_application::upload::UploadError::NotFound)?;
    let digest: String = row.get::<Option<String>, _>("sha256").ok_or_else(|| {
        app_depot_application::upload::UploadError::Retryable("artifact digest missing".into())
    })?;
    let size = row.get::<Option<i64>, _>("size").ok_or_else(|| {
        app_depot_application::upload::UploadError::Retryable("artifact size missing".into())
    })? as u64;
    serve_blob(&state.root, &digest, size, &headers).await
}

async fn serve_blob(
    root: &PathBuf,
    digest: &str,
    size: u64,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let path = root.join("cas").join(&digest[..2]).join(digest);
    let checked_path = path.clone();
    let expected = digest.to_owned();
    let valid = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = std::fs::File::open(checked_path)?;
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 65536];
        let mut length = 0u64;
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            hash.update(&bytes[..count]);
        }
        Ok(length == size && format!("{:x}", hash.finalize()) == expected)
    })
    .await
    .map_err(|_| ApiError::server())?
    .map_err(|_| ApiError::server())?;
    if !valid {
        return Err(ApiError::server());
    }
    serve_file(path, size, format!("\"adfile-{digest}\""), headers).await
}

pub(crate) async fn serve_file(
    path: PathBuf,
    size: u64,
    etag: String,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let meta = path.symlink_metadata().map_err(|_| {
        ApiError::from(app_depot_application::upload::UploadError::Retryable(
            "CAS unavailable".into(),
        ))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != size {
        return Err(app_depot_application::upload::UploadError::Retryable(
            "CAS identity invalid".into(),
        )
        .into());
    }
    let use_range = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v == etag);
    let selection = if use_range {
        headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|v| range(v, size))
            .transpose()
    } else {
        Ok(None)
    };
    let selected = match selection {
        Ok(value) => value.flatten(),
        Err(()) => {
            let mut response = problem(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                "Range not satisfiable",
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).unwrap(),
            );
            return Ok(response);
        }
    };
    let (start, end, status) = selected
        .map(|(a, b)| (a, b, StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, size, StatusCode::OK));
    let mut file = tokio::fs::File::open(path).await.map_err(|_| {
        ApiError::from(app_depot_application::upload::UploadError::Retryable(
            "CAS unavailable".into(),
        ))
    })?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| {
            ApiError::from(app_depot_application::upload::UploadError::Retryable(
                "CAS unavailable".into(),
            ))
        })?;
    let body = Body::from_stream(ReaderStream::new(file.take(end - start)));
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(end - start).to_string()).unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1)).unwrap(),
        );
    }
    Ok(response)
}

async fn capabilities() -> Json<serde_json::Value> {
    Json(
        json!({"apiVersion":"1.0.0","ingestionModes":["BareFile","BareTree","ZipImport"],"zipMethods":["Store","Deflate"],"treeIdentity":"adtree:v1:sha256","fileIdentity":"adfile:sha256","uploadRanges":true,"downloadRanges":true,
        "limits":{"maxPageSize":1000,"maxConcurrentRanges":8,"maxUploadBytes":1125899906842624u64},"features":[]}),
    )
}

pub fn router(state: DownloadState) -> Router {
    Router::new()
        .route("/capabilities", get(capabilities))
        .route("/artifacts/{artifact_id}", get(artifact))
        .route("/artifacts/{artifact_id}/content", get(content))
        .route("/artifacts/{artifact_id}/inventory", get(inventory))
        .route("/artifacts/{artifact_id}/files/{file_id}", get(tree_file))
        .with_state(state)
}
