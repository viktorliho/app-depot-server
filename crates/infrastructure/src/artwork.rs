//! Current App artwork bindings. Image bytes use the same immutable CAS as software artifacts.
use crate::{
    auth::now,
    catalog::{self, CatalogError},
    upload::promote_file,
};
use app_depot_application::Principal;
use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Write},
    path::Path,
};
use uuid::Uuid;

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

struct ClaimCleanup {
    pool: SqlitePool,
    id: String,
}

impl Drop for ClaimCleanup {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let _ = sqlx::query("DELETE FROM artwork_claims WHERE id=?")
                .bind(id)
                .execute(&pool)
                .await;
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artwork {
    pub app_id: String,
    pub role: String,
    pub id: String,
    pub digest: String,
    pub mime_type: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub etag: String,
}

pub fn valid_role(role: &str) -> bool {
    let bytes = role.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 80
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !bytes
            .windows(2)
            .any(|pair| pair[0] == b'.' && pair[1] == b'.')
        && !matches!(bytes.last(), Some(b'.' | b'_' | b'-'))
}

fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Artwork, CatalogError> {
    let id: String = row.try_get("id")?;
    let version: i64 = row.try_get("etag_version")?;
    Ok(Artwork {
        app_id: row.try_get("app_id")?,
        role: row.try_get("role")?,
        id: id.clone(),
        digest: row.try_get("digest")?,
        mime_type: row.try_get("mime_type")?,
        size: row.try_get::<i64, _>("size")? as u64,
        width: row.try_get::<i64, _>("width")? as u32,
        height: row.try_get::<i64, _>("height")? as u32,
        etag: catalog::etag("artwork", &id, version),
    })
}

async fn visible(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    app_id: &str,
) -> Result<(), CatalogError> {
    catalog::get_app(pool, principal, app_id).await.map(|_| ())
}

async fn manageable(
    pool: &SqlitePool,
    principal: &Principal,
    app_id: &str,
) -> Result<(), CatalogError> {
    let (app, _) = catalog::get_app(pool, Some(principal), app_id).await?;
    if !principal.may_manage(&app.owner_id) {
        return Err(CatalogError::Forbidden);
    }
    Ok(())
}

pub async fn list(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    app_id: &str,
) -> Result<Vec<Artwork>, CatalogError> {
    visible(pool, principal, app_id).await?;
    let rows = sqlx::query("SELECT w.* FROM artwork_bindings w JOIN apps a ON a.id=w.app_id WHERE w.app_id=? AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND a.state='Published' AND (?=1 OR (SELECT anonymous_read_enabled FROM server_settings WHERE id=1)=1))) ORDER BY w.role")
        .bind(app_id)
        .bind(principal.map(|actor| actor.id.as_str()).unwrap_or(""))
        .bind(principal.map(|actor| actor.server_admin as i64).unwrap_or(0))
        .bind(principal.is_some() as i64)
        .fetch_all(pool)
        .await?;
    if rows.is_empty() {
        visible(pool, principal, app_id).await?;
    }
    rows.into_iter().map(from_row).collect()
}

pub async fn get(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    app_id: &str,
    role: &str,
) -> Result<Artwork, CatalogError> {
    if !valid_role(role) {
        return Err(CatalogError::NotFound);
    }
    let row = sqlx::query("SELECT w.* FROM artwork_bindings w JOIN apps a ON a.id=w.app_id WHERE w.app_id=? AND w.role=? AND (a.owner_id=? OR ?=1 OR (a.visibility='Public' AND a.state='Published' AND (?=1 OR (SELECT anonymous_read_enabled FROM server_settings WHERE id=1)=1)))")
        .bind(app_id)
        .bind(role)
        .bind(principal.map(|actor| actor.id.as_str()).unwrap_or(""))
        .bind(principal.map(|actor| actor.server_admin as i64).unwrap_or(0))
        .bind(principal.is_some() as i64)
        .fetch_optional(pool)
        .await?
        .ok_or(CatalogError::NotFound)?;
    from_row(row)
}

fn inspect(bytes: &[u8]) -> Result<(&'static str, u32, u32), CatalogError> {
    fn image_error(error: image::ImageError) -> CatalogError {
        match error {
            image::ImageError::Limits(_) => CatalogError::ImageTooLarge,
            _ => CatalogError::InvalidImage,
        }
    }
    if bytes.is_empty() {
        return Err(CatalogError::InvalidImage);
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(CatalogError::ImageTooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| CatalogError::UnsupportedImage)?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err(CatalogError::UnsupportedImage),
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    let (width, height) = reader.into_dimensions().map_err(image_error)?;
    if width == 0 || height == 0 || (width as u64) * (height as u64) > 16_000_000 {
        return Err(CatalogError::ImageTooLarge);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    reader.decode().map_err(image_error)?;
    Ok((mime, width, height))
}

fn request_hash(app_id: &str, role: &str, validator: &str, digest: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{app_id}\0{role}\0{validator}\0{digest}").as_bytes())
    )
}

async fn replay(
    pool: &SqlitePool,
    principal: &Principal,
    action: &str,
    key: &str,
    hash: &str,
) -> Result<Option<Artwork>, CatalogError> {
    let row = sqlx::query("SELECT input_hash, result_json FROM idempotency_bindings WHERE principal_id=? AND action=? AND key=?")
        .bind(&principal.id).bind(action).bind(key).fetch_optional(pool).await?;
    match row {
        Some(row) if row.try_get::<String, _>("input_hash")? == hash => Ok(Some(
            serde_json::from_str(&row.try_get::<String, _>("result_json")?)?,
        )),
        Some(_) => Err(CatalogError::IdempotencyConflict),
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn put(
    pool: &SqlitePool,
    root: &Path,
    principal: &Principal,
    key: &str,
    app_id: &str,
    role: &str,
    validator: Option<&str>,
    bytes: &[u8],
) -> Result<Artwork, CatalogError> {
    if !valid_role(role) || key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid);
    }
    let validator = validator.ok_or(CatalogError::PreconditionRequired)?;
    if validator != "*" && !(validator.starts_with("\"artwork-") && validator.ends_with('"')) {
        return Err(CatalogError::StaleEtag);
    }
    manageable(pool, principal, app_id).await?;
    let (mime, width, height) = inspect(bytes)?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    let hash = request_hash(app_id, role, validator, &digest);
    if let Some(item) = replay(pool, principal, "ArtworkPut", key, &hash).await? {
        return Ok(item);
    }
    let claim_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO artwork_claims(id, digest, created_at) VALUES(?, ?, ?)")
        .bind(&claim_id)
        .bind(&digest)
        .bind(now())
        .execute(pool)
        .await?;
    let _claim_cleanup = ClaimCleanup {
        pool: pool.clone(),
        id: claim_id.clone(),
    };
    let result = put_claimed(
        pool, root, principal, key, app_id, role, validator, bytes, mime, width, height, &digest,
        &hash, &claim_id,
    )
    .await;
    // The drop guard also clears a claim if the request future is cancelled.
    let _ = sqlx::query("DELETE FROM artwork_claims WHERE id=?")
        .bind(&claim_id)
        .execute(pool)
        .await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn put_claimed(
    pool: &SqlitePool,
    root: &Path,
    principal: &Principal,
    key: &str,
    app_id: &str,
    role: &str,
    validator: &str,
    bytes: &[u8],
    mime: &str,
    width: u32,
    height: u32,
    digest: &str,
    hash: &str,
    claim_id: &str,
) -> Result<Artwork, CatalogError> {
    let stage = root.join("ops").join(format!("artwork-{claim_id}"));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        File::open(root.join("ops"))?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&stage);
        return Err(CatalogError::Storage);
    }
    let promoted =
        promote_file(root, &stage, digest, bytes.len() as u64).map_err(|_| CatalogError::Storage);
    let _ = fs::remove_file(&stage);
    let _ = File::open(root.join("ops")).and_then(|file| file.sync_all());
    promoted?;
    let mut tx = pool.begin().await?;
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='ArtworkPut' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await?;
    if retired != 0 {
        return Err(CatalogError::IdempotencyConflict);
    }
    if let Some(row) = sqlx::query("SELECT input_hash, result_json FROM idempotency_bindings WHERE principal_id=? AND action='ArtworkPut' AND key=?")
        .bind(&principal.id).bind(key).fetch_optional(&mut *tx).await? {
        if row.try_get::<String, _>("input_hash")? != hash { return Err(CatalogError::IdempotencyConflict); }
        return Ok(serde_json::from_str(&row.try_get::<String, _>("result_json")?)?);
    }
    let app = sqlx::query("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&app.try_get::<String, _>("owner_id")?) {
        return Err(CatalogError::NotFound);
    }
    let current =
        sqlx::query("SELECT id, etag_version FROM artwork_bindings WHERE app_id=? AND role=?")
            .bind(app_id)
            .bind(role)
            .fetch_optional(&mut *tx)
            .await?;
    let (id, version) = if let Some(row) = current {
        let id: String = row.try_get("id")?;
        let version: i64 = row.try_get("etag_version")?;
        if validator != catalog::etag("artwork", &id, version) {
            return Err(CatalogError::StaleEtag);
        }
        (id, version + 1)
    } else {
        if validator != "*" {
            return Err(CatalogError::StaleEtag);
        }
        (Uuid::new_v4().to_string(), 1)
    };
    let registered = sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1) ON CONFLICT(digest) DO UPDATE SET verified=1 WHERE cas_objects.size=excluded.size")
        .bind(digest).bind(bytes.len() as i64).execute(&mut *tx).await?;
    if registered.rows_affected() != 1 {
        return Err(CatalogError::Storage);
    }
    let item = Artwork {
        app_id: app_id.into(),
        role: role.into(),
        id: id.clone(),
        digest: digest.into(),
        mime_type: mime.into(),
        size: bytes.len() as u64,
        width,
        height,
        etag: catalog::etag("artwork", &id, version),
    };
    sqlx::query("INSERT INTO artwork_bindings(app_id, role, id, etag_version, digest, mime_type, size, width, height) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(app_id, role) DO UPDATE SET etag_version=excluded.etag_version, digest=excluded.digest, mime_type=excluded.mime_type, size=excluded.size, width=excluded.width, height=excluded.height")
        .bind(app_id).bind(role).bind(&id).bind(version).bind(digest).bind(mime).bind(bytes.len() as i64).bind(width as i64).bind(height as i64).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'ArtworkPut', ?, ?, ?, ?, ?)")
        .bind(&principal.id).bind(key).bind(hash).bind(app_id).bind(serde_json::to_string(&item)?).bind(now()).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM artwork_claims WHERE id=?")
        .bind(claim_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(item)
}

