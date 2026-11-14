use app_depot_application::{may_read_catalog, Principal};
use app_depot_domain::{base_slug, App, Association, FileArtifactId, Release, TreeArtifactId};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::auth::now;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("invalid request")]
    Invalid,
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("idempotency key conflict")]
    IdempotencyConflict,
    #[error("resource conflict")]
    Conflict,
    #[error("immutable release")]
    ImmutableRelease,
    #[error("invalid channel target")]
    InvalidChannelTarget,
    #[error("precondition required")]
    PreconditionRequired,
    #[error("stale ETag")]
    StaleEtag,
    #[error("invalid image")]
    InvalidImage,
    #[error("unsupported image format")]
    UnsupportedImage,
    #[error("artwork exceeds resource limits")]
    ImageTooLarge,
    #[error("storage unavailable")]
    Storage,
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn etag(kind: &str, id: &str, version: i64) -> String {
    format!("\"{kind}-{id}-{version}\"")
}

fn input_hash(value: &impl Serialize) -> Result<String, CatalogError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

async fn replay<T: DeserializeOwned>(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &str,
    action: &str,
    key: &str,
    expected: &str,
) -> Result<Option<T>, CatalogError> {
    let row = sqlx::query("SELECT input_hash, result_json FROM idempotency_bindings WHERE principal_id=? AND action=? AND key=?")
        .bind(principal).bind(action).bind(key).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.try_get::<String, _>("input_hash")? != expected {
        return Err(CatalogError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(
        &row.try_get::<String, _>("result_json")?,
    )?))
}

