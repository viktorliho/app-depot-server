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
    let artifact = sqlx::query("SELECT f.id, f.kind, x.release_id FROM associations x JOIN artifacts f ON f.id=x.artifact_id WHERE x.id=?")
        .bind(&id).fetch_optional(&state.pool).await.map_err(CatalogError::from)?
        .ok_or(CatalogError::NotFound)?;
    let artifact_id: String = artifact.get("id");
    let kind: String = artifact.get("kind");
    let release_id: String = artifact.get("release_id");
    let (release_document, _) =
        storage::get(&state.pool, principal.as_ref(), "Release", &release_id).await?;
    let release_platforms = release_document
        .data
        .get("appDepot")
        .and_then(|block| block.get("platforms"));
    let block = input.get("data").and_then(|data| data.get("appDepot"));
    if block.is_some_and(|value| !value.is_object()) {
        return Ok(Json(
            json!({"status":"Unsupported","launchStatus":"Unsupported","selectionRequiredFor":[],"items":[]}),
        ));
    }
    let entries = block.and_then(|value| value.get("entryPoints"));
    let Some(entries) = entries else {
        return Ok(Json(
            json!({"status":"Absent","launchStatus":"Absent","selectionRequiredFor":[],"items":[]}),
        ));
    };
    let Some(entries) = entries.as_array() else {
        return Ok(Json(
            json!({"status":"Unsupported","launchStatus":"Unsupported","selectionRequiredFor":[],"items":[]}),
        ));
    };
    if entries.len() > 100 {
        return Ok(Json(
            json!({"status":"Unsupported","launchStatus":"Unsupported","selectionRequiredFor":[],"items":[]}),
        ));
    }
    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(entries.len());
    let mut launch_counts: HashMap<&'static str, usize> = HashMap::new();
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
        let launch = assess_launch(
            &state.pool,
            &artifact_id,
            &kind,
            entry,
            status,
            release_platforms,
        )
        .await?;
        for platform in &launch.platforms {
            *launch_counts.entry(platform).or_default() += 1;
        }
        results.push(json!({"index":index,"id":id_value,"status":status,
            "launchStatus":launch.status,"launchReason":launch.reason,
            "effectivePlatforms":launch.platforms}));
    }
    let status = if results.iter().all(|item| item["status"] == "Valid") {
        "Valid"
    } else {
        "Issues"
    };
    let launch_status = if results.iter().all(|item| item["launchStatus"] == "Absent") {
        "Absent"
    } else if results
        .iter()
        .all(|item| item["launchStatus"] == "Valid" || item["launchStatus"] == "Absent")
    {
        "Valid"
    } else if results
        .iter()
        .all(|item| item["launchStatus"] == "Unsupported" || item["launchStatus"] == "Absent")
    {
        "Unsupported"
    } else {
        "Issues"
    };
    let mut selection_required_for: Vec<_> = launch_counts
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name))
        .collect();
    selection_required_for.sort_unstable();
    Ok(Json(json!({"status":status,"launchStatus":launch_status,
        "selectionRequiredFor":selection_required_for,"items":results})))
}

const NATIVE_PLATFORMS: [&str; 6] = [
    "linux/x86_64",
    "linux/aarch64",
    "windows/x86_64",
    "windows/aarch64",
    "macos/x86_64",
    "macos/aarch64",
];

struct LaunchAssessment {
    status: &'static str,
    reason: Option<&'static str>,
    platforms: Vec<&'static str>,
}

impl LaunchAssessment {
    fn new(status: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            status,
            reason,
            platforms: Vec::new(),
        }
    }
}

