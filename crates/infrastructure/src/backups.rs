use crate::auth::{now, Principal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("invalid request")]
    Invalid,
    #[error("idempotency key conflict")]
    IdempotencyConflict,
    #[error("backup unavailable")]
    Unavailable,
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupMember {
    pub entry_id: String,
    pub kind: String,
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub format_version: u32,
    pub backup_id: String,
    pub captured_at: String,
    pub catalog_format_version: i64,
    pub members: Vec<BackupMember>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOperation {
    pub id: String,
    pub kind: &'static str,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    pub backup_output_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub id: String,
    pub format_version: u32,
    pub captured_at: String,
    pub completed_at: String,
    pub member_count: usize,
    pub total_bytes: u64,
}

fn require_admin(actor: &Principal) -> Result<(), BackupError> {
    if actor.server_admin {
        Ok(())
    } else {
        Err(BackupError::Forbidden)
    }
}

fn digest_file(path: &Path) -> Result<(u64, String), BackupError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        digest.update(&bytes[..count]);
    }
    Ok((size, format!("{:x}", digest.finalize())))
}

fn sync_dir(path: &Path) -> Result<(), BackupError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), BackupError> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn verify_directory(directory: &Path) -> Result<BackupManifest, BackupError> {
    let meta = directory
        .symlink_metadata()
        .map_err(|_| BackupError::Unavailable)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(BackupError::Unavailable);
    }
    let manifest_file = directory.join("manifest.json");
    let meta = manifest_file
        .symlink_metadata()
        .map_err(|_| BackupError::Unavailable)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(BackupError::Unavailable);
    }
    let manifest: BackupManifest = serde_json::from_slice(&fs::read(manifest_file)?)?;
    if manifest.format_version != 1
        || Uuid::parse_str(&manifest.backup_id).is_err()
        || manifest.catalog_format_version < 1
    {
        return Err(BackupError::Unavailable);
    }
    let cas_meta = directory
        .join("cas")
        .symlink_metadata()
        .map_err(|_| BackupError::Unavailable)?;
    if !cas_meta.is_dir() || cas_meta.file_type().is_symlink() {
        return Err(BackupError::Unavailable);
    }
    let mut paths = HashSet::new();
    let mut ids = HashSet::new();
    let mut sqlite_count = 0;
    for member in &manifest.members {
        if Uuid::parse_str(&member.entry_id).is_err()
            || !ids.insert(&member.entry_id)
            || !paths.insert(&member.relative_path)
            || !valid_digest(&member.sha256)
        {
            return Err(BackupError::Unavailable);
        }
        match member.kind.as_str() {
            "SqliteSnapshot" if member.relative_path == "catalog.sqlite" => sqlite_count += 1,
            "CasObject" if member.relative_path == format!("cas/{}", member.sha256) => {}
            _ => return Err(BackupError::Unavailable),
        }
        let path = directory.join(&member.relative_path);
        let meta = path
            .symlink_metadata()
            .map_err(|_| BackupError::Unavailable)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != member.size {
            return Err(BackupError::Unavailable);
        }
        let (size, digest) = digest_file(&path)?;
        if size != member.size || digest != member.sha256 {
            return Err(BackupError::Unavailable);
        }
    }
    if sqlite_count != 1 {
        return Err(BackupError::Unavailable);
    }
    Ok(manifest)
}

pub fn verify_output(root: &Path, backup_id: &str) -> Result<BackupManifest, BackupError> {
    Uuid::parse_str(backup_id).map_err(|_| BackupError::Invalid)?;
    let manifest = verify_directory(&root.join("backups").join(backup_id))?;
    if manifest.backup_id != backup_id {
        return Err(BackupError::Unavailable);
    }
    Ok(manifest)
}