async fn bind<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &str,
    action: &str,
    key: &str,
    hash: &str,
    id: &str,
    result: &T,
) -> Result<(), CatalogError> {
    sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, ?, ?, ?, ?, ?, ?)")
        .bind(principal).bind(action).bind(key).bind(hash).bind(id).bind(serde_json::to_string(result)?).bind(now())
        .execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn create_initial_declaration(
    tx: &mut Transaction<'_, Sqlite>,
    scope: &str,
    resource_id: &str,
) -> Result<(), sqlx::Error> {
    let revision_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO declaration_documents(scope, resource_id, revision_id) VALUES(?, ?, ?)",
    )
    .bind(scope)
    .bind(resource_id)
    .bind(&revision_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO declaration_revisions(id, scope, resource_id, data_json, created_at) VALUES(?, ?, ?, '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))")
        .bind(revision_id).bind(scope).bind(resource_id).execute(&mut **tx).await?;
    Ok(())
}

pub async fn create_app(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    name: &str,
) -> Result<App, CatalogError> {
    if key.is_empty() || key.len() > 255 || name.trim().is_empty() {
        return Err(CatalogError::Invalid);
    }
    let hash = input_hash(&name)?;
    let mut tx = pool.begin().await?;
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='CreateApp' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await?;
    if retired != 0 {
        return Err(CatalogError::IdempotencyConflict);
    }
    if let Some(app) = replay(&mut tx, &principal.id, "CreateApp", key, &hash).await? {
        return Ok(app);
    }
    if !principal.may_create_app() {
        return Err(CatalogError::Forbidden);
    }
    let permission: i64 =
        sqlx::query_scalar("SELECT server_admin OR publish_permission FROM users WHERE id=?")
            .bind(&principal.id)
            .fetch_one(&mut *tx)
            .await?;
    if permission == 0 {
        return Err(CatalogError::Forbidden);
    }
    let id = Uuid::new_v4().to_string();
    let base = base_slug(name).unwrap_or_else(|| format!("app-{}", &id[..8]));
    let mut slug = base.clone();
    let mut suffix = 2;
    while sqlx::query_scalar::<_, i64>("SELECT count(*) FROM apps WHERE slug=?")
        .bind(&slug)
        .fetch_one(&mut *tx)
        .await?
        != 0
    {
        slug = format!("{base}-{suffix}");
        suffix += 1;
    }
    sqlx::query("INSERT INTO apps(id, name, slug, owner_id) VALUES(?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(&slug)
        .bind(&principal.id)
        .execute(&mut *tx)
        .await?;
    create_initial_declaration(&mut tx, "App", &id).await?;
    let app = App {
        id: id.clone(),
        name: name.into(),
        slug,
        owner_id: principal.id.clone(),
        visibility: "Private".into(),
        state: "Draft".into(),
    };
    bind(&mut tx, &principal.id, "CreateApp", key, &hash, &id, &app).await?;
    tx.commit().await?;
    Ok(app)
}

pub async fn get_app(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    id: &str,
) -> Result<(App, i64), CatalogError> {
    let row = sqlx::query(
        "SELECT id, name, slug, owner_id, visibility, state, etag_version FROM apps WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let owner: String = row.try_get("owner_id")?;
    let anonymous_enabled = if principal.is_none() {
        sqlx::query_scalar::<_, i64>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(pool)
        .await?
            != 0
    } else {
        false
    };
    if !may_read_catalog(
        principal,
        &owner,
        row.try_get::<String, _>("visibility")? == "Public"
            && row.try_get::<String, _>("state")? == "Published",
        anonymous_enabled,
    ) {
        return Err(CatalogError::NotFound);
    }
    Ok((
        App {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            slug: row.try_get("slug")?,
            owner_id: owner,
            visibility: row.try_get("visibility")?,
            state: row.try_get("state")?,
        },
        row.try_get("etag_version")?,
    ))
}

pub async fn list_apps(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    query: &str,
) -> Result<Vec<App>, CatalogError> {
    let public_allowed = if principal.is_some() {
        true
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(pool)
        .await?
            != 0
    };
    let actor_id = principal.map(|actor| actor.id.as_str()).unwrap_or("");
    let admin = principal.map(|actor| actor.server_admin).unwrap_or(false);
    let rows = sqlx::query("SELECT id, name, slug, owner_id, visibility, state FROM apps WHERE (owner_id=? OR ?=1 OR (?=1 AND visibility='Public' AND state='Published')) AND instr(lower(name), lower(?)) > 0 ORDER BY id")
        .bind(actor_id).bind(admin as i64).bind(public_allowed as i64).bind(query)
        .fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(App {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                slug: row.try_get("slug")?,
                owner_id: row.try_get("owner_id")?,
                visibility: row.try_get("visibility")?,
                state: row.try_get("state")?,
            })
        })
        .collect()
}

pub async fn patch_app(
    pool: &SqlitePool,
    principal: &Principal,
    id: &str,
    if_match: Option<&str>,
    name: Option<&str>,
    slug: Option<&str>,
    visibility: Option<&str>,
) -> Result<(App, i64), CatalogError> {
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    if name.is_some_and(|value| value.trim().is_empty())
        || slug.is_some_and(|value| value.is_empty())
        || visibility.is_some_and(|value| !matches!(value, "Private" | "Public"))
    {
        return Err(CatalogError::Invalid);
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT name, slug, owner_id, visibility, state, etag_version FROM apps WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let owner: String = row.try_get("owner_id")?;
    if !principal.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let version: i64 = row.try_get("etag_version")?;
    if validator != etag("app", id, version) {
        return Err(CatalogError::StaleEtag);
    }
    let app = App {
        id: id.into(),
        name: name.unwrap_or(&row.try_get::<String, _>("name")?).into(),
        slug: slug.unwrap_or(&row.try_get::<String, _>("slug")?).into(),
        owner_id: owner,
        visibility: visibility
            .unwrap_or(&row.try_get::<String, _>("visibility")?)
            .into(),
        state: row.try_get("state")?,
    };
    let duplicate: i64 = sqlx::query_scalar("SELECT count(*) FROM apps WHERE slug=? AND id<>?")
        .bind(&app.slug)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if duplicate != 0 {
        return Err(CatalogError::Conflict);
    }
    sqlx::query(
        "UPDATE apps SET name=?, slug=?, visibility=?, etag_version=etag_version+1 WHERE id=?",
    )
    .bind(&app.name)
    .bind(&app.slug)
    .bind(&app.visibility)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((app, version + 1))
}

pub async fn create_release(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    app_id: &str,
    version: Option<&str>,
) -> Result<Release, CatalogError> {
    if key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid);
    }
    let hash = input_hash(&(app_id, version))?;
    let mut tx = pool.begin().await?;
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='CreateRelease' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await?;
    if retired != 0 {
        return Err(CatalogError::IdempotencyConflict);
    }
    if let Some(release) = replay(&mut tx, &principal.id, "CreateRelease", key, &hash).await? {
        return Ok(release);
    }
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO releases(id, app_id, version) VALUES(?, ?, ?)")
        .bind(&id)
        .bind(app_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    create_initial_declaration(&mut tx, "Release", &id).await?;
    let release = Release {
        id: id.clone(),
        app_id: app_id.into(),
        state: "Draft".into(),
        version: version.map(str::to_owned),
    };
    bind(
        &mut tx,
        &principal.id,
        "CreateRelease",
        key,
        &hash,
        &id,
        &release,
    )
    .await?;
    tx.commit().await?;
    Ok(release)
}

pub async fn get_release(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    id: &str,
) -> Result<(Release, i64), CatalogError> {
    let row = sqlx::query("SELECT r.id, r.app_id, r.version, r.state, r.etag_version, a.owner_id, a.visibility FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(id).fetch_optional(pool).await?.ok_or(CatalogError::NotFound)?;
    let owner: String = row.try_get("owner_id")?;
    let anonymous_enabled = if principal.is_none() {
        sqlx::query_scalar::<_, i64>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(pool)
        .await?
            != 0
    } else {
        false
    };
    if !may_read_catalog(
        principal,
        &owner,
        row.try_get::<String, _>("visibility")? == "Public"
            && row.try_get::<String, _>("state")? == "Published",
        anonymous_enabled,
    ) {
        return Err(CatalogError::NotFound);
    }
    Ok((
        Release {
            id: row.try_get("id")?,
            app_id: row.try_get("app_id")?,
            state: row.try_get("state")?,
            version: row.try_get("version")?,
        },
        row.try_get("etag_version")?,
    ))
}

pub async fn list_releases(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    app_id: &str,
) -> Result<Vec<Release>, CatalogError> {
    let (app, _) = get_app(pool, principal, app_id).await?;
    let full = principal
        .map(|actor| actor.may_manage(&app.owner_id))
        .unwrap_or(false);
    let rows = sqlx::query("SELECT id, app_id, version, state FROM releases WHERE app_id=? AND (?=1 OR state='Published') ORDER BY id")
        .bind(app_id).bind(full as i64).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(Release {
                id: row.try_get("id")?,
                app_id: row.try_get("app_id")?,
                version: row.try_get("version")?,
                state: row.try_get("state")?,
            })
        })
        .collect()
}

