use app_depot_application::upload::UploadError;
use app_depot_infrastructure::auth::{self, Principal};
use app_depot_infrastructure::catalog::CatalogError;
use axum::{
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    net::{IpAddr, SocketAddr},
    path::Path as FsPath,
    sync::Arc,
};
use tokio::sync::Notify;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct AuthState {
    pub pool: SqlitePool,
    signing_key: Arc<Vec<u8>>,
    cleanup_signal: Arc<Notify>,
}

impl AuthState {
    pub fn from_data_root(
        pool: SqlitePool,
        root: &FsPath,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let path = root.join("ops/session-key");
        if !path.exists() {
            let mut key = [0u8; 32];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut key);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            file.write_all(&key)?;
            file.sync_all()?;
            File::open(root.join("ops"))?.sync_all()?;
        }
        Self::new(pool, &path)
    }
    pub fn new(pool: SqlitePool, key_file: &FsPath) -> Result<Self, Box<dyn std::error::Error>> {
        let meta = key_file.symlink_metadata()?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("session key must be a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("session key file must be owner-only".into());
            }
        }
        let key = fs::read(key_file)?;
        Self::from_key(pool, key)
    }

    pub fn from_key(pool: SqlitePool, key: Vec<u8>) -> Result<Self, Box<dyn std::error::Error>> {
        if key.len() < 32 {
            return Err("session key must have at least 32 bytes".into());
        }
        Ok(Self {
            pool,
            signing_key: Arc::new(key),
            cleanup_signal: Arc::new(Notify::new()),
        })
    }

    pub fn trigger_cleanup(&self) {
        self.cleanup_signal.notify_one();
    }

    pub fn cleanup_signal(&self) -> Arc<Notify> {
        self.cleanup_signal.clone()
    }

    fn mac(&self, label: &str, value: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC accepts any key size");
        mac.update(label.as_bytes());
        mac.update(value.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn signed_cookie(&self, id: &str) -> String {
        format!("{id}.{}", self.mac("cookie:", id))
    }

    fn cookie_id(&self, headers: &HeaderMap) -> Option<String> {
        let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
        let value = cookie
            .split(';')
            .filter_map(|part| part.trim().strip_prefix("ad_session="))
            .next()?;
        let (id, signature) = value.split_once('.')?;
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let supplied = hex::decode(signature).ok()?;
        let mut mac = HmacSha256::new_from_slice(&self.signing_key).ok()?;
        mac.update(b"cookie:");
        mac.update(id.as_bytes());
        mac.verify_slice(&supplied).ok()?;
        Some(id.to_owned())
    }

    fn csrf(&self, id: &str) -> String {
        self.mac("csrf:", id)
    }
}

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    title: &'static str,
    cancellation_reason: Option<&'static str>,
}

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        let (status, code, title) = match error {
            CatalogError::Invalid => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid request",
            ),
            CatalogError::InvalidCursor => {
                (StatusCode::BAD_REQUEST, "invalid_cursor", "Invalid cursor")
            }
            CatalogError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Forbidden"),
            CatalogError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Not found"),
            CatalogError::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency key conflict",
            ),
            CatalogError::Conflict => {
                (StatusCode::CONFLICT, "invalid_request", "Resource conflict")
            }
            CatalogError::ImmutableRelease => (
                StatusCode::CONFLICT,
                "immutable_release",
                "Immutable release",
            ),
            CatalogError::InvalidChannelTarget => (
                StatusCode::CONFLICT,
                "invalid_channel_target",
                "Invalid channel target",
            ),
            CatalogError::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "precondition_required",
                "Precondition required",
            ),
            CatalogError::StaleEtag => {
                (StatusCode::PRECONDITION_FAILED, "stale_etag", "Stale ETag")
            }
            CatalogError::InvalidImage => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_image",
                "Invalid image",
            ),
            CatalogError::UnsupportedImage => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_image",
                "Unsupported image format",
            ),
            CatalogError::ImageTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                "Image exceeds artwork limits",
            ),
            CatalogError::Storage => (
                StatusCode::SERVICE_UNAVAILABLE,
                "retryable_failure",
                "Storage unavailable",
            ),
            CatalogError::Sql(_) | CatalogError::Json(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "retryable_failure",
                "Server error",
            ),
        };
        Self {
            status,
            code,
            title,
            cancellation_reason: None,
        }
    }
}

