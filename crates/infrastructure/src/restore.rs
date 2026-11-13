use crate::backups::verify_directory;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    str::FromStr,
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("invalid or incompatible backup")]
    InvalidBackup,
    #[error("target is active or unsafe")]
    UnsafeTarget,
    #[error("restore requires --replace for a nonempty target")]
    ReplaceRequired,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
}

fn sync_dir(path: &Path) -> Result<(), RestoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), RestoreError> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn member_matches(path: &Path, size: u64, digest: &str) -> Result<bool, RestoreError> {
    use std::io::Read;
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != size {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or(RestoreError::InvalidBackup)?;
        hasher.update(&buffer[..read]);
    }
    Ok(count == size && format!("{:x}", hasher.finalize()) == digest)
}

async fn inspect_catalog(
    path: &Path,
    expected_version: i64,
    manifest_cas: &HashSet<String>,
) -> Result<(), RestoreError> {
    let options = SqliteConnectOptions::from_str("sqlite:catalog.sqlite")?
        .filename(path)
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        return Err(RestoreError::InvalidBackup);
    }
    let foreign: Vec<sqlx::sqlite::SqliteRow> = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await?;
    if !foreign.is_empty() {
        return Err(RestoreError::InvalidBackup);
    }
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    if version != expected_version {
        return Err(RestoreError::InvalidBackup);
    }
    let rows = sqlx::query("SELECT digest FROM artifacts WHERE digest IS NOT NULL UNION SELECT sha256 AS digest FROM artifact_entries WHERE sha256 IS NOT NULL")
        .fetch_all(&pool).await?;
    let required: HashSet<String> = rows.into_iter().map(|row| row.get("digest")).collect();
    if !required.is_subset(manifest_cas) {
        return Err(RestoreError::InvalidBackup);
    }
    pool.close().await;
    Ok(())
}