pub async fn patch_release(
    pool: &SqlitePool,
    principal: &Principal,
    id: &str,
    if_match: Option<&str>,
    version_patch: Option<Option<&str>>,
) -> Result<(Release, i64), CatalogError> {
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT r.app_id, r.version, r.state, r.etag_version, a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&row.try_get::<String, _>("owner_id")?) {
        return Err(CatalogError::NotFound);
    }
    let version: i64 = row.try_get("etag_version")?;
    if validator != etag("release", id, version) {
        return Err(CatalogError::StaleEtag);
    }
    let visible_version = match version_patch {
        Some(value) => value.map(str::to_owned),
        None => row.try_get("version")?,
    };
    sqlx::query("UPDATE releases SET version=?, etag_version=etag_version+1 WHERE id=?")
        .bind(&visible_version)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let release = Release {
        id: id.into(),
        app_id: row.try_get("app_id")?,
        state: row.try_get("state")?,
        version: visible_version,
    };
    tx.commit().await?;
    Ok((release, version + 1))
}

pub async fn list_associations(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    release_id: &str,
) -> Result<Vec<Association>, CatalogError> {
    get_release(pool, principal, release_id).await?;
    let rows = sqlx::query(
        "SELECT id, release_id, artifact_id FROM associations WHERE release_id=? ORDER BY id",
    )
    .bind(release_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Association {
                id: row.try_get("id")?,
                release_id: row.try_get("release_id")?,
                artifact_id: row.try_get("artifact_id")?,
            })
        })
        .collect()
}

pub async fn get_association(
    pool: &SqlitePool,
    principal: Option<&Principal>,
    id: &str,
) -> Result<(Association, i64), CatalogError> {
    let row = sqlx::query("SELECT x.id, x.release_id, x.artifact_id, x.etag_version, a.owner_id FROM associations x JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE x.id=?")
        .bind(id).fetch_optional(pool).await?.ok_or(CatalogError::NotFound)?;
    let release_id: String = row.try_get("release_id")?;
    get_release(pool, principal, &release_id).await?;
    Ok((
        Association {
            id: row.try_get("id")?,
            release_id: row.try_get("release_id")?,
            artifact_id: row.try_get("artifact_id")?,
        },
        row.try_get("etag_version")?,
    ))
}

pub async fn create_association(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    release_id: &str,
    artifact_id: &str,
) -> Result<Association, CatalogError> {
    if key.is_empty()
        || key.len() > 255
        || (FileArtifactId::parse(artifact_id).is_none()
            && TreeArtifactId::parse(artifact_id).is_none())
    {
        return Err(CatalogError::Invalid);
    }
    let hash = input_hash(&(release_id, artifact_id))?;
    let mut tx = pool.begin().await?;
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='CreateAssociation' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await?;
    if retired != 0 {
        return Err(CatalogError::IdempotencyConflict);
    }
    if let Some(association) =
        replay(&mut tx, &principal.id, "CreateAssociation", key, &hash).await?
    {
        return Ok(association);
    }
    let row = sqlx::query(
        "SELECT r.state, a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?",
    )
    .bind(release_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&row.try_get::<String, _>("owner_id")?) {
        return Err(CatalogError::NotFound);
    }
    if row.try_get::<String, _>("state")? != "Draft" {
        return Err(CatalogError::ImmutableRelease);
    }
    let accessible: i64 = sqlx::query_scalar("SELECT count(*) FROM associations x JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE x.artifact_id=? AND (a.owner_id=? OR ?=1)")
        .bind(artifact_id).bind(&principal.id).bind(principal.server_admin as i64).fetch_one(&mut *tx).await?;
    if accessible == 0 {
        return Err(CatalogError::NotFound);
    }
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM associations WHERE release_id=? AND artifact_id=?")
            .bind(release_id)
            .bind(artifact_id)
            .fetch_optional(&mut *tx)
            .await?;
    let is_new = existing.is_none();
    let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
    if is_new {
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(&id)
            .bind(release_id)
            .bind(artifact_id)
            .execute(&mut *tx)
            .await?;
        create_initial_declaration(&mut tx, "Association", &id).await?;
    }
    let association = Association {
        id: id.clone(),
        release_id: release_id.into(),
        artifact_id: artifact_id.into(),
    };
    bind(
        &mut tx,
        &principal.id,
        "CreateAssociation",
        key,
        &hash,
        &id,
        &association,
    )
    .await?;
    tx.commit().await?;
    Ok(association)
}