impl From<UploadError> for ApiError {
    fn from(error: UploadError) -> Self {
        let restore_invalidated = matches!(error, UploadError::RestoreInvalidated);
        let (status, code, title) = match error {
            UploadError::Invalid => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid request",
            ),
            UploadError::UnsupportedKind => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_artifact_kind",
                "Unsupported artifact kind",
            ),
            UploadError::InvalidZip => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_zip",
                "Invalid ZIP",
            ),
            UploadError::UnsupportedZipMethod => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_zip_method",
                "Unsupported ZIP method",
            ),
            UploadError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Not found"),
            UploadError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Forbidden"),
            UploadError::Incomplete => (
                StatusCode::CONFLICT,
                "incomplete_upload",
                "Incomplete upload",
            ),
            UploadError::ConflictingBytes => (
                StatusCode::CONFLICT,
                "conflicting_upload_bytes",
                "Conflicting upload bytes",
            ),
            UploadError::InvalidRange => (
                StatusCode::BAD_REQUEST,
                "invalid_upload_range",
                "Invalid upload range",
            ),
            UploadError::HashMismatch => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "hash_mismatch",
                "Hash mismatch",
            ),
            UploadError::ImmutableRelease => (
                StatusCode::CONFLICT,
                "immutable_release",
                "Immutable release",
            ),
            UploadError::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency key conflict",
            ),
            UploadError::InvalidCursor => {
                (StatusCode::BAD_REQUEST, "invalid_cursor", "Invalid cursor")
            }
            UploadError::ProgressChanged => (
                StatusCode::CONFLICT,
                "progress_changed",
                "Upload progress changed",
            ),
            UploadError::OperationRunning => (
                StatusCode::CONFLICT,
                "operation_running",
                "Operation running",
            ),
            UploadError::SessionCancelled => (
                StatusCode::GONE,
                "session_cancelled",
                "Upload session cancelled",
            ),
            UploadError::RestoreInvalidated => (
                StatusCode::GONE,
                "session_cancelled",
                "Upload session cancelled",
            ),
            UploadError::SessionExpired => (
                StatusCode::GONE,
                "session_expired",
                "Upload session expired",
            ),
            UploadError::Retryable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "retryable_failure",
                "Retryable failure",
            ),
        };
        Self {
            status,
            code,
            title,
            cancellation_reason: restore_invalidated.then_some("RestoreInvalidated"),
        }
    }
}

impl ApiError {
    fn unauthenticated() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthenticated",
            title: "Unauthenticated",
            cancellation_reason: None,
        }
    }
    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            title: "Forbidden",
            cancellation_reason: None,
        }
    }
    pub(crate) fn server() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "retryable_failure",
            title: "Server error",
            cancellation_reason: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"type": format!("urn:app-depot:problem:{}", self.code), "title": self.title, "status": self.status.as_u16(), "code": self.code});
        if let Some(reason) = self.cancellation_reason {
            body["cancellationReason"] = json!(reason);
        }
        (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(body),
        )
            .into_response()
    }
}

fn local_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    headers
        .get("x-app-depot-effective-origin")
        .and_then(|h| h.to_str().ok())
        == Some(origin)
}

pub async fn trusted_origin(
    State(peers): State<Arc<HashSet<IpAddr>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    request.headers_mut().remove("x-app-depot-effective-origin");
    if let Some(origin) = effective_origin(request.headers(), peer.ip(), &peers) {
        request
            .headers_mut()
            .insert("x-app-depot-effective-origin", origin);
    }
    next.run(request).await
}