async fn invalidated_catalog(path: &Path) -> Result<(), RestoreError> {
    let options = SqliteConnectOptions::from_str("sqlite:catalog.sqlite")?
        .filename(path)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
    let pool: SqlitePool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let mut tx = pool.begin().await?;
    let problem = r#"{"type":"urn:app-depot:problem:session_cancelled","title":"Upload session cancelled","status":410,"code":"session_cancelled","cancellationReason":"RestoreInvalidated"}"#;
    let rows = sqlx::query("SELECT id, operation_id FROM upload_sessions WHERE phase IN ('Receiving','Verifying','Importing','Committing')")
        .fetch_all(&mut *tx).await?;
    for row in rows {
        let session_id: String = row.get("id");
        let operation_id: String = row.get("operation_id");
        let result = serde_json::json!({"sessionId":session_id,"operationId":operation_id,
            "phase":"Cancelled","cancellationReason":"RestoreInvalidated","problem":serde_json::from_str::<serde_json::Value>(problem).map_err(|_| RestoreError::InvalidBackup)?});
        sqlx::query("UPDATE upload_sessions SET phase='Cancelled', terminal_code='session_cancelled', cancellation_reason='RestoreInvalidated', progress_version=progress_version+1, updated_at=strftime('%s','now') WHERE id=?")
            .bind(&session_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE operations SET status='Failed', result_json=?, problem_json=? WHERE id=? AND status!='Completed'")
            .bind(result.to_string()).bind(problem).bind(&operation_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM upload_ranges WHERE session_id=?")
            .bind(&session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
            .bind(&operation_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM backup_pins")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        return Err(RestoreError::InvalidBackup);
    }
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

pub async fn restore(
    backup_dir: &Path,
    data_root: &Path,
    replace: bool,
) -> Result<(), RestoreError> {
    if !data_root.is_absolute() {
        return Err(RestoreError::UnsafeTarget);
    }
    let parent = data_root
        .parent()
        .ok_or(RestoreError::UnsafeTarget)?
        .canonicalize()?;
    let name = data_root.file_name().ok_or(RestoreError::UnsafeTarget)?;
    let name_text = name.to_str().ok_or(RestoreError::UnsafeTarget)?;
    let target = parent.join(name);
    if target == parent || target == Path::new("/") {
        return Err(RestoreError::UnsafeTarget);
    }
    let parent_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(".app-depot-restore-lock"))?;
    parent_lock
        .try_lock_exclusive()
        .map_err(|_| RestoreError::UnsafeTarget)?;
    let marker = parent.join(format!(".restore-{name_text}"));
    if marker.exists() {
        return Err(RestoreError::UnsafeTarget);
    }
    let mut target_lock = None;
    if target.exists() {
        let meta = target.symlink_metadata()?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(RestoreError::UnsafeTarget);
        }
        let nonempty = fs::read_dir(&target)?.next().is_some();
        if nonempty && !replace {
            return Err(RestoreError::ReplaceRequired);
        }
        if nonempty {
            let lock_path = target.join(".app-depot-lock");
            if !lock_path.is_file() || lock_path.symlink_metadata()?.file_type().is_symlink() {
                return Err(RestoreError::UnsafeTarget);
            }
            let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
            lock.try_lock_exclusive()
                .map_err(|_| RestoreError::UnsafeTarget)?;
            target_lock = Some(lock);
        }
    }
    let manifest = verify_directory(backup_dir).map_err(|_| RestoreError::InvalidBackup)?;
    let max_version = sqlx::migrate!("./migrations")
        .migrations
        .last()
        .map(|m| m.version)
        .ok_or(RestoreError::InvalidBackup)?;
    if manifest.catalog_format_version > max_version {
        return Err(RestoreError::InvalidBackup);
    }
    let sqlite_member = manifest
        .members
        .iter()
        .find(|m| m.kind == "SqliteSnapshot")
        .ok_or(RestoreError::InvalidBackup)?;
    if sqlite_member.relative_path != "catalog.sqlite" {
        return Err(RestoreError::InvalidBackup);
    }
    let cas_set: HashSet<String> = manifest
        .members
        .iter()
        .filter(|m| m.kind == "CasObject")
        .map(|m| m.sha256.clone())
        .collect();
    inspect_catalog(
        &backup_dir.join("catalog.sqlite"),
        manifest.catalog_format_version,
        &cas_set,
    )
    .await?;
    let candidate = parent.join(format!(".app-depot-restore-{}", Uuid::new_v4()));
    private_dir(&candidate)?;
    let prepared = async {
        for part in ["db", "cas", "staging", "backups", "ops"] {
            private_dir(&candidate.join(part))?;
        }
        let mode_marker = if target.join(".app-depot-instance").is_file() {
            fs::read(target.join(".app-depot-instance"))?
        } else {
            br#"{"format":1,"mode":"Production"}"#.to_vec()
        };
        let mut f = File::create_new(candidate.join(".app-depot-instance"))?;
        f.write_all(&mode_marker)?;
        f.sync_all()?;
        let mut lock = File::create_new(candidate.join(".app-depot-lock"))?;
        lock.flush()?;
        lock.sync_all()?;
        fs::copy(
            backup_dir.join("catalog.sqlite"),
            candidate.join("db/catalog.sqlite"),
        )?;
        File::open(candidate.join("db/catalog.sqlite"))?.sync_all()?;
        if !member_matches(
            &candidate.join("db/catalog.sqlite"),
            sqlite_member.size,
            &sqlite_member.sha256,
        )? {
            return Err(RestoreError::InvalidBackup);
        }
        for member in manifest.members.iter().filter(|m| m.kind == "CasObject") {
            let digest = &member.sha256;
            let fanout = candidate.join("cas").join(&digest[..2]);
            if !fanout.exists() {
                fs::create_dir(&fanout)?;
            }
            fs::copy(backup_dir.join(&member.relative_path), fanout.join(digest))?;
            File::open(fanout.join(digest))?.sync_all()?;
            if !member_matches(&fanout.join(digest), member.size, digest)? {
                return Err(RestoreError::InvalidBackup);
            }
        }
        for entry in fs::read_dir(candidate.join("cas"))? {
            sync_dir(&entry?.path())?;
        }
        sync_dir(&candidate.join("cas"))?;
        invalidated_catalog(&candidate.join("db/catalog.sqlite")).await?;
        sync_dir(&candidate.join("db"))?;
        sync_dir(&candidate)?;
        Ok::<(), RestoreError>(())
    }
    .await;
    if let Err(error) = prepared {
        fs::remove_dir_all(&candidate)?;
        return Err(error);
    }
    let old = parent.join(format!(".app-depot-prior-{}", Uuid::new_v4()));
    let mut marker_file = File::create_new(&marker)?;
    marker_file.write_all(
        serde_json::json!({"version":1,"target":name_text,
        "prior":old.file_name().unwrap().to_string_lossy(),
        "candidate":candidate.file_name().unwrap().to_string_lossy()})
        .to_string()
        .as_bytes(),
    )?;
    marker_file.sync_all()?;
    sync_dir(&parent)?;
    if target.exists() {
        if let Err(error) = fs::rename(&target, &old) {
            fs::remove_file(&marker)?;
            fs::remove_dir_all(&candidate)?;
            sync_dir(&parent)?;
            return Err(error.into());
        }
        sync_dir(&parent)?;
    }
    if let Err(error) = fs::rename(&candidate, &target) {
        if old.exists() {
            fs::rename(&old, &target)?;
            sync_dir(&parent)?;
        }
        if candidate.exists() {
            fs::remove_dir_all(&candidate)?;
        }
        fs::remove_file(&marker)?;
        sync_dir(&parent)?;
        return Err(error.into());
    }
    sync_dir(&parent)?;
    fs::remove_file(marker)?;
    sync_dir(&parent)?;
    drop(target_lock);
    if old.exists() {
        fs::remove_dir_all(old)?;
        sync_dir(&parent)?;
    }
    Ok(())
}

pub async fn diagnose(data_root: &Path) -> Result<serde_json::Value, RestoreError> {
    if !data_root.is_absolute() {
        return Err(RestoreError::UnsafeTarget);
    }
    let marker = data_root
        .parent()
        .ok_or(RestoreError::UnsafeTarget)?
        .join(format!(
            ".restore-{}",
            data_root
                .file_name()
                .ok_or(RestoreError::UnsafeTarget)?
                .to_string_lossy()
        ));
    if marker.exists() {
        return Ok(serde_json::json!({"status":"Corrupt","sqlite":"ActivationIncomplete"}));
    }
    let lock_path = data_root.join(".app-depot-lock");
    let lock = match OpenOptions::new().read(true).write(true).open(lock_path) {
        Ok(lock) => lock,
        Err(_) => {
            return Ok(serde_json::json!({"status":"Corrupt","sqlite":"DataRootUnavailable"}))
        }
    };
    lock.try_lock_exclusive()
        .map_err(|_| RestoreError::UnsafeTarget)?;
    let db = data_root.join("db/catalog.sqlite");
    if !db.is_file() {
        return Ok(serde_json::json!({"status":"Corrupt","sqlite":"Missing"}));
    }
    let options = SqliteConnectOptions::from_str("sqlite:catalog.sqlite")?
        .filename(db)
        .read_only(true);
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(_) => return Ok(serde_json::json!({"status":"Corrupt","sqlite":"OpenFailure"})),
    };
    let check: Result<String, sqlx::Error> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await;
    if !matches!(check.as_deref(), Ok("ok")) {
        pool.close().await;
        return Ok(serde_json::json!({"status":"Corrupt","sqlite":"IntegrityFailure"}));
    }
    let rows = match sqlx::query("SELECT digest, size FROM cas_objects")
        .fetch_all(&pool)
        .await
    {
        Ok(rows) => rows,
        Err(_) => {
            pool.close().await;
            return Ok(serde_json::json!({"status":"Corrupt","sqlite":"CatalogReadFailure"}));
        }
    };
    let mut corrupt = 0u64;
    let mut affected_operations = HashSet::new();
    for row in rows {
        let digest: String = row.get("digest");
        let size: i64 = row.get("size");
        if digest.len() != 64
            || size < 0
            || !digest.bytes().all(|c| c.is_ascii_hexdigit())
            || !member_matches(
                &data_root.join("cas").join(&digest[..2]).join(&digest),
                size as u64,
                &digest,
            )
            .unwrap_or(false)
        {
            corrupt += 1;
            if let Ok(rows) = sqlx::query("SELECT DISTINCT o.id FROM operations o JOIN artifacts a ON CASE WHEN json_valid(o.result_json) THEN json_extract(o.result_json, '$.artifactId') END=a.id WHERE o.status='Completed' AND (a.digest=? OR EXISTS (SELECT 1 FROM artifact_entries e WHERE e.artifact_id=a.id AND e.sha256=?)) UNION SELECT operation_id AS id FROM deletion_cas WHERE digest=?")
                .bind(&digest).bind(&digest).bind(&digest).fetch_all(&pool).await {
                for row in rows { affected_operations.insert(row.get::<String, _>("id")); }
            }
        }
    }
    pool.close().await;
    let status = if corrupt == 0 { "Healthy" } else { "Corrupt" };
    let mut affected_operations: Vec<String> = affected_operations.into_iter().collect();
    affected_operations.sort();
    Ok(
        serde_json::json!({"status":status,"sqlite":"Ok","corruptCasObjects":corrupt,
        "affectedOperationIds":affected_operations}),
    )
}
