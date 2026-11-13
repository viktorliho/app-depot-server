pub use app_depot_application::Principal;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
use std::{fs, path::Path};
use uuid::Uuid;

use crate::catalog::{etag, CatalogError};
use crate::StorageError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub server_admin: bool,
    pub publish_permission: bool,
}

pub async fn list_users(
    pool: &SqlitePool,
    actor: &Principal,
) -> Result<Vec<UserRecord>, CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let rows =
        sqlx::query("SELECT id, username, server_admin, publish_permission FROM users ORDER BY id")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|row| UserRecord {
            id: row.get("id"),
            username: row.get("username"),
            server_admin: row.get::<i64, _>("server_admin") != 0,
            publish_permission: row.get::<i64, _>("publish_permission") != 0,
        })
        .collect())
}

pub async fn get_user(
    pool: &SqlitePool,
    actor: &Principal,
    id: &str,
) -> Result<(UserRecord, i64), CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let row = sqlx::query(
        "SELECT id, username, server_admin, publish_permission, etag_version FROM users WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(CatalogError::NotFound)?;
    Ok((
        UserRecord {
            id: row.get("id"),
            username: row.get("username"),
            server_admin: row.get::<i64, _>("server_admin") != 0,
            publish_permission: row.get::<i64, _>("publish_permission") != 0,
        },
        row.get("etag_version"),
    ))
}

pub async fn create_user(
    pool: &SqlitePool,
    actor: &Principal,
    key: &str,
    username: &str,
    password: &str,
    publish_permission: bool,
) -> Result<UserRecord, CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    if key.is_empty()
        || key.len() > 255
        || username.is_empty()
        || username.len() > 255
        || password.is_empty()
        || password.len() > 1024
    {
        return Err(CatalogError::Invalid);
    }
    let input_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            username,
            password,
            publish_permission
        ))?)
    );
    let mut tx = pool.begin().await?;
    if let Some(row) = sqlx::query("SELECT input_hash, result_json FROM idempotency_bindings WHERE principal_id=? AND action='CreateUser' AND key=?")
        .bind(&actor.id).bind(key).fetch_optional(&mut *tx).await? {
        if row.get::<String, _>("input_hash") != input_hash { return Err(CatalogError::IdempotencyConflict); }
        return Ok(serde_json::from_str(&row.get::<String, _>("result_json"))?);
    }
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username=?")
        .bind(username)
        .fetch_one(&mut *tx)
        .await?;
    if existing != 0 {
        return Err(CatalogError::Conflict);
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| CatalogError::Invalid)?
        .to_string();
    let user = UserRecord {
        id: Uuid::new_v4().to_string(),
        username: username.into(),
        server_admin: false,
        publish_permission,
    };
    sqlx::query("INSERT INTO users(id, username, password_hash, server_admin, publish_permission) VALUES(?, ?, ?, 0, ?)")
        .bind(&user.id).bind(username).bind(password_hash).bind(publish_permission as i64).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CreateUser', ?, ?, ?, ?, ?)")
        .bind(&actor.id).bind(key).bind(input_hash).bind(&user.id).bind(serde_json::to_string(&user)?).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(user)
}

pub async fn patch_user(
    pool: &SqlitePool,
    actor: &Principal,
    id: &str,
    if_match: Option<&str>,
    publish: Option<bool>,
    admin: Option<bool>,
) -> Result<(UserRecord, i64), CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT username, server_admin, publish_permission, etag_version FROM users WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let version: i64 = row.get("etag_version");
    if validator != etag("user", id, version) {
        return Err(CatalogError::StaleEtag);
    }
    let user = UserRecord {
        id: id.into(),
        username: row.get("username"),
        server_admin: admin.unwrap_or(row.get::<i64, _>("server_admin") != 0),
        publish_permission: publish.unwrap_or(row.get::<i64, _>("publish_permission") != 0),
    };
    sqlx::query("UPDATE users SET server_admin=?, publish_permission=?, etag_version=etag_version+1 WHERE id=?")
        .bind(user.server_admin as i64).bind(user.publish_permission as i64).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((user, version + 1))
}