async fn assess_launch(
    pool: &sqlx::SqlitePool,
    artifact_id: &str,
    kind: &str,
    entry: &Value,
    target_status: &str,
    release_platforms: Option<&Value>,
) -> Result<LaunchAssessment, ApiError> {
    let Some(launch) = entry.get("launch") else {
        return Ok(LaunchAssessment::new("Absent", None));
    };
    let Some(launch) = launch.as_object() else {
        return Ok(LaunchAssessment::new("Invalid", Some("MalformedLaunch")));
    };
    let Some(method) = launch.get("method").and_then(Value::as_str) else {
        return Ok(LaunchAssessment::new("Invalid", Some("MalformedMethod")));
    };
    if method != "NativePortableV1" {
        return Ok(LaunchAssessment::new("Unsupported", Some("UnknownMethod")));
    }
    if target_status != "Valid" {
        return Ok(LaunchAssessment::new(
            "TargetUnavailable",
            Some("TargetNotValid"),
        ));
    }
    let Some(entry_platforms) = entry.get("platforms").and_then(storage::platform_list) else {
        return Ok(LaunchAssessment::new(
            "Invalid",
            Some("MissingOrMalformedPlatforms"),
        ));
    };
    if entry_platforms.is_empty() {
        return Ok(LaunchAssessment::new(
            "Invalid",
            Some("MissingOrMalformedPlatforms"),
        ));
    }
    let release_platforms = match release_platforms {
        Some(value) => match storage::platform_list(value) {
            Some(items) => Some(items),
            None => {
                return Ok(LaunchAssessment::new(
                    "Invalid",
                    Some("MalformedReleasePlatforms"),
                ))
            }
        },
        None => None,
    };
    let platforms: Vec<_> = NATIVE_PLATFORMS
        .into_iter()
        .filter(|name| {
            entry_platforms.contains(name)
                && release_platforms
                    .as_ref()
                    .is_none_or(|items| items.is_empty() || items.contains(name))
        })
        .collect();
    if platforms.is_empty() {
        let known_entry = entry_platforms
            .iter()
            .any(|name| NATIVE_PLATFORMS.contains(name));
        let known_release = release_platforms.as_ref().is_none_or(|items| {
            items.is_empty() || items.iter().any(|name| NATIVE_PLATFORMS.contains(name))
        });
        return Ok(LaunchAssessment::new(
            if known_entry && known_release {
                "Invalid"
            } else {
                "Unsupported"
            },
            Some(if known_entry && known_release {
                "NoPlatformIntersection"
            } else {
                "UnknownPlatform"
            }),
        ));
    }
    if let Some(arguments) = launch.get("arguments") {
        let Some(arguments) = arguments.as_array() else {
            return Ok(LaunchAssessment::new("Invalid", Some("MalformedArguments")));
        };
        if arguments.len() > 64
            || arguments
                .iter()
                .map(|arg| arg.as_str().map(str::len))
                .collect::<Option<Vec<_>>>()
                .is_none_or(|lengths| {
                    lengths.iter().any(|len| *len > 4096) || lengths.iter().sum::<usize>() > 16384
                })
            || arguments.iter().any(|arg| {
                arg.as_str()
                    .is_some_and(|s| s.chars().any(char::is_control))
            })
        {
            return Ok(LaunchAssessment::new("Invalid", Some("MalformedArguments")));
        }
    }
    if let Some(directory) = launch.get("workingDirectory") {
        let Some(directory) = directory.as_str() else {
            return Ok(LaunchAssessment::new(
                "Invalid",
                Some("MalformedWorkingDirectory"),
            ));
        };
        if !directory.is_empty() {
            if kind != "Tree" || validate_path(directory).is_err() {
                return Ok(LaunchAssessment::new(
                    "Invalid",
                    Some("MalformedWorkingDirectory"),
                ));
            }
            let found: Option<String> = sqlx::query_scalar(
                "SELECT entry_type FROM artifact_entries WHERE artifact_id=? AND path=?",
            )
            .bind(artifact_id)
            .bind(directory)
            .fetch_optional(pool)
            .await
            .map_err(CatalogError::from)?;
            if found.as_deref() != Some("Directory") {
                return Ok(LaunchAssessment::new(
                    "Invalid",
                    Some("WorkingDirectoryNotFound"),
                ));
            }
        }
    }
    Ok(LaunchAssessment {
        status: "Valid",
        reason: None,
        platforms,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn portable_launch_platforms_arguments_and_methods() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let entry = json!({"id":"game","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64","linux/aarch64"],
            "launch":{"method":"NativePortableV1","arguments":["--windowed",""]}});
        let release = json!(["linux/x86_64"]);
        let result = assess_launch(&pool, "artifact", "File", &entry, "Valid", Some(&release))
            .await
            .ok()
            .unwrap();
        assert_eq!(result.status, "Valid");
        assert_eq!(result.platforms, ["linux/x86_64"]);
        let unknown = json!({"platforms":["linux/amd64"],"launch":{"method":"NativePortableV1"}});
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &unknown, "Valid", None)
                .await
                .ok()
                .unwrap()
                .reason,
            Some("UnknownPlatform")
        );
        let conflict = json!(["windows/x86_64"]);
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &entry, "Valid", Some(&conflict))
                .await
                .ok()
                .unwrap()
                .reason,
            Some("NoPlatformIntersection")
        );
        let legacy = json!({"target":{"type":"ArtifactFile"}});
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &legacy, "Valid", None)
                .await
                .ok()
                .unwrap()
                .status,
            "Absent"
        );
        let future = json!({"launch":{"method":"NativePortableV2","extension":42}});
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &future, "Valid", None)
                .await
                .ok()
                .unwrap()
                .status,
            "Unsupported"
        );
        let bad_args = json!({"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","arguments":["ok\nno"]}});
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &bad_args, "Valid", None)
                .await
                .ok()
                .unwrap()
                .reason,
            Some("MalformedArguments")
        );
        let file_dir = json!({"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"bin"}});
        assert_eq!(
            assess_launch(&pool, "artifact", "File", &file_dir, "Valid", None)
                .await
                .ok()
                .unwrap()
                .reason,
            Some("MalformedWorkingDirectory")
        );
    }

    #[tokio::test]
    async fn portable_tree_working_directory_requires_inventory_directory() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE artifact_entries(artifact_id TEXT,path TEXT,entry_type TEXT)")
            .execute(&pool)
            .await
            .ok()
            .unwrap();
        sqlx::query("INSERT INTO artifact_entries VALUES ('artifact','System64','Directory'),('artifact','System64/game','File')").execute(&pool).await.unwrap();
        let entry = json!({"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"System64"}});
        assert_eq!(
            assess_launch(&pool, "artifact", "Tree", &entry, "Valid", None)
                .await
                .ok()
                .unwrap()
                .status,
            "Valid"
        );
        for (directory, reason) in [
            ("System64/game", "WorkingDirectoryNotFound"),
            ("missing", "WorkingDirectoryNotFound"),
            ("../host", "MalformedWorkingDirectory"),
            ("/absolute", "MalformedWorkingDirectory"),
        ] {
            let entry = json!({"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":directory}});
            assert_eq!(
                assess_launch(&pool, "artifact", "Tree", &entry, "Valid", None)
                    .await
                    .ok()
                    .unwrap()
                    .reason,
                Some(reason)
            );
        }
    }
}