async fn retire_session(
    tx: &mut Transaction<'_, Sqlite>,
    deletion_id: &str,
    session_id: &str,
    operation_id: &str,
    cancel_operation_id: Option<&str>,
    time: i64,
) -> Result<(), CatalogError> {
    sqlx::query("INSERT OR IGNORE INTO deletion_staging(operation_id, session_id) VALUES(?, ?)")
        .bind(deletion_id)
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO idempotency_tombstones(principal_id, action, key, input_hash, created_at) SELECT principal_id, action, key, input_hash, ? FROM idempotency_bindings WHERE result_id=?")
        .bind(time).bind(session_id).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM idempotency_bindings WHERE result_id=?")
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM upload_sessions WHERE id=?")
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM operations WHERE id=?")
        .bind(operation_id)
        .execute(&mut **tx)
        .await?;
    if let Some(cancel_id) = cancel_operation_id {
        sqlx::query("DELETE FROM operations WHERE id=?")
            .bind(cancel_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn retire_association(
    tx: &mut Transaction<'_, Sqlite>,
    deletion_id: &str,
    association_id: &str,
    time: i64,
) -> Result<(), CatalogError> {
    let artifact_id: String = sqlx::query_scalar("SELECT artifact_id FROM associations WHERE id=?")
        .bind(association_id)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO deletion_artifacts(operation_id, artifact_id) VALUES(?, ?)")
        .bind(deletion_id)
        .bind(&artifact_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO deletion_cas(operation_id, digest) SELECT ?, digest FROM artifacts WHERE id=? AND digest IS NOT NULL UNION SELECT ?, sha256 FROM artifact_entries WHERE artifact_id=? AND sha256 IS NOT NULL")
        .bind(deletion_id).bind(&artifact_id).bind(deletion_id).bind(&artifact_id).execute(&mut **tx).await?;
    let sessions = sqlx::query("SELECT s.id, s.operation_id, s.cancel_operation_id FROM upload_sessions s JOIN operations o ON o.id=s.operation_id WHERE o.result_json IS NOT NULL AND json_valid(o.result_json) AND json_extract(o.result_json, '$.associationId')=?")
        .bind(association_id).fetch_all(&mut **tx).await?;
    for session in sessions {
        let session_id: String = session.try_get("id")?;
        let operation_id: String = session.try_get("operation_id")?;
        let cancel_id: Option<String> = session.try_get("cancel_operation_id")?;
        retire_session(
            tx,
            deletion_id,
            &session_id,
            &operation_id,
            cancel_id.as_deref(),
            time,
        )
        .await?;
    }
    sqlx::query("INSERT INTO idempotency_tombstones(principal_id, action, key, input_hash, created_at) SELECT principal_id, action, key, input_hash, ? FROM idempotency_bindings WHERE action='CreateAssociation' AND result_id=?")
        .bind(time).bind(association_id).execute(&mut **tx).await?;
    sqlx::query(
        "DELETE FROM idempotency_bindings WHERE action='CreateAssociation' AND result_id=?",
    )
    .bind(association_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM declaration_documents WHERE scope='Association' AND resource_id=?")
        .bind(association_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM associations WHERE id=?")
        .bind(association_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn delete_association(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    id: &str,
    if_match: Option<&str>,
) -> Result<String, CatalogError> {
    if key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid);
    }
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let hash = input_hash(&(id, validator))?;
    let mut tx = pool.begin().await?;
    if let Some(operation_id) =
        replay::<String>(&mut tx, &principal.id, "DeleteAssociation", key, &hash).await?
    {
        return Ok(operation_id);
    }
    let row = sqlx::query("SELECT x.etag_version, r.state, a.owner_id FROM associations x JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE x.id=?")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
    if !principal.may_manage(&row.try_get::<String, _>("owner_id")?) {
        return Err(CatalogError::NotFound);
    }
    let version: i64 = row.try_get("etag_version")?;
    if validator != etag("association", id, version) {
        return Err(CatalogError::StaleEtag);
    }
    if row.try_get::<String, _>("state")? != "Draft" {
        return Err(CatalogError::ImmutableRelease);
    }
    let operation_id = Uuid::new_v4().to_string();
    let now = now();
    sqlx::query("INSERT INTO deletion_operations(id, owner_id, kind, target_id, status, physical_cleanup, created_at, updated_at) VALUES(?, ?, 'DeleteAssociation', ?, 'Accepted', 'Pending', ?, ?)")
        .bind(&operation_id).bind(&principal.id).bind(id).bind(now).bind(now).execute(&mut *tx).await?;
    retire_association(&mut tx, &operation_id, id, now).await?;
    bind(
        &mut tx,
        &principal.id,
        "DeleteAssociation",
        key,
        &hash,
        id,
        &operation_id,
    )
    .await?;
    tx.commit().await?;
    Ok(operation_id)
}

async fn delete_resource(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    id: &str,
    if_match: Option<&str>,
    kind: &str,
) -> Result<String, CatalogError> {
    if key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid);
    }
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let hash = input_hash(&(id, validator))?;
    let mut tx = pool.begin().await?;
    if let Some(operation_id) = replay::<String>(&mut tx, &principal.id, kind, key, &hash).await? {
        return Ok(operation_id);
    }
    let (app_id, release_ids, version, owner): (String, Vec<String>, i64, String) = if kind
        == "DeleteApp"
    {
        let row = sqlx::query("SELECT etag_version, owner_id FROM apps WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let releases: Vec<String> =
            sqlx::query_scalar("SELECT id FROM releases WHERE app_id=? ORDER BY id")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        (
            id.into(),
            releases,
            row.try_get("etag_version")?,
            row.try_get("owner_id")?,
        )
    } else if kind == "DeleteRelease" {
        let row = sqlx::query("SELECT r.app_id, r.etag_version, a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
            .bind(id).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
        (
            row.try_get("app_id")?,
            vec![id.into()],
            row.try_get("etag_version")?,
            row.try_get("owner_id")?,
        )
    } else {
        return Err(CatalogError::Invalid);
    };
    if !principal.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let resource_kind = if kind == "DeleteApp" {
        "app"
    } else {
        "release"
    };
    if validator != etag(resource_kind, id, version) {
        return Err(CatalogError::StaleEtag);
    }
    let operation_id = Uuid::new_v4().to_string();
    let time = now();
    sqlx::query("INSERT INTO deletion_operations(id, owner_id, kind, target_id, status, physical_cleanup, created_at, updated_at) VALUES(?, ?, ?, ?, 'Accepted', 'Pending', ?, ?)")
        .bind(&operation_id).bind(&principal.id).bind(kind).bind(id).bind(time).bind(time).execute(&mut *tx).await?;
    for release_id in &release_ids {
        let association_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM associations WHERE release_id=? ORDER BY id")
                .bind(release_id)
                .fetch_all(&mut *tx)
                .await?;
        for association_id in association_ids {
            retire_association(&mut tx, &operation_id, &association_id, time).await?;
        }
    }
    let sessions = sqlx::query("SELECT id, operation_id, cancel_operation_id, target_json FROM upload_sessions ORDER BY id")
        .fetch_all(&mut *tx).await?;
    for session in sessions {
        let target: app_depot_application::upload::Target =
            serde_json::from_str(&session.try_get::<String, _>("target_json")?)?;
        let matches = match target {
            app_depot_application::upload::Target::Release { release_id } => {
                release_ids.contains(&release_id)
            }
            app_depot_application::upload::Target::NewRelease {
                app_id: target_app, ..
            } => kind == "DeleteApp" && target_app == app_id,
            app_depot_application::upload::Target::NewApp { .. } => false,
        };
        if matches {
            let session_id: String = session.try_get("id")?;
            let finalize_id: String = session.try_get("operation_id")?;
            let cancel_id: Option<String> = session.try_get("cancel_operation_id")?;
            retire_session(
                &mut tx,
                &operation_id,
                &session_id,
                &finalize_id,
                cancel_id.as_deref(),
                time,
            )
            .await?;
        }
    }
    if kind == "DeleteApp" {
        sqlx::query("INSERT OR IGNORE INTO deletion_cas(operation_id, digest) SELECT ?, digest FROM artwork_bindings WHERE app_id=?")
            .bind(&operation_id).bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM channels WHERE app_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM channel_moves WHERE app_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    for release_id in &release_ids {
        if kind == "DeleteRelease" {
            sqlx::query("DELETE FROM channels WHERE release_id=?")
                .bind(release_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM channel_moves WHERE release_id=?")
                .bind(release_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM declaration_documents WHERE scope='Release' AND resource_id=?")
            .bind(release_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_tombstones(principal_id, action, key, input_hash, created_at) SELECT principal_id, action, key, input_hash, ? FROM idempotency_bindings WHERE result_id=? AND action IN ('CreateRelease', 'PublishRelease')")
            .bind(time).bind(release_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM idempotency_bindings WHERE result_id=? AND action IN ('CreateRelease', 'PublishRelease')")
            .bind(release_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM releases WHERE id=?")
            .bind(release_id)
            .execute(&mut *tx)
            .await?;
    }
    if kind == "DeleteApp" {
        sqlx::query("DELETE FROM declaration_documents WHERE scope='App' AND resource_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_tombstones(principal_id, action, key, input_hash, created_at) SELECT principal_id, action, key, input_hash, ? FROM idempotency_bindings WHERE result_id=? AND action IN ('CreateApp', 'ArtworkPut')")
            .bind(time).bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM idempotency_bindings WHERE result_id=? AND action IN ('CreateApp', 'ArtworkPut')")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM apps WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("UPDATE apps SET state=CASE WHEN EXISTS(SELECT 1 FROM releases WHERE app_id=? AND state='Published') THEN 'Published' ELSE 'Draft' END, etag_version=etag_version+1 WHERE id=?")
            .bind(&app_id).bind(&app_id).execute(&mut *tx).await?;
    }
    bind(&mut tx, &principal.id, kind, key, &hash, id, &operation_id).await?;
    tx.commit().await?;
    Ok(operation_id)
}

pub async fn delete_app(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    id: &str,
    if_match: Option<&str>,
) -> Result<String, CatalogError> {
    delete_resource(pool, principal, key, id, if_match, "DeleteApp").await
}

pub async fn delete_release(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    id: &str,
    if_match: Option<&str>,
) -> Result<String, CatalogError> {
    delete_resource(pool, principal, key, id, if_match, "DeleteRelease").await
}

pub async fn publish_release(
    pool: &SqlitePool,
    principal: &Principal,
    key: &str,
    release_id: &str,
    if_match: Option<&str>,
) -> Result<(Release, i64), CatalogError> {
    if key.is_empty() || key.len() > 255 {
        return Err(CatalogError::Invalid);
    }
    let hash = input_hash(&(release_id, if_match))?;
    let mut tx = pool.begin().await?;
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='PublishRelease' AND key=?")
        .bind(&principal.id).bind(key).fetch_one(&mut *tx).await?;
    if retired != 0 {
        return Err(CatalogError::IdempotencyConflict);
    }
    if let Some((release, version)) =
        replay(&mut tx, &principal.id, "PublishRelease", key, &hash).await?
    {
        return Ok((release, version));
    }
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let row = sqlx::query("SELECT r.app_id, r.version, r.state, r.etag_version, a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(release_id).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
    let owner: String = row.try_get("owner_id")?;
    if !principal.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    if !principal.may_publish(&owner) {
        return Err(CatalogError::Forbidden);
    }
    let grant: i64 =
        sqlx::query_scalar("SELECT server_admin OR publish_permission FROM users WHERE id=?")
            .bind(&principal.id)
            .fetch_one(&mut *tx)
            .await?;
    if grant == 0 {
        return Err(CatalogError::Forbidden);
    }
    let version: i64 = row.try_get("etag_version")?;
    if validator != etag("release", release_id, version) {
        return Err(CatalogError::StaleEtag);
    }
    if row.try_get::<String, _>("state")? != "Draft" {
        return Err(CatalogError::Conflict);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM associations WHERE release_id=?")
        .bind(release_id)
        .fetch_one(&mut *tx)
        .await?;
    if count == 0 {
        return Err(CatalogError::Conflict);
    }
    sqlx::query("UPDATE releases SET state='Published', etag_version=etag_version+1 WHERE id=?")
        .bind(release_id)
        .execute(&mut *tx)
        .await?;
    let app_id: String = row.try_get("app_id")?;
    sqlx::query("UPDATE apps SET state='Published', etag_version=etag_version+1 WHERE id=?")
        .bind(&app_id)
        .execute(&mut *tx)
        .await?;
    let release = Release {
        id: release_id.into(),
        app_id,
        state: "Published".into(),
        version: row.try_get("version")?,
    };
    bind(
        &mut tx,
        &principal.id,
        "PublishRelease",
        key,
        &hash,
        release_id,
        &(release.clone(), version + 1),
    )
    .await?;
    tx.commit().await?;
    Ok((release, version + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::bootstrap_admin, init_local, lock_local, open_catalog, upload::UploadAdapter,
        LocalMode,
    };
    use app_depot_application::upload::{
        FileClaim, Interval, Target, UploadCoordinator, UploadCreate,
    };
    use std::{fs, io::Write};

    #[tokio::test]
    async fn replay_and_publish_recheck_authorization() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "catalog", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "catalog", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        std::fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let admin_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE username='admin'")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO users(id, username, password_hash, server_admin, publish_permission) VALUES(?, 'owner', ?, 0, 1)")
            .bind(Uuid::new_v4().to_string()).bind(admin_hash).execute(&pool).await.unwrap();
        let user = crate::auth::verify_password(&pool, "owner", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let app = create_app(&pool, &user, "app-key", "My App").await.unwrap();
        assert_eq!(app.visibility, "Private");
        assert_eq!(
            app.id,
            create_app(&pool, &user, "app-key", "My App")
                .await
                .unwrap()
                .id
        );
        assert!(matches!(
            create_app(&pool, &user, "app-key", "Other").await,
            Err(CatalogError::IdempotencyConflict)
        ));
        let release = create_release(&pool, &user, "rel-key", &app.id, None)
            .await
            .unwrap();
        assert!(matches!(
            publish_release(
                &pool,
                &user,
                "pub-key",
                &release.id,
                Some(&etag("release", &release.id, 1))
            )
            .await,
            Err(CatalogError::Conflict)
        ));
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, 1, 1)")
            .bind("a".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        let artifact_id = format!("adfile:sha256:{}", "a".repeat(64));
        sqlx::query("INSERT INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', 1)")
            .bind(&artifact_id)
            .bind("a".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(Uuid::new_v4().to_string())
            .bind(&release.id)
            .bind(&artifact_id)
            .execute(&pool)
            .await
            .unwrap();
        let second = create_release(&pool, &user, "second-release", &app.id, None)
            .await
            .unwrap();
        let associated =
            create_association(&pool, &user, "association-create", &second.id, &artifact_id)
                .await
                .unwrap();
        assert_eq!(
            create_association(&pool, &user, "association-create", &second.id, &artifact_id)
                .await
                .unwrap()
                .id,
            associated.id
        );
        assert_eq!(
            list_associations(&pool, Some(&user), &second.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            delete_association(
                &pool,
                &user,
                "association-delete",
                &associated.id,
                Some("\"wrong\"")
            )
            .await,
            Err(CatalogError::StaleEtag)
        ));
        let operation_id = delete_association(
            &pool,
            &user,
            "association-delete",
            &associated.id,
            Some(&etag("association", &associated.id, 1)),
        )
        .await
        .unwrap();
        assert_eq!(
            delete_association(
                &pool,
                &user,
                "association-delete",
                &associated.id,
                Some(&etag("association", &associated.id, 1))
            )
            .await
            .unwrap(),
            operation_id
        );
        assert!(list_associations(&pool, Some(&user), &second.id)
            .await
            .unwrap()
            .is_empty());
        let kind: String = sqlx::query_scalar(
            "SELECT kind FROM deletion_operations WHERE id=? AND status='Accepted' AND physical_cleanup='Pending'",
        )
        .bind(&operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kind, "DeleteAssociation");
        let docs: i64 = sqlx::query_scalar("SELECT count(*) FROM declaration_documents WHERE scope='Association' AND resource_id=?")
            .bind(&associated.id).fetch_one(&pool).await.unwrap();
        assert_eq!(docs, 0);
        sqlx::query("UPDATE users SET publish_permission=0 WHERE id=?")
            .bind(&user.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            publish_release(
                &pool,
                &user,
                "pub-key",
                &release.id,
                Some(&etag("release", &release.id, 1))
            )
            .await,
            Err(CatalogError::Forbidden)
        ));
        sqlx::query("UPDATE users SET publish_permission=1 WHERE id=?")
            .bind(&user.id)
            .execute(&pool)
            .await
            .unwrap();
        let (published, version) = publish_release(
            &pool,
            &user,
            "pub-key",
            &release.id,
            Some(&etag("release", &release.id, 1)),
        )
        .await
        .unwrap();
        assert_eq!(published.state, "Published");
        assert_eq!(version, 2);
        assert!(matches!(
            create_association(
                &pool,
                &user,
                "published-association",
                &release.id,
                &artifact_id
            )
            .await,
            Err(CatalogError::ImmutableRelease)
        ));
        let published_association: String =
            sqlx::query_scalar("SELECT id FROM associations WHERE release_id=?")
                .bind(&release.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(matches!(
            delete_association(
                &pool,
                &user,
                "published-delete",
                &published_association,
                Some(&etag("association", &published_association, 1))
            )
            .await,
            Err(CatalogError::ImmutableRelease)
        ));
        assert_eq!(
            publish_release(
                &pool,
                &user,
                "pub-key",
                &release.id,
                Some(&etag("release", &release.id, 1))
            )
            .await
            .unwrap()
            .1,
            2
        );
    }

    #[tokio::test]
    async fn association_deletion_waits_for_shared_references_claims_and_recovery() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "deletion", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "deletion", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let actor = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let app = create_app(&pool, &actor, "deletion-app", "Deletion")
            .await
            .unwrap();
        let first = create_release(&pool, &actor, "deletion-first", &app.id, None)
            .await
            .unwrap();
        let second = create_release(&pool, &actor, "deletion-second", &app.id, None)
            .await
            .unwrap();
        let bytes = b"shared CAS content";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let artifact = format!("adfile:sha256:{digest}");
        let parent = lock.path.join("cas").join(&digest[..2]);
        fs::create_dir_all(&parent).unwrap();
        let cas_path = parent.join(&digest);
        let mut file = fs::File::create(&cas_path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        fs::File::open(&parent).unwrap().sync_all().unwrap();
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1)")
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', ?)")
            .bind(&artifact)
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        let first_association = Uuid::new_v4().to_string();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(&first_association)
            .bind(&first.id)
            .bind(&artifact)
            .execute(&mut *tx)
            .await
            .unwrap();
        create_initial_declaration(&mut tx, "Association", &first_association)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let second_association =
            create_association(&pool, &actor, "second-association", &second.id, &artifact)
                .await
                .unwrap();
        let (document, version) =
            crate::declarations::get(&pool, Some(&actor), "Association", &second_association.id)
                .await
                .unwrap();
        assert!(document.data.is_object());
        crate::declarations::put(
            &pool,
            &actor,
            "Association",
            &second_association.id,
            Some(&etag("declarations", &second_association.id, version)),
            &serde_json::json!({"test":true}),
        )
        .await
        .unwrap();
        let first_operation = delete_association(
            &pool,
            &actor,
            "delete-first",
            &first_association,
            Some(&etag("association", &first_association, 1)),
        )
        .await
        .unwrap();
        let adapter = UploadAdapter::new(pool.clone(), lock.path.clone());
        let coordinator = UploadCoordinator::new(adapter.clone());
        coordinator
            .maintenance(crate::auth::now() - 86400)
            .await
            .unwrap();
        let first_status: String =
            sqlx::query_scalar("SELECT status FROM deletion_operations WHERE id=?")
                .bind(&first_operation)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(first_status, "Completed");
        assert!(cas_path.exists());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artifacts WHERE id=?")
                .bind(&artifact)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        let claim_operation = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO operations(id, owner_id, kind, status) VALUES(?, ?, 'Finalize', 'Accepted')")
            .bind(&claim_operation).bind(&actor.id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO prepared_claims(operation_id, digest) VALUES(?, ?)")
            .bind(&claim_operation)
            .bind(&digest)
            .execute(&pool)
            .await
            .unwrap();
        let second_operation = delete_association(
            &pool,
            &actor,
            "delete-second",
            &second_association.id,
            Some(&etag("association", &second_association.id, 1)),
        )
        .await
        .unwrap();
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM declaration_revisions WHERE scope='Association' AND resource_id=?")
            .bind(&second_association.id).fetch_one(&pool).await.unwrap(), 0);
        adapter.fail_deletion_cleanup_once();
        coordinator
            .maintenance(crate::auth::now() - 86400)
            .await
            .unwrap();
        let failed: (String, String) =
            sqlx::query_as("SELECT status, physical_cleanup FROM deletion_operations WHERE id=?")
                .bind(&second_operation)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(failed, ("Running".into(), "RetryableFailure".into()));
        assert!(cas_path.exists());
        drop(coordinator);
        let restarted = UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        restarted.recover().await.unwrap();
        let pending: String =
            sqlx::query_scalar("SELECT status FROM deletion_operations WHERE id=?")
                .bind(&second_operation)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending, "Running");
        assert!(cas_path.exists());
        sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
            .bind(&claim_operation)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM operations WHERE id=?")
            .bind(&claim_operation)
            .execute(&pool)
            .await
            .unwrap();
        restarted.recover().await.unwrap();
        let completed: (String, String) =
            sqlx::query_as("SELECT status, physical_cleanup FROM deletion_operations WHERE id=?")
                .bind(&second_operation)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(completed, ("Completed".into(), "Completed".into()));
        assert!(!cas_path.exists());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artifacts WHERE id=?")
                .bind(&artifact)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );

        let mut file = fs::File::create(&cas_path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        fs::File::open(&parent).unwrap().sync_all().unwrap();
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1)")
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        let upload = restarted
            .create(
                &actor,
                "concurrent-create",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Concurrent".into(),
                    },
                    file: Some(FileClaim {
                        size: bytes.len() as u64,
                        sha256: digest.clone(),
                    }),
                },
            )
            .await
            .unwrap();
        let mut input: &[u8] = bytes;
        restarted
            .receive(
                &actor,
                &upload.id,
                &upload.file().file_id,
                Interval {
                    start: 0,
                    end: bytes.len() as u64,
                },
                &mut input,
            )
            .await
            .unwrap();
        let (finalized, maintenance) = tokio::join!(
            restarted.finalize(&actor, &upload.id, "concurrent-finalize"),
            restarted.maintenance(crate::auth::now() - 86400)
        );
        maintenance.unwrap();
        assert_eq!(finalized.unwrap().phase, "Completed");
        assert_eq!(fs::read(&cas_path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn release_and_app_deletion_revoke_access_and_clean_dependents() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "catalog-delete", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "catalog-delete", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let actor = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let app = create_app(&pool, &actor, "delete-app-create", "To delete")
            .await
            .unwrap();
        let published = create_release(&pool, &actor, "delete-release-create", &app.id, None)
            .await
            .unwrap();
        let draft = create_release(&pool, &actor, "delete-draft-create", &app.id, None)
            .await
            .unwrap();
        let bytes = b"published file";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let artifact = format!("adfile:sha256:{digest}");
        let parent = lock.path.join("cas").join(&digest[..2]);
        fs::create_dir_all(&parent).unwrap();
        let cas_path = parent.join(&digest);
        let mut file = fs::File::create(&cas_path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1)")
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', ?)")
            .bind(&artifact)
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        let association_id = Uuid::new_v4().to_string();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(&association_id)
            .bind(&published.id)
            .bind(&artifact)
            .execute(&mut *tx)
            .await
            .unwrap();
        create_initial_declaration(&mut tx, "Association", &association_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        publish_release(
            &pool,
            &actor,
            "delete-release-publish",
            &published.id,
            Some(&etag("release", &published.id, 1)),
        )
        .await
        .unwrap();
        crate::channels::put(
            &pool,
            &actor,
            &app.id,
            "stable",
            &published.id,
            None,
            Some("*"),
        )
        .await
        .unwrap();
        let release_operation = delete_release(
            &pool,
            &actor,
            "delete-release-key",
            &published.id,
            Some(&etag("release", &published.id, 2)),
        )
        .await
        .unwrap();
        assert_eq!(
            delete_release(
                &pool,
                &actor,
                "delete-release-key",
                &published.id,
                Some(&etag("release", &published.id, 2))
            )
            .await
            .unwrap(),
            release_operation
        );
        assert!(matches!(
            get_release(&pool, Some(&actor), &published.id).await,
            Err(CatalogError::NotFound)
        ));
        assert_eq!(
            get_app(&pool, Some(&actor), &app.id).await.unwrap().0.state,
            "Draft"
        );
        assert!(matches!(
            crate::channels::get(&pool, Some(&actor), &app.id, "stable").await,
            Err(CatalogError::NotFound)
        ));
        assert_eq!(
            crate::channels::history(&pool, &actor, &app.id, "stable")
                .await
                .unwrap()
                .len(),
            0
        );
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        coordinator
            .maintenance(crate::auth::now() - 86400)
            .await
            .unwrap();
        assert!(!cas_path.exists());
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM deletion_operations WHERE id=?")
                .bind(&release_operation)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "Completed"
        );
        let app_operation = delete_app(
            &pool,
            &actor,
            "delete-app-key",
            &app.id,
            Some(&etag("app", &app.id, 3)),
        )
        .await
        .unwrap();
        assert!(matches!(
            get_app(&pool, Some(&actor), &app.id).await,
            Err(CatalogError::NotFound)
        ));
        assert!(matches!(
            get_release(&pool, Some(&actor), &draft.id).await,
            Err(CatalogError::NotFound)
        ));
        coordinator
            .maintenance(crate::auth::now() - 86400)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM deletion_operations WHERE id=?")
                .bind(&app_operation)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "Completed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM declaration_documents WHERE resource_id IN (?, ?, ?, ?)"
            )
            .bind(&app.id)
            .bind(&published.id)
            .bind(&draft.id)
            .bind(&association_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert!(matches!(
            create_app(&pool, &actor, "delete-app-create", "To delete").await,
            Err(CatalogError::IdempotencyConflict)
        ));
    }
}