pub async fn delete(
    pool: &SqlitePool,
    principal: &Principal,
    app_id: &str,
    role: &str,
    validator: Option<&str>,
) -> Result<(), CatalogError> {
    if !valid_role(role) {
        return Err(CatalogError::Invalid);
    }
    let validator = validator.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let app = sqlx::query("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&app.try_get::<String, _>("owner_id")?) {
        return Err(CatalogError::NotFound);
    }
    let row =
        sqlx::query("SELECT id, etag_version FROM artwork_bindings WHERE app_id=? AND role=?")
            .bind(app_id)
            .bind(role)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(CatalogError::NotFound)?;
    let expected = catalog::etag(
        "artwork",
        &row.try_get::<String, _>("id")?,
        row.try_get("etag_version")?,
    );
    if validator != expected {
        return Err(CatalogError::StaleEtag);
    }
    sqlx::query("DELETE FROM artwork_bindings WHERE app_id=? AND role=?")
        .bind(app_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn recover_claims(pool: &SqlitePool, root: &Path) -> Result<(), CatalogError> {
    sqlx::query("DELETE FROM artwork_claims")
        .execute(pool)
        .await?;
    let ops = root.join("ops");
    for entry in fs::read_dir(ops).map_err(|_| CatalogError::Storage)? {
        let entry = entry.map_err(|_| CatalogError::Storage)?;
        if entry.file_name().to_string_lossy().starts_with("artwork-")
            && entry
                .file_type()
                .map_err(|_| CatalogError::Storage)?
                .is_file()
        {
            fs::remove_file(entry.path()).map_err(|_| CatalogError::Storage)?;
        }
    }
    Ok(())
}