fn effective_origin(
    headers: &HeaderMap,
    peer: IpAddr,
    trusted_proxy_peers: &HashSet<IpAddr>,
) -> Option<HeaderValue> {
    if trusted_proxy_peers.contains(&peer) {
        let proto = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok());
        let host = headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok());
        if proto == Some("https") {
            if let Some(host) = host.filter(|host| {
                !host.is_empty()
                    && host.len() <= 255
                    && host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".-:[]".contains(&c))
            }) {
                if let Ok(value) = HeaderValue::from_str(&format!("https://{host}")) {
                    return Some(value);
                }
            }
        }
    }
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok())?;
    if host.is_empty()
        || host.len() > 255
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-:[]".contains(&c))
    {
        return None;
    }
    HeaderValue::from_str(&format!("http://{host}")).ok()
}

fn secure_cookie(headers: &HeaderMap) -> bool {
    headers
        .get("x-app-depot-effective-origin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InitialSetup {
    token: String,
    username: String,
    password: String,
}

async fn setup_status(State(state): State<AuthState>) -> Result<Json<Value>, ApiError> {
    let required = auth::setup_required(&state.pool)
        .await
        .map_err(|_| ApiError::server())?;
    Ok(Json(json!({"required": required})))
}

async fn setup_admin(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(input): Json<InitialSetup>,
) -> Result<Response, ApiError> {
    if !local_origin(&headers) {
        return Err(ApiError::forbidden());
    }
    let created = auth::complete_setup(&state.pool, &input.token, &input.username, &input.password)
        .await
        .map_err(|_| ApiError::server())?;
    if !created {
        return Err(ApiError::forbidden());
    }
    Ok((StatusCode::CREATED, Json(json!({"created": true}))).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserCreate {
    username: String,
    password: String,
    publish_permission: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserPatch {
    publish_permission: Option<bool>,
    server_admin: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsPatch {
    anonymous_read_enabled: Option<bool>,
}

fn required_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(CatalogError::Invalid)?;
    if key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid.into());
    }
    Ok(key)
}

fn with_etag(
    status: StatusCode,
    value: impl Serialize,
    etag: &str,
    location: Option<&str>,
) -> Result<Response, ApiError> {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(etag).map_err(|_| ApiError::server())?,
    );
    if let Some(location) = location {
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(location).map_err(|_| ApiError::server())?,
        );
    }
    Ok(response)
}

async fn list_users(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let principal = authorize(&state, &headers, false).await?;
    let users = auth::list_users(&state.pool, &principal).await?;
    let size = query
        .get("pageSize")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| CatalogError::Invalid)?
        .unwrap_or(100);
    if !(1..=1000).contains(&size) {
        return Err(CatalogError::Invalid.into());
    }
    let offset = if let Some(cursor) = query.get("cursor") {
        let parts: Vec<&str> = cursor.split('.').collect();
        if parts.len() != 4
            || parts[0] != "u1"
            || parts[1] != principal.id
            || parts[3] != size.to_string()
        {
            return Err(CatalogError::InvalidCursor.into());
        }
        let offset: usize = parts[2].parse().map_err(|_| CatalogError::InvalidCursor)?;
        if offset == 0 || offset >= users.len() {
            return Err(CatalogError::InvalidCursor.into());
        }
        offset
    } else {
        0
    };
    let end = offset.saturating_add(size).min(users.len());
    let next = (end < users.len()).then(|| format!("u1.{}.{end}.{size}", principal.id));
    Ok(Json(
        json!({"items": &users[offset..end], "nextCursor": next}),
    ))
}

async fn create_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(input): Json<UserCreate>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let user = auth::create_user(
        &state.pool,
        &principal,
        required_key(&headers)?,
        &input.username,
        &input.password,
        input.publish_permission.unwrap_or(false),
    )
    .await?;
    let location = format!("/api/v1/users/{}", user.id);
    with_etag(
        StatusCode::CREATED,
        &user,
        &app_depot_infrastructure::catalog::etag("user", &user.id, 1),
        Some(&location),
    )
}

async fn get_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, false).await?;
    let (user, version) = auth::get_user(&state.pool, &principal, &id).await?;
    with_etag(
        StatusCode::OK,
        user,
        &app_depot_infrastructure::catalog::etag("user", &id, version),
        None,
    )
}