pub async fn revoke_user_token(
    pool: &SqlitePool,
    actor: &Principal,
    id: &str,
) -> Result<(), CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let result = sqlx::query("UPDATE users SET token_hash=NULL, token_issued_at=NULL WHERE id=?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(CatalogError::NotFound);
    }
    Ok(())
}

pub async fn get_settings(
    pool: &SqlitePool,
    actor: &Principal,
) -> Result<(bool, i64), CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let row =
        sqlx::query("SELECT anonymous_read_enabled, etag_version FROM server_settings WHERE id=1")
            .fetch_one(pool)
            .await?;
    Ok((
        row.get::<i64, _>("anonymous_read_enabled") != 0,
        row.get("etag_version"),
    ))
}

pub async fn patch_settings(
    pool: &SqlitePool,
    actor: &Principal,
    if_match: Option<&str>,
    enabled: Option<bool>,
) -> Result<(bool, i64), CatalogError> {
    if !actor.server_admin {
        return Err(CatalogError::Forbidden);
    }
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let row =
        sqlx::query("SELECT anonymous_read_enabled, etag_version FROM server_settings WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
    let version: i64 = row.get("etag_version");
    if validator != etag("settings", "1", version) {
        return Err(CatalogError::StaleEtag);
    }
    let value = enabled.unwrap_or(row.get::<i64, _>("anonymous_read_enabled") != 0);
    sqlx::query("UPDATE server_settings SET anonymous_read_enabled=?, etag_version=etag_version+1 WHERE id=1")
        .bind(value as i64).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((value, version + 1))
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn hash_secret(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Rotate the one-time challenge at each startup. Only its digest is durable.
pub async fn prepare_setup(pool: &SqlitePool) -> Result<Option<String>, StorageError> {
    let mut tx = pool.begin().await?;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    let completed: Option<String> =
        sqlx::query_scalar("SELECT value FROM instance_meta WHERE key='setup_complete'")
            .fetch_optional(&mut *tx)
            .await?;
    if users != 0 || completed.is_some() {
        if users != 0 {
            sqlx::query("INSERT INTO instance_meta(key,value) VALUES('setup_complete','1') ON CONFLICT(key) DO NOTHING")
                .execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM instance_meta WHERE key IN ('setup_token_hash','setup_attempts')")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    let token = random_secret();
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    sqlx::query("INSERT INTO instance_meta(key,value) VALUES('setup_token_hash',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(hash).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO instance_meta(key,value) VALUES('setup_attempts','0') ON CONFLICT(key) DO UPDATE SET value='0'")
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(token))
}

pub async fn setup_required(pool: &SqlitePool) -> Result<bool, StorageError> {
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?;
    let completed: Option<String> =
        sqlx::query_scalar("SELECT value FROM instance_meta WHERE key='setup_complete'")
            .fetch_optional(pool)
            .await?;
    Ok(users == 0 && completed.is_none())
}

pub async fn complete_setup(
    pool: &SqlitePool,
    token: &str,
    username: &str,
    password: &str,
) -> Result<bool, StorageError> {
    if token.len() != 64
        || username.is_empty()
        || username.len() > 255
        || password.len() < 16
        || password.len() > 1024
    {
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    let completed: Option<String> =
        sqlx::query_scalar("SELECT value FROM instance_meta WHERE key='setup_complete'")
            .fetch_optional(&mut *tx)
            .await?;
    if users != 0 || completed.is_some() {
        return Ok(false);
    }
    let hash: Option<String> =
        sqlx::query_scalar("SELECT value FROM instance_meta WHERE key='setup_token_hash'")
            .fetch_optional(&mut *tx)
            .await?;
    let attempts: i64 = sqlx::query_scalar(
        "SELECT CAST(value AS INTEGER) FROM instance_meta WHERE key='setup_attempts'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if attempts >= 10 {
        return Ok(false);
    }
    let supplied = Sha256::digest(token.as_bytes());
    let expected = hash.as_deref().and_then(|value| hex::decode(value).ok());
    if expected.as_deref() != Some(supplied.as_slice()) {
        sqlx::query(
            "UPDATE instance_meta SET value=CAST(value AS INTEGER)+1 WHERE key='setup_attempts'",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(false);
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| StorageError::InvalidRoot("password verifier creation failed"))?
        .to_string();
    sqlx::query("INSERT INTO users(id, username, password_hash, server_admin, publish_permission) VALUES(?, ?, ?, 1, 1)")
        .bind(Uuid::new_v4().to_string()).bind(username).bind(password_hash).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO instance_meta(key,value) VALUES('setup_complete','1')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM instance_meta WHERE key IN ('setup_token_hash','setup_attempts')")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
pub async fn bootstrap_admin(
    pool: &SqlitePool,
    secret_file: Option<&Path>,
) -> Result<(), StorageError> {
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?;
    if users != 0 {
        return Ok(());
    }
    let path = secret_file.ok_or(StorageError::InvalidRoot(
        "first startup requires --bootstrap-secret-file",
    ))?;
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StorageError::InvalidRoot(
            "bootstrap secret must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(StorageError::InvalidRoot(
                "bootstrap secret file must be owner-only",
            ));
        }
    }
    let raw = fs::read_to_string(path)?;
    let secret = raw.strip_suffix("\n").unwrap_or(&raw);
    bootstrap_admin_value(pool, Some(secret)).await
}

#[cfg(test)]
pub async fn bootstrap_admin_value(
    pool: &SqlitePool,
    secret: Option<&str>,
) -> Result<(), StorageError> {
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?;
    if users != 0 {
        return Ok(());
    }
    let secret = secret.ok_or(StorageError::InvalidRoot(
        "first startup requires a bootstrap secret",
    ))?;
    if secret.len() < 16 || secret.len() > 1024 || secret.trim().len() != secret.len() {
        return Err(StorageError::InvalidRoot(
            "bootstrap secret must be 16–1024 nonspace-boundary characters",
        ));
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|_| StorageError::InvalidRoot("password verifier creation failed"))?
        .to_string();
    sqlx::query("INSERT INTO users(id, username, password_hash, server_admin, publish_permission) VALUES(?, 'admin', ?, 1, 1)")
        .bind(Uuid::new_v4().to_string()).bind(hash).execute(pool).await?;
    Ok(())
}

pub async fn verify_password(
    pool: &SqlitePool,
    username: &str,
    password: &str,
) -> Result<Option<Principal>, StorageError> {
    let row = sqlx::query("SELECT id, username, password_hash, server_admin, publish_permission FROM users WHERE username = ?")
        .bind(username).fetch_optional(pool).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let hash: String = row.try_get("password_hash")?;
    let parsed = PasswordHash::new(&hash)
        .map_err(|_| StorageError::InvalidRoot("stored password verifier invalid"))?;
    if Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_err()
    {
        return Ok(None);
    }
    Ok(Some(Principal {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        server_admin: row.try_get::<i64, _>("server_admin")? != 0,
        publish_permission: row.try_get::<i64, _>("publish_permission")? != 0,
    }))
}

pub async fn create_browser_session(
    pool: &SqlitePool,
    user_id: &str,
    session: &str,
    csrf: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO browser_sessions(id_hash, user_id, csrf_hash, expires_at) VALUES(?, ?, ?, ?)",
    )
    .bind(hash_secret(session))
    .bind(user_id)
    .bind(hash_secret(csrf))
    .bind(now() + 12 * 3600)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn browser_principal(
    pool: &SqlitePool,
    session: &str,
) -> Result<Option<Principal>, StorageError> {
    let row = sqlx::query("SELECT u.id, u.username, u.server_admin, u.publish_permission FROM browser_sessions s JOIN users u ON u.id=s.user_id WHERE s.id_hash=? AND s.expires_at>?")
        .bind(hash_secret(session)).bind(now()).fetch_optional(pool).await?;
    Ok(row.map(|row| Principal {
        id: row.get("id"),
        username: row.get("username"),
        server_admin: row.get::<i64, _>("server_admin") != 0,
        publish_permission: row.get::<i64, _>("publish_permission") != 0,
    }))
}

pub async fn csrf_valid(
    pool: &SqlitePool,
    session: &str,
    csrf: &str,
) -> Result<bool, StorageError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM browser_sessions WHERE id_hash=? AND csrf_hash=? AND expires_at>?",
    )
    .bind(hash_secret(session))
    .bind(hash_secret(csrf))
    .bind(now())
    .fetch_one(pool)
    .await?;
    Ok(count == 1)
}

pub async fn revoke_browser_session(pool: &SqlitePool, session: &str) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM browser_sessions WHERE id_hash=?")
        .bind(hash_secret(session))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn issue_token(pool: &SqlitePool, user_id: &str) -> Result<(String, i64), StorageError> {
    let token = random_secret();
    let issued = now();
    sqlx::query(
        "UPDATE users SET token_hash=?, token_issued_at=datetime(?, 'unixepoch') WHERE id=?",
    )
    .bind(hash_secret(&token))
    .bind(issued)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok((token, issued))
}

pub async fn bearer_principal(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<Principal>, StorageError> {
    let row = sqlx::query(
        "SELECT id, username, server_admin, publish_permission FROM users WHERE token_hash=?",
    )
    .bind(hash_secret(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Principal {
        id: row.get("id"),
        username: row.get("username"),
        server_admin: row.get::<i64, _>("server_admin") != 0,
        publish_permission: row.get::<i64, _>("publish_permission") != 0,
    }))
}

pub async fn token_metadata(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<(bool, Option<String>), StorageError> {
    let row = sqlx::query("SELECT token_hash, strftime('%Y-%m-%dT%H:%M:%SZ', token_issued_at) AS issued_at FROM users WHERE id=?")
        .bind(user_id).fetch_one(pool).await?;
    let active = row.try_get::<Option<String>, _>("token_hash")?.is_some();
    let issued_at = row.try_get("issued_at")?;
    Ok((active, issued_at))
}

pub async fn revoke_token(pool: &SqlitePool, user_id: &str) -> Result<(), StorageError> {
    sqlx::query("UPDATE users SET token_hash=NULL, token_issued_at=NULL WHERE id=?")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_local, lock_local, open_catalog, LocalMode};

    #[tokio::test]
    async fn initial_setup_rotates_and_only_one_concurrent_admin_wins() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "setup", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "setup", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let old = prepare_setup(&pool).await.unwrap().unwrap();
        assert!(setup_required(&pool).await.unwrap());
        let digest: String =
            sqlx::query_scalar("SELECT value FROM instance_meta WHERE key='setup_token_hash'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(digest, old);
        let new = prepare_setup(&pool).await.unwrap().unwrap();
        assert_ne!(new, old);
        assert!(
            !complete_setup(&pool, &old, "admin", "safe secret phrase 12345")
                .await
                .unwrap()
        );
        let a = complete_setup(&pool, &new, "admin", "safe secret phrase 12345");
        let b = complete_setup(&pool, &new, "other", "safe secret phrase 12345");
        let (a, b) = tokio::join!(a, b);
        assert_ne!(a.unwrap(), b.unwrap());
        assert!(!setup_required(&pool).await.unwrap());
        assert!(prepare_setup(&pool).await.unwrap().is_none());
        assert!(
            !complete_setup(&pool, &new, "extra", "safe secret phrase 12345")
                .await
                .unwrap()
        );
        let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 1);
        pool.close().await;
        let reopened = open_catalog(&lock).await.unwrap();
        assert!(!setup_required(&reopened).await.unwrap());
        assert!(prepare_setup(&reopened).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn setup_attempt_limit_requires_restart_rotation() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "attempts", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "attempts", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let token = prepare_setup(&pool).await.unwrap().unwrap();
        let wrong = "0".repeat(64);
        for _ in 0..10 {
            assert!(
                !complete_setup(&pool, &wrong, "admin", "safe secret phrase 12345")
                    .await
                    .unwrap()
            );
        }
        assert!(
            !complete_setup(&pool, &token, "admin", "safe secret phrase 12345")
                .await
                .unwrap()
        );
        let rotated = prepare_setup(&pool).await.unwrap().unwrap();
        assert_ne!(rotated, token);
        assert!(
            complete_setup(&pool, &rotated, "admin", "safe secret phrase 12345")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn bootstrap_and_credentials_are_one_time_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "auth", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "auth", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        assert!(bootstrap_admin(&pool, None).await.is_err());
        let path = dir.path().join("bootstrap-secret");
        fs::write(&path, "correct horse battery staple\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&path)).await.unwrap();
        fs::write(&path, "changed secret phrase\n").unwrap();
        bootstrap_admin(&pool, Some(&path)).await.unwrap();
        assert!(verify_password(&pool, "admin", "changed secret phrase")
            .await
            .unwrap()
            .is_none());
        let principal = verify_password(&pool, "admin", "correct horse battery staple")
            .await
            .unwrap()
            .unwrap();
        assert!(principal.server_admin && principal.publish_permission);
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=?")
            .bind(&principal.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!hash.contains("correct horse"));
        let (first, _) = issue_token(&pool, &principal.id).await.unwrap();
        assert!(bearer_principal(&pool, &first).await.unwrap().is_some());
        let (second, _) = issue_token(&pool, &principal.id).await.unwrap();
        assert!(bearer_principal(&pool, &first).await.unwrap().is_none());
        assert!(bearer_principal(&pool, &second).await.unwrap().is_some());
        let reader = create_user(
            &pool,
            &principal,
            "create-reader",
            "reader",
            "reader password 12345",
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            create_user(
                &pool,
                &principal,
                "create-reader",
                "reader",
                "reader password 12345",
                false
            )
            .await
            .unwrap()
            .id,
            reader.id
        );
        assert!(matches!(
            create_user(
                &pool,
                &principal,
                "create-reader",
                "reader",
                "other password",
                false
            )
            .await,
            Err(CatalogError::IdempotencyConflict)
        ));
        assert!(verify_password(&pool, "reader", "reader password 12345")
            .await
            .unwrap()
            .is_some());
        let reader_principal = verify_password(&pool, "reader", "reader password 12345")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            list_users(&pool, &reader_principal).await,
            Err(CatalogError::Forbidden)
        ));
        let (updated, version) = patch_user(
            &pool,
            &principal,
            &reader.id,
            Some(&etag("user", &reader.id, 1)),
            Some(true),
            None,
        )
        .await
        .unwrap();
        assert!(updated.publish_permission);
        assert_eq!(version, 2);
        assert!(matches!(
            patch_user(
                &pool,
                &principal,
                &reader.id,
                Some(&etag("user", &reader.id, 1)),
                Some(false),
                None
            )
            .await,
            Err(CatalogError::StaleEtag)
        ));
        let (reader_token, _) = issue_token(&pool, &reader.id).await.unwrap();
        revoke_user_token(&pool, &principal, &reader.id)
            .await
            .unwrap();
        assert!(bearer_principal(&pool, &reader_token)
            .await
            .unwrap()
            .is_none());
        assert_eq!(get_settings(&pool, &principal).await.unwrap(), (false, 1));
        assert_eq!(
            patch_settings(
                &pool,
                &principal,
                Some(&etag("settings", "1", 1)),
                Some(true)
            )
            .await
            .unwrap(),
            (true, 2)
        );
        assert!(matches!(
            patch_settings(
                &pool,
                &principal,
                Some(&etag("settings", "1", 1)),
                Some(false)
            )
            .await,
            Err(CatalogError::StaleEtag)
        ));
    }
}