async fn stored(pool: &SqlitePool, root: &Path, id: &str) -> Result<BackupOperation, BackupError> {
    let row =
        sqlx::query("SELECT backup_id, status, problem_json FROM backup_operations WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or(BackupError::NotFound)?;
    let backup_id: String = row.get("backup_id");
    let status: String = row.get("status");
    let available = status == "Completed" && verify_output(root, &backup_id).is_ok();
    let problem = row
        .get::<Option<String>, _>("problem_json")
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?;
    Ok(BackupOperation {
        id: id.into(),
        kind: "Backup",
        status: status.clone(),
        backup_id: (status == "Completed").then_some(backup_id),
        backup_output_available: available,
        problem,
    })
}

pub async fn create(
    pool: &SqlitePool,
    root: &Path,
    actor: &Principal,
    key: &str,
) -> Result<(BackupOperation, bool), BackupError> {
    require_admin(actor)?;
    if key.is_empty() || key.len() > 255 {
        return Err(BackupError::Invalid);
    }
    let hash = format!("{:x}", Sha256::digest(b"backup-v1"));
    let mut tx = pool.begin().await?;
    if let Some(row) = sqlx::query("SELECT input_hash, result_id FROM idempotency_bindings WHERE principal_id=? AND action='Backup' AND key=?")
        .bind(&actor.id).bind(key).fetch_optional(&mut *tx).await? {
        if row.get::<String, _>("input_hash") != hash { return Err(BackupError::IdempotencyConflict); }
        let id: String = row.get("result_id");
        drop(tx);
        return Ok((stored(pool, root, &id).await?, false));
    }
    let id = Uuid::new_v4().to_string();
    let backup_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO backup_operations(id, owner_id, backup_id, status, created_at, updated_at) VALUES(?, ?, ?, 'Accepted', ?, ?)")
        .bind(&id).bind(&actor.id).bind(&backup_id).bind(now()).bind(now()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'Backup', ?, ?, ?, ?, ?)")
        .bind(&actor.id).bind(key).bind(hash).bind(&id).bind(serde_json::to_string(&id)?).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((stored(pool, root, &id).await?, true))
}

pub async fn operation(
    pool: &SqlitePool,
    root: &Path,
    actor: &Principal,
    id: &str,
) -> Result<BackupOperation, BackupError> {
    require_admin(actor)?;
    stored(pool, root, id).await
}

fn copy_member(
    source: &Path,
    destination: &Path,
    expected_size: u64,
    expected_digest: &str,
) -> Result<BackupMember, BackupError> {
    let source_meta = source.symlink_metadata()?;
    if !source_meta.is_file()
        || source_meta.file_type().is_symlink()
        || source_meta.len() != expected_size
    {
        return Err(BackupError::Unavailable);
    }
    let mut input = File::open(source)?;
    let mut output = File::create_new(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    let (size, digest) = digest_file(destination)?;
    if size != expected_size || digest != expected_digest {
        return Err(BackupError::Unavailable);
    }
    Ok(BackupMember {
        entry_id: Uuid::new_v4().to_string(),
        kind: "CasObject".into(),
        relative_path: format!("cas/{expected_digest}"),
        size,
        sha256: digest,
    })
}

async fn run_one(
    pool: &SqlitePool,
    root: &Path,
    boundary: &RwLock<()>,
    id: &str,
) -> Result<(), BackupError> {
    let row =
        sqlx::query("SELECT backup_id FROM backup_operations WHERE id=? AND status='Accepted'")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or(BackupError::NotFound)?;
    let backup_id: String = row.get("backup_id");
    let backup_root = root.join("backups");
    let temporary = backup_root.join(format!(".pending-{backup_id}"));
    private_dir(&temporary)?;
    private_dir(&temporary.join("cas"))?;
    sync_dir(&backup_root)?;
    let snapshot = temporary.join("catalog.sqlite");
    let captured_at: String;
    let catalog_format_version: i64;
    {
        let _exclusive = boundary.write().await;
        let mut tx = pool.begin().await?;
        sqlx::query("INSERT INTO backup_pins(operation_id, digest) SELECT ?, digest FROM artifacts WHERE digest IS NOT NULL UNION SELECT ?, sha256 FROM artifact_entries WHERE sha256 IS NOT NULL UNION SELECT ?, digest FROM artwork_bindings")
            .bind(id).bind(id).bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE backup_operations SET status='Running', updated_at=? WHERE id=?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        captured_at = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')")
            .fetch_one(pool)
            .await?;
        catalog_format_version = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await?;
        let path = snapshot.to_str().ok_or(BackupError::Invalid)?;
        sqlx::query("VACUUM INTO ?")
            .bind(path)
            .execute(pool)
            .await?;
    }
    File::open(&snapshot)?.sync_all()?;
    sync_dir(&temporary)?;
    let (size, digest) = digest_file(&snapshot)?;
    let mut members = vec![BackupMember {
        entry_id: Uuid::new_v4().to_string(),
        kind: "SqliteSnapshot".into(),
        relative_path: "catalog.sqlite".into(),
        size,
        sha256: digest,
    }];
    let pins = sqlx::query("SELECT b.digest, c.size FROM backup_pins b JOIN cas_objects c ON c.digest=b.digest WHERE b.operation_id=? ORDER BY b.digest")
        .bind(id).fetch_all(pool).await?;
    let pin_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM backup_pins WHERE operation_id=?")
            .bind(id)
            .fetch_one(pool)
            .await?;
    if pins.len() as i64 != pin_count {
        return Err(BackupError::Unavailable);
    }
    for pin in pins {
        let digest: String = pin.get("digest");
        if !valid_digest(&digest) {
            return Err(BackupError::Unavailable);
        }
        let size: i64 = pin.get("size");
        members.push(copy_member(
            &root.join("cas").join(&digest[..2]).join(&digest),
            &temporary.join("cas").join(&digest),
            size as u64,
            &digest,
        )?);
    }
    sync_dir(&temporary.join("cas"))?;
    let manifest = BackupManifest {
        format_version: 1,
        backup_id: backup_id.clone(),
        captured_at,
        catalog_format_version,
        members,
    };
    let marker = temporary.join("manifest.json.tmp");
    let mut file = File::create_new(&marker)?;
    file.write_all(&serde_json::to_vec(&manifest)?)?;
    file.sync_all()?;
    fs::rename(&marker, temporary.join("manifest.json"))?;
    sync_dir(&temporary)?;
    fs::rename(&temporary, backup_root.join(&backup_id))?;
    sync_dir(&backup_root)?;
    verify_output(root, &backup_id)?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE backup_operations SET status='Completed', updated_at=? WHERE id=?")
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM backup_pins WHERE operation_id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn run(
    pool: &SqlitePool,
    root: &Path,
    boundary: &RwLock<()>,
    id: &str,
) -> Result<(), BackupError> {
    if let Err(error) = run_one(pool, root, boundary, id).await {
        let row = sqlx::query("SELECT backup_id FROM backup_operations WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        if let Some(row) = row {
            let backup_id: String = row.get("backup_id");
            for path in [
                root.join("backups").join(format!(".pending-{backup_id}")),
                root.join("backups").join(&backup_id),
            ] {
                if path.exists() {
                    fs::remove_dir_all(path)?;
                }
            }
            sync_dir(&root.join("backups"))?;
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE backup_operations SET status='Failed', problem_json=?, updated_at=? WHERE id=? AND status!='Completed'")
                .bind(r#"{"type":"urn:app-depot:problem:retryable_failure","title":"Backup failed","status":503,"code":"retryable_failure"}"#)
                .bind(now()).bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM backup_pins WHERE operation_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        return Err(error);
    }
    Ok(())
}

pub async fn recover(pool: &SqlitePool, root: &Path) -> Result<(), BackupError> {
    let rows = sqlx::query(
        "SELECT id, backup_id FROM backup_operations WHERE status IN ('Accepted','Running')",
    )
    .fetch_all(pool)
    .await?;
    for row in rows {
        let id: String = row.get("id");
        let backup_id: String = row.get("backup_id");
        for path in [
            root.join("backups").join(format!(".pending-{backup_id}")),
            root.join("backups").join(&backup_id),
        ] {
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        sync_dir(&root.join("backups"))?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE backup_operations SET status='Failed', problem_json=?, updated_at=? WHERE id=?")
            .bind(r#"{"type":"urn:app-depot:problem:retryable_failure","title":"Interrupted backup","status":503,"code":"retryable_failure"}"#)
            .bind(now()).bind(&id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM backup_pins WHERE operation_id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    sqlx::query("DELETE FROM backup_pins WHERE operation_id IN (SELECT id FROM backup_operations WHERE status IN ('Completed','Failed'))")
        .execute(pool).await?;
    Ok(())
}

pub async fn completed(
    pool: &SqlitePool,
    root: &Path,
    actor: &Principal,
) -> Result<Vec<BackupSummary>, BackupError> {
    require_admin(actor)?;
    let rows = sqlx::query("SELECT backup_id, strftime('%Y-%m-%dT%H:%M:%SZ', updated_at, 'unixepoch') AS completed_at FROM backup_operations WHERE status='Completed' ORDER BY backup_id")
        .fetch_all(pool).await?;
    let mut output = Vec::new();
    for row in rows {
        let backup_id: String = row.get("backup_id");
        if let Ok(manifest) = verify_output(root, &backup_id) {
            output.push(BackupSummary {
                id: backup_id,
                format_version: 1,
                captured_at: manifest.captured_at,
                completed_at: row.get("completed_at"),
                member_count: manifest.members.len(),
                total_bytes: manifest
                    .members
                    .iter()
                    .fold(0u64, |total, member| total.saturating_add(member.size)),
            });
        }
    }
    Ok(output)
}

pub async fn manifest(
    pool: &SqlitePool,
    root: &Path,
    actor: &Principal,
    backup_id: &str,
) -> Result<BackupManifest, BackupError> {
    require_admin(actor)?;
    let known: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM backup_operations WHERE backup_id=? AND status='Completed'",
    )
    .bind(backup_id)
    .fetch_one(pool)
    .await?;
    if known == 0 {
        return Err(BackupError::NotFound);
    }
    verify_output(root, backup_id).map_err(|_| BackupError::NotFound)
}

pub async fn member(
    pool: &SqlitePool,
    root: &Path,
    actor: &Principal,
    backup_id: &str,
    entry_id: &str,
) -> Result<(PathBuf, BackupMember), BackupError> {
    let manifest = manifest(pool, root, actor, backup_id).await?;
    let member = manifest
        .members
        .into_iter()
        .find(|member| member.entry_id == entry_id)
        .ok_or(BackupError::NotFound)?;
    Ok((
        root.join("backups")
            .join(backup_id)
            .join(&member.relative_path),
        member,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{bootstrap_admin, verify_password},
        catalog::{create_app, create_release},
        init_local, lock_local, lock_production, open_catalog,
        upload::UploadAdapter,
        LocalMode,
    };
    use app_depot_application::upload::{
        FileClaim, Target, UploadCoordinator, UploadCreate, UploadPort,
    };

    #[tokio::test]
    async fn backup_pins_snapshot_and_historical_availability() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "backup", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "backup", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let actor = verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let app = create_app(&pool, &actor, "backup-app", "Backed up")
            .await
            .unwrap();
        let release = create_release(&pool, &actor, "backup-release", &app.id, None)
            .await
            .unwrap();
        let upload_id = Uuid::new_v4().to_string();
        let operation_id = Uuid::new_v4().to_string();
        let file_id = Uuid::new_v4().to_string();
        let upload_input = UploadCreate {
            mode: "BareFile".into(),
            target: Target::Release {
                release_id: release.id.clone(),
            },
            file: Some(FileClaim {
                size: 3,
                sha256: "a".repeat(64),
            }),
        };
        let upload_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&upload_input).unwrap())
        );
        sqlx::query("INSERT INTO operations(id, owner_id, kind, status) VALUES(?, ?, 'Finalize', 'Accepted')")
            .bind(&operation_id).bind(&actor.id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO upload_sessions(id, operation_id, owner_id, mode, phase, target_json, file_id, file_size, file_sha256, file_path, created_at, updated_at) VALUES(?, ?, ?, 'BareFile', 'Receiving', ?, ?, 3, ?, 'file.bin', ?, ?)")
            .bind(&upload_id).bind(&operation_id).bind(&actor.id)
            .bind(serde_json::json!({"releaseId":release.id}).to_string())
            .bind(&file_id).bind("a".repeat(64)).bind(now()).bind(now()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CreateUpload', 'old-key', ?, ?, '{}', ?)")
            .bind(&actor.id).bind(upload_hash).bind(&upload_id).bind(now()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO upload_ranges(session_id, file_id, start, end, sha256) VALUES(?, ?, 0, 3, ?)")
            .bind(&upload_id).bind(&file_id).bind("a".repeat(64)).execute(&pool).await.unwrap();
        let deletion_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO deletion_operations(id, owner_id, kind, target_id, status, physical_cleanup, created_at, updated_at) VALUES(?, ?, 'DeleteAssociation', ?, 'Running', 'Pending', ?, ?)")
            .bind(&deletion_id).bind(&actor.id).bind(Uuid::new_v4().to_string())
            .bind(now()).bind(now()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO deletion_cas(operation_id, digest) VALUES(?, ?)")
            .bind(&deletion_id)
            .bind("b".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        let bytes = b"backup content";
        let digest = format!("{:x}", Sha256::digest(bytes));
        sqlx::query("UPDATE deletion_cas SET digest=? WHERE operation_id=?")
            .bind(&digest)
            .bind(&deletion_id)
            .execute(&pool)
            .await
            .unwrap();
        let cas_parent = lock.path.join("cas").join(&digest[..2]);
        fs::create_dir_all(&cas_parent).unwrap();
        let cas_file = cas_parent.join(&digest);
        let mut file = File::create(&cas_file).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1)")
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        let artifact = format!("adfile:sha256:{digest}");
        sqlx::query("INSERT INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', ?)")
            .bind(&artifact)
            .bind(&digest)
            .bind(bytes.len() as i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(Uuid::new_v4().to_string())
            .bind(&release.id)
            .bind(&artifact)
            .execute(&pool)
            .await
            .unwrap();
        let boundary = RwLock::new(());
        let (earlier, _) = create(&pool, &lock.path, &actor, "earlier-backup")
            .await
            .unwrap();
        run(&pool, &lock.path, &boundary, &earlier.id)
            .await
            .unwrap();
        let earlier_finished = operation(&pool, &lock.path, &actor, &earlier.id)
            .await
            .unwrap();
        let (accepted, created) = create(&pool, &lock.path, &actor, "backup-key")
            .await
            .unwrap();
        assert!(created);
        assert_eq!(accepted.status, "Accepted");
        assert!(!accepted.backup_output_available);
        let (replayed, created) = create(&pool, &lock.path, &actor, "backup-key")
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(replayed.id, accepted.id);
        run(&pool, &lock.path, &boundary, &accepted.id)
            .await
            .unwrap();
        let finished = operation(&pool, &lock.path, &actor, &accepted.id)
            .await
            .unwrap();
        assert_eq!(finished.status, "Completed");
        assert!(finished.backup_output_available);
        let backup_id = finished.backup_id.unwrap();
        let manifest = verify_output(&lock.path, &backup_id).unwrap();
        assert_eq!(manifest.members.len(), 2);
        let new_production = dir.path().join("new-production");
        crate::restore::restore(
            &lock.path.join("backups").join(&backup_id),
            &new_production,
            false,
        )
        .await
        .unwrap();
        let production_lock = lock_production(&new_production).unwrap();
        let production_pool = open_catalog(&production_lock).await.unwrap();
        production_pool.close().await;
        let restored = dir.path().join("restored");
        init_local(dir.path(), "restored", LocalMode::Preview).unwrap();
        crate::restore::restore(&lock.path.join("backups").join(&backup_id), &restored, true)
            .await
            .unwrap();
        let restored_lock = lock_local(dir.path(), "restored", LocalMode::Preview).unwrap();
        let restored_pool = open_catalog(&restored_lock).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM deletion_operations WHERE id=?")
                .bind(&deletion_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            "Running"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM deletion_cas WHERE operation_id=?")
                .bind(&deletion_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            1
        );
        let historical = operation(&restored_pool, &restored, &actor, &earlier.id)
            .await
            .unwrap();
        assert_eq!(historical.status, "Completed");
        assert!(!historical.backup_output_available);
        let (replayed_earlier, created) =
            create(&restored_pool, &restored, &actor, "earlier-backup")
                .await
                .unwrap();
        assert!(!created);
        assert_eq!(replayed_earlier.id, earlier.id);
        assert!(!replayed_earlier.backup_output_available);
        let restored_upload: (String, Option<String>) =
            sqlx::query_as("SELECT phase, cancellation_reason FROM upload_sessions WHERE id=?")
                .bind(&upload_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap();
        assert_eq!(restored_upload.0, "Cancelled");
        assert_eq!(restored_upload.1.as_deref(), Some("RestoreInvalidated"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM upload_ranges WHERE session_id=?")
                .bind(&upload_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM operations WHERE id=?")
                .bind(&operation_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            "Failed"
        );
        assert_eq!(sqlx::query_scalar::<_, String>("SELECT result_id FROM idempotency_bindings WHERE action='CreateUpload' AND key='old-key'")
            .fetch_one(&restored_pool).await.unwrap(), upload_id);
        let coordinator = UploadCoordinator::new(UploadAdapter::new(
            restored_pool.clone(),
            restored_lock.path.clone(),
        ));
        let retried = coordinator
            .create(&actor, "old-key", &upload_input)
            .await
            .unwrap();
        assert_eq!(retried.id, upload_id);
        assert_eq!(retried.phase, "Cancelled");
        assert_eq!(
            retried.cancellation_reason.as_deref(),
            Some("RestoreInvalidated")
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM associations")
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            1
        );
        coordinator.recover().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT phase FROM upload_sessions WHERE id=?")
                .bind(&upload_id)
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            "Cancelled"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM associations")
                .fetch_one(&restored_pool)
                .await
                .unwrap(),
            1
        );
        restored_pool.close().await;
        drop(restored_lock);
        fs::write(
            restored.join("cas").join(&digest[..2]).join(&digest),
            b"corrupt content",
        )
        .unwrap();
        let diagnostic = crate::restore::diagnose(&restored).await.unwrap();
        assert_eq!(diagnostic["status"], "Corrupt");
        assert_eq!(diagnostic["corruptCasObjects"], 1);
        assert!(diagnostic["affectedOperationIds"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(deletion_id)));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM backup_pins WHERE operation_id=?")
                .bind(&accepted.id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        fs::remove_file(
            lock.path
                .join("backups")
                .join(&backup_id)
                .join("cas")
                .join(&digest),
        )
        .unwrap();
        let rejected_target = dir.path().join("rejected");
        assert!(crate::restore::restore(
            &lock.path.join("backups").join(&backup_id),
            &rejected_target,
            false
        )
        .await
        .is_err());
        assert!(!rejected_target.exists());
        let retained = init_local(dir.path(), "retained", LocalMode::Preview).unwrap();
        fs::write(retained.join("ops").join("sentinel"), b"prior state").unwrap();
        assert!(crate::restore::restore(
            &lock.path.join("backups").join(&backup_id),
            &retained,
            true
        )
        .await
        .is_err());
        assert_eq!(
            fs::read(retained.join("ops").join("sentinel")).unwrap(),
            b"prior state"
        );
        let historical = operation(&pool, &lock.path, &actor, &accepted.id)
            .await
            .unwrap();
        assert_eq!(historical.status, "Completed");
        assert!(!historical.backup_output_available);
        let available = completed(&pool, &lock.path, &actor).await.unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].id, earlier_finished.backup_id.unwrap());
        assert!(cas_file.exists());
        sqlx::query("INSERT INTO backup_pins(operation_id, digest) VALUES(?, ?)")
            .bind(&accepted.id)
            .bind(&digest)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM associations WHERE artifact_id=?")
            .bind(&artifact)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM artifacts WHERE id=?")
            .bind(&artifact)
            .execute(&pool)
            .await
            .unwrap();
        let adapter = UploadAdapter::new(pool.clone(), lock.path.clone());
        adapter.gc_unreferenced().await.unwrap();
        assert!(cas_file.exists());
        sqlx::query("DELETE FROM backup_pins WHERE operation_id=?")
            .bind(&accepted.id)
            .execute(&pool)
            .await
            .unwrap();
        adapter.gc_unreferenced().await.unwrap();
        assert!(!cas_file.exists());
    }
}