async fn patch_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<UserPatch>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let (user, version) = auth::patch_user(
        &state.pool,
        &principal,
        &id,
        validator,
        input.publish_permission,
        input.server_admin,
    )
    .await?;
    with_etag(
        StatusCode::OK,
        user,
        &app_depot_infrastructure::catalog::etag("user", &id, version),
        None,
    )
}

async fn revoke_user_token(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    auth::revoke_user_token(&state.pool, &principal, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, false).await?;
    let (enabled, version) = auth::get_settings(&state.pool, &principal).await?;
    with_etag(
        StatusCode::OK,
        json!({"anonymousReadEnabled":enabled}),
        &app_depot_infrastructure::catalog::etag("settings", "1", version),
        None,
    )
}

async fn patch_settings(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(input): Json<SettingsPatch>,
) -> Result<Response, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let validator = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let (enabled, version) = auth::patch_settings(
        &state.pool,
        &principal,
        validator,
        input.anonymous_read_enabled,
    )
    .await?;
    with_etag(
        StatusCode::OK,
        json!({"anonymousReadEnabled":enabled}),
        &app_depot_infrastructure::catalog::etag("settings", "1", version),
        None,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct User {
    id: String,
    username: String,
    server_admin: bool,
    publish_permission: bool,
}

impl From<Principal> for User {
    fn from(value: Principal) -> Self {
        Self {
            id: value.id,
            username: value.username,
            server_admin: value.server_admin,
            publish_permission: value.publish_permission,
        }
    }
}

async fn login(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Response, ApiError> {
    if !local_origin(&headers) {
        return Err(ApiError::forbidden());
    }
    let principal = auth::verify_password(&state.pool, &input.username, &input.password)
        .await
        .map_err(|_| ApiError::server())?
        .ok_or_else(ApiError::unauthenticated)?;
    let id = auth::random_secret();
    let csrf = state.csrf(&id);
    auth::create_browser_session(&state.pool, &principal.id, &id, &csrf)
        .await
        .map_err(|_| ApiError::server())?;
    let cookie = format!(
        "ad_session={}; {}HttpOnly; SameSite=Lax; Path=/; Max-Age=43200",
        state.signed_cookie(&id),
        if secure_cookie(&headers) {
            "Secure; "
        } else {
            ""
        }
    );
    let mut response =
        Json(json!({"user": User::from(principal), "csrfToken": csrf})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::server())?,
    );
    Ok(response)
}

async fn current(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let id = state
        .cookie_id(&headers)
        .ok_or_else(ApiError::unauthenticated)?;
    let principal = auth::browser_principal(&state.pool, &id)
        .await
        .map_err(|_| ApiError::server())?
        .ok_or_else(ApiError::unauthenticated)?;
    Ok(Json(
        json!({"user": User::from(principal), "csrfToken": state.csrf(&id)}),
    ))
}

async fn logout(State(state): State<AuthState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let _ = authorize(&state, &headers, true).await?;
    let id = state
        .cookie_id(&headers)
        .ok_or_else(ApiError::unauthenticated)?;
    auth::revoke_browser_session(&state.pool, &id)
        .await
        .map_err(|_| ApiError::server())?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "ad_session=; {}HttpOnly; SameSite=Lax; Path=/; Max-Age=0",
            if secure_cookie(&headers) {
                "Secure; "
            } else {
                ""
            }
        ))
        .map_err(|_| ApiError::server())?,
    );
    Ok(response)
}

async fn issue_token(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    let (token, issued) = auth::issue_token(&state.pool, &principal.id)
        .await
        .map_err(|_| ApiError::server())?;
    // SQLite stores the timestamp; this UTC spelling satisfies OpenAPI date-time.
    let timestamp: String =
        sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch')")
            .bind(issued)
            .fetch_one(&state.pool)
            .await
            .map_err(|_| ApiError::server())?;
    Ok(Json(
        json!({"token": token, "metadata": {"active": true, "issuedAt": timestamp}}),
    ))
}

async fn read_token(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = authorize(&state, &headers, false).await?;
    let (active, issued_at) = auth::token_metadata(&state.pool, &principal.id)
        .await
        .map_err(|_| ApiError::server())?;
    Ok(Json(json!({"active": active, "issuedAt": issued_at})))
}

async fn delete_token(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let principal = authorize(&state, &headers, true).await?;
    auth::revoke_token(&state.pool, &principal.id)
        .await
        .map_err(|_| ApiError::server())?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn authorize(
    state: &AuthState,
    headers: &HeaderMap,
    mutation: bool,
) -> Result<Principal, ApiError> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let raw = value.to_str().map_err(|_| ApiError::unauthenticated())?;
        let token = raw
            .strip_prefix("Bearer ")
            .ok_or_else(ApiError::unauthenticated)?;
        return auth::bearer_principal(&state.pool, token)
            .await
            .map_err(|_| ApiError::server())?
            .ok_or_else(ApiError::unauthenticated);
    }
    let id = state
        .cookie_id(headers)
        .ok_or_else(ApiError::unauthenticated)?;
    if mutation {
        if !local_origin(headers) {
            return Err(ApiError::forbidden());
        }
        let csrf = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(ApiError::forbidden)?;
        if !auth::csrf_valid(&state.pool, &id, csrf)
            .await
            .map_err(|_| ApiError::server())?
        {
            return Err(ApiError::forbidden());
        }
    }
    auth::browser_principal(&state.pool, &id)
        .await
        .map_err(|_| ApiError::server())?
        .ok_or_else(ApiError::unauthenticated)
}

pub async fn reader(state: &AuthState, headers: &HeaderMap) -> Result<Option<Principal>, ApiError> {
    let supplied = headers.contains_key(header::AUTHORIZATION)
        || headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .any(|part| part.trim().starts_with("ad_session="))
            })
            .unwrap_or(false);
    if supplied {
        return authorize(state, headers, false).await.map(Some);
    }
    let enabled: i64 =
        sqlx::query_scalar("SELECT anonymous_read_enabled FROM server_settings WHERE id=1")
            .fetch_one(&state.pool)
            .await
            .map_err(|_| ApiError::server())?;
    if enabled == 0 {
        return Err(ApiError::unauthenticated());
    }
    Ok(None)
}

pub fn router(state: AuthState) -> Router {
    Router::new()
        .route("/setup", get(setup_status).post(setup_admin))
        .route("/auth/sessions", post(login))
        .route("/auth/sessions/current", get(current).delete(logout))
        .route(
            "/me/token",
            get(read_token).put(issue_token).delete(delete_token),
        )
        .route("/users", get(list_users).post(create_user))
        .route("/users/{user_id}", get(get_user).patch(patch_user))
        .route(
            "/users/{user_id}/token",
            axum::routing::delete(revoke_user_token),
        )
        .route("/settings", get(get_settings).patch(patch_settings))
        .with_state(state)
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    #[test]
    fn loopback_browser_origin_survives_a_container_bridge() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:18796"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:18796"),
        );
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert("x-forwarded-host", HeaderValue::from_static("evil.example"));
        let origin = effective_origin(&headers, IpAddr::from([172, 18, 0, 1]), &HashSet::new())
            .expect("loopback Host remains usable through Docker's bridge");
        assert_eq!(origin, "http://127.0.0.1:18796");
        headers.insert("x-app-depot-effective-origin", origin);
        assert!(local_origin(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!local_origin(&headers));
    }

    #[test]
    fn remote_http_origin_and_trusted_https_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("depot.example"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("depot.example"),
        );
        let peer = IpAddr::from([172, 18, 0, 1]);
        assert_eq!(
            effective_origin(&headers, peer, &HashSet::new()).unwrap(),
            "http://depot.example"
        );
        let trusted = HashSet::from([peer]);
        assert_eq!(
            effective_origin(&headers, peer, &trusted).unwrap(),
            "https://depot.example"
        );
    }
}
