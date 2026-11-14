use app_depot_application::{
    upload::{
        CancelOutcome, FinalizationResult, Interval, InventoryBatch, InventoryBatchResult,
        InventoryEntry, InventoryEntryInput, StoredSession, Target, UploadCreate, UploadError,
        UploadFile, UploadPort, UploadProgress, UploadSession,
    },
    Principal,
};
use app_depot_domain::base_slug;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::auth::now;

#[derive(Clone)]
pub struct UploadAdapter {
    pool: SqlitePool,
    root: PathBuf,
    #[cfg(test)]
    lose_commit_response: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    fail_deletion_cleanup: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl UploadAdapter {
    pub fn new(pool: SqlitePool, root: PathBuf) -> Self {
        Self {
            pool,
            root,
            #[cfg(test)]
            lose_commit_response: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            fail_deletion_cleanup: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub fn fail_deletion_cleanup_once(&self) {
        self.fail_deletion_cleanup
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn stage_path(&self, id: &str) -> PathBuf {
        self.root.join("staging").join(id).join("file")
    }

    fn tree_stage_path(&self, id: &str, file_id: &str) -> PathBuf {
        self.root
            .join("staging")
            .join(id)
            .join("files")
            .join(file_id)
    }

    fn stage_for(&self, session: &UploadSession, file: &UploadFile) -> PathBuf {
        if session.mode == "BareTree"
            || (session.mode == "ZipImport"
                && session.file.as_ref().map(|claim| claim.file_id.as_str())
                    != Some(file.file_id.as_str()))
        {
            self.tree_stage_path(&session.id, &file.file_id)
        } else {
            self.stage_path(&session.id)
        }
    }

    async fn file_claims(&self, session: &UploadSession) -> Result<Vec<UploadFile>, UploadError> {
        if session.mode != "BareTree"
            && !(session.mode == "ZipImport"
                && session.inventory_complete
                && matches!(
                    session.phase.as_str(),
                    "Importing" | "Committing" | "Completed"
                ))
        {
            return Ok(vec![session.file().clone()]);
        }
        Ok(self
            .inventory(&session.id)
            .await?
            .into_iter()
            .filter_map(|entry| match entry {
                InventoryEntry::File {
                    path,
                    file_id,
                    size,
                    sha256,
                    executable,
                } => Some(UploadFile {
                    path,
                    entry_type: "File".into(),
                    file_id,
                    size,
                    sha256,
                    executable,
                }),
                InventoryEntry::Directory { .. } => None,
            })
            .collect())
    }

    async fn stored(&self, id: &str) -> Result<StoredSession, UploadError> {
        let row = sqlx::query("SELECT id, operation_id, owner_id, mode, phase, terminal_code, cancellation_reason, inventory_complete, target_json, file_id, file_size, file_sha256, file_path FROM upload_sessions WHERE id=?")
            .bind(id).fetch_optional(&self.pool).await.map_err(db)?.ok_or(UploadError::NotFound)?;
        let expired = row
            .try_get::<Option<String>, _>("terminal_code")
            .map_err(db)?
            .as_deref()
            == Some("session_expired");
        let target: Target =
            serde_json::from_str(&row.get::<String, _>("target_json")).map_err(retry)?;
        let file = if let Some(file_id) = row.get::<Option<String>, _>("file_id") {
            Some(UploadFile {
                path: row
                    .get::<Option<String>, _>("file_path")
                    .ok_or_else(|| UploadError::Retryable("transport path missing".into()))?,
                entry_type: "File".into(),
                file_id,
                size: row
                    .get::<Option<i64>, _>("file_size")
                    .ok_or_else(|| UploadError::Retryable("transport size missing".into()))?
                    as u64,
                sha256: row
                    .get::<Option<String>, _>("file_sha256")
                    .ok_or_else(|| UploadError::Retryable("transport digest missing".into()))?,
                executable: false,
            })
        } else {
            None
        };
        Ok(StoredSession {
            owner_id: row.get("owner_id"),
            expired,
            public: UploadSession {
                id: row.get("id"),
                operation_id: row.get("operation_id"),
                mode: row.get("mode"),
                phase: row.get("phase"),
                cancellation_reason: row.get("cancellation_reason"),
                inventory_complete: row.get::<i64, _>("inventory_complete") != 0,
                target,
                file,
            },
        })
    }

    async fn mark_failed(
        &self,
        id: &str,
        code: &str,
        title: &str,
        status: u16,
    ) -> Result<FinalizationResult, UploadError> {
        let session = self.stored(id).await?;
        let problem = serde_json::json!({"type":format!("urn:app-depot:problem:{code}"),"title":title,"status":status,"code":code});
        let result = FinalizationResult {
            session_id: id.into(),
            operation_id: session.public.operation_id.clone(),
            phase: "Failed".into(),
            cancellation_reason: None,
            artifact_id: None,
            app_id: None,
            release_id: None,
            association_id: None,
            problem: Some(problem.clone()),
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "UPDATE operations SET status='Failed', result_json=?, problem_json=? WHERE id=?",
        )
        .bind(serde_json::to_string(&result).map_err(retry)?)
        .bind(problem.to_string())
        .bind(&session.public.operation_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query("UPDATE upload_sessions SET phase='Failed', updated_at=? WHERE id=?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
            .bind(&session.public.operation_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO cleanup_obligations(operation_id, staging_path, state) VALUES(?, ?, 'Pending')")
            .bind(&session.public.operation_id).bind(format!("staging/{id}")).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}

fn db(error: sqlx::Error) -> UploadError {
    UploadError::Retryable(error.to_string())
}
fn retry(error: impl std::fmt::Display) -> UploadError {
    UploadError::Retryable(error.to_string())
}

fn hash_json(value: &impl serde::Serialize) -> Result<String, UploadError> {
    let bytes = serde_json::to_vec(value).map_err(retry)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn file_digest(path: &Path) -> Result<(u64, String), UploadError> {
    let mut file = File::open(path).map_err(retry)?;
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(retry)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn range_digest(path: &Path, range: &Interval) -> Result<String, UploadError> {
    let mut file = File::open(path).map_err(retry)?;
    file.seek(SeekFrom::Start(range.start)).map_err(retry)?;
    let mut hasher = Sha256::new();
    let mut remaining = range.end - range.start;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let amount = buffer.len().min(remaining as usize);
        let count = file.read(&mut buffer[..amount]).map_err(retry)?;
        if count == 0 {
            return Err(UploadError::Retryable("staged range truncated".into()));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn uncovered(request: &Interval, recorded: &[Interval]) -> Vec<Interval> {
    let mut cursor = request.start;
    let mut missing = Vec::new();
    for range in recorded {
        if range.end <= cursor {
            continue;
        }
        if range.start >= request.end {
            break;
        }
        if range.start > cursor {
            missing.push(Interval {
                start: cursor,
                end: range.start.min(request.end),
            });
        }
        cursor = cursor.max(range.end).min(request.end);
        if cursor == request.end {
            break;
        }
    }
    if cursor < request.end {
        missing.push(Interval {
            start: cursor,
            end: request.end,
        });
    }
    missing
}

pub(crate) fn promote_file(
    root: &Path,
    stage: &Path,
    digest: &str,
    size: u64,
) -> Result<(), UploadError> {
    let cas = root.join("cas");
    let parent = cas.join(&digest[..2]);
    if !parent.exists() {
        fs::create_dir(&parent).map_err(retry)?;
        File::open(&cas).and_then(|f| f.sync_all()).map_err(retry)?;
    }
    let final_path = parent.join(digest);
    if let Ok(meta) = final_path.symlink_metadata() {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(UploadError::Retryable(
                "CAS path is not a regular file".into(),
            ));
        }
        let (actual_size, actual_digest) = file_digest(&final_path)?;
        if actual_size != size || actual_digest != digest {
            return Err(UploadError::Retryable(
                "CAS identity verification failed".into(),
            ));
        }
        File::open(&final_path)
            .and_then(|f| f.sync_all())
            .map_err(retry)?;
        File::open(&parent)
            .and_then(|f| f.sync_all())
            .map_err(retry)?;
        return Ok(());
    }
    let candidate = parent.join(format!(".candidate-{}", Uuid::new_v4()));
    let result = (|| {
        let mut source = File::open(stage).map_err(retry)?;
        let mut dest = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .map_err(retry)?;
        std::io::copy(&mut source, &mut dest).map_err(retry)?;
        dest.flush().map_err(retry)?;
        dest.sync_all().map_err(retry)?;
        drop(dest);
        let (actual_size, actual_digest) = file_digest(&candidate)?;
        if actual_size != size || actual_digest != digest {
            return Err(UploadError::Retryable(
                "candidate identity verification failed".into(),
            ));
        }
        match fs::hard_link(&candidate, &final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let (existing_size, existing_digest) = file_digest(&final_path)?;
                if existing_size != size || existing_digest != digest {
                    return Err(UploadError::Retryable("conflicting CAS object".into()));
                }
            }
            Err(error) => return Err(retry(error)),
        }
        File::open(&parent)
            .and_then(|f| f.sync_all())
            .map_err(retry)?;
        Ok(())
    })();
    if candidate.exists() {
        fs::remove_file(&candidate).map_err(retry)?;
        File::open(&parent)
            .and_then(|f| f.sync_all())
            .map_err(retry)?;
    }
    result
}

#[allow(async_fn_in_trait)]
impl UploadPort for UploadAdapter {
    async fn create(
        &self,
        principal: &Principal,
        key: &str,
        input: &UploadCreate,
    ) -> Result<StoredSession, UploadError> {
        if key.is_empty() || key.len() > 255 {
            return Err(UploadError::Invalid);
        }
        let hash = hash_json(input)?;
        let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_tombstones WHERE principal_id=? AND action='CreateUpload' AND key=?")
            .bind(&principal.id).bind(key).fetch_one(&self.pool).await.map_err(db)?;
        if retired != 0 {
            return Err(UploadError::IdempotencyConflict);
        }
        if let Some(row) = sqlx::query("SELECT input_hash, result_id FROM idempotency_bindings WHERE principal_id=? AND action='CreateUpload' AND key=?")
            .bind(&principal.id).bind(key).fetch_optional(&self.pool).await.map_err(db)? {
            if row.get::<String, _>("input_hash") != hash { return Err(UploadError::IdempotencyConflict); }
            return self.stored(&row.get::<String, _>("result_id")).await;
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        match &input.target {
            Target::NewApp { .. } => {
                let grant: i64 = sqlx::query_scalar(
                    "SELECT server_admin OR publish_permission FROM users WHERE id=?",
                )
                .bind(&principal.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
                if !principal.may_create_app() || grant == 0 {
                    return Err(UploadError::Forbidden);
                }
            }
            Target::NewRelease {
                app_id,
                create_release,
            } => {
                if !create_release {
                    return Err(UploadError::Invalid);
                }
                let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
                    .bind(app_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db)?
                    .ok_or(UploadError::NotFound)?;
                if !principal.may_manage(&owner) {
                    return Err(UploadError::NotFound);
                }
            }
            Target::Release { release_id } => {
                let row = sqlx::query("SELECT a.owner_id, r.state FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
                    .bind(release_id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(UploadError::NotFound)?;
                if !principal.may_manage(&row.get::<String, _>("owner_id")) {
                    return Err(UploadError::NotFound);
                }
                if row.get::<String, _>("state") != "Draft" {
                    return Err(UploadError::ImmutableRelease);
                }
            }
        }
        let session_id = Uuid::new_v4().to_string();
        let operation_id = Uuid::new_v4().to_string();
        let file_id = input.file.as_ref().map(|_| Uuid::new_v4().to_string());
        let file = input.file.as_ref();
        let stage_dir = self.root.join("staging").join(&session_id);
        fs::create_dir(&stage_dir).map_err(retry)?;
        if input.mode == "BareTree" || input.mode == "ZipImport" {
            fs::create_dir(stage_dir.join("files")).map_err(retry)?;
        }
        if input.mode != "BareTree" {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage_dir.join("file"))
                .map_err(retry)?;
        }
        let target_json = serde_json::to_string(&input.target).map_err(retry)?;
        sqlx::query("INSERT INTO operations(id, owner_id, kind, status) VALUES(?, ?, 'Finalize', 'Accepted')")
            .bind(&operation_id).bind(&principal.id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO upload_sessions(id, operation_id, owner_id, mode, phase, inventory_complete, target_json, file_id, file_size, file_sha256, file_path, created_at, updated_at) VALUES(?, ?, ?, ?, 'Receiving', ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&session_id).bind(&operation_id).bind(&principal.id).bind(&input.mode).bind((input.mode != "BareTree") as i64).bind(target_json).bind(&file_id)
            .bind(file.map(|claim| claim.size as i64)).bind(file.map(|claim| &claim.sha256)).bind(file.map(|_| "file")).bind(now()).bind(now())
            .execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CreateUpload', ?, ?, ?, '{}', ?)")
            .bind(&principal.id).bind(key).bind(hash).bind(&session_id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.stored(&session_id).await
    }

    async fn get(&self, id: &str) -> Result<StoredSession, UploadError> {
        self.stored(id).await
    }

    async fn append_inventory(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
        batch: &InventoryBatch,
    ) -> Result<InventoryBatchResult, UploadError> {
        if key.is_empty() || key.len() > 255 {
            return Err(UploadError::Invalid);
        }
        let hash = hash_json(&(id, batch))?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(row) = sqlx::query("SELECT input_hash, result_json FROM idempotency_bindings WHERE principal_id=? AND action='AppendInventory' AND key=?")
            .bind(&principal.id).bind(key).fetch_optional(&mut *tx).await.map_err(db)? {
            if row.get::<String, _>("input_hash") != hash { return Err(UploadError::IdempotencyConflict); }
            return serde_json::from_str(&row.get::<String, _>("result_json")).map_err(retry);
        }
        let phase: Option<(String, i64)> = sqlx::query_as(
            "SELECT phase, inventory_complete FROM upload_sessions WHERE id=? AND mode='BareTree'",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if !matches!(phase, Some((ref phase, 0)) if phase == "Receiving") {
            return Err(UploadError::Invalid);
        }
        let current: i64 =
            sqlx::query_scalar("SELECT count(*) FROM upload_inventory WHERE session_id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if current + batch.entries.len() as i64 > 100_000 {
            return Err(UploadError::Invalid);
        }
        let mut output = Vec::with_capacity(batch.entries.len());
        for entry in &batch.entries {
            let exists: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM upload_inventory WHERE session_id=? AND path=?",
            )
            .bind(id)
            .bind(entry.path())
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if exists != 0 {
                return Err(UploadError::Invalid);
            }
            match entry {
                InventoryEntryInput::File {
                    path,
                    size,
                    sha256,
                    executable,
                } => {
                    let file_id = Uuid::new_v4().to_string();
                    let stage = self
                        .root
                        .join("staging")
                        .join(id)
                        .join("files")
                        .join(&file_id);
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(stage)
                        .map_err(retry)?;
                    sqlx::query("INSERT INTO upload_inventory(session_id, path, entry_type, file_id, size, sha256, executable) VALUES(?, ?, 'File', ?, ?, ?, ?)")
                        .bind(id).bind(path).bind(&file_id).bind(*size as i64).bind(sha256).bind(*executable as i64)
                        .execute(&mut *tx).await.map_err(db)?;
                    output.push(InventoryEntry::File {
                        path: path.clone(),
                        file_id,
                        size: *size,
                        sha256: sha256.clone(),
                        executable: *executable,
                    });
                }
                InventoryEntryInput::Directory { path } => {
                    sqlx::query("INSERT INTO upload_inventory(session_id, path, entry_type) VALUES(?, ?, 'Directory')")
                        .bind(id).bind(path).execute(&mut *tx).await.map_err(db)?;
                    output.push(InventoryEntry::Directory { path: path.clone() });
                }
            }
        }
        let result = InventoryBatchResult { entries: output };
        sqlx::query("UPDATE upload_sessions SET inventory_version=inventory_version+1, updated_at=? WHERE id=?")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'AppendInventory', ?, ?, ?, ?, ?)")
            .bind(&principal.id).bind(key).bind(hash).bind(id).bind(serde_json::to_string(&result).map_err(retry)?).bind(now())
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    async fn complete_inventory(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<StoredSession, UploadError> {
        if key.is_empty() || key.len() > 255 {
            return Err(UploadError::Invalid);
        }
        let hash = hash_json(&id)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(row) = sqlx::query("SELECT input_hash FROM idempotency_bindings WHERE principal_id=? AND action='CompleteInventory' AND key=?")
            .bind(&principal.id).bind(key).fetch_optional(&mut *tx).await.map_err(db)? {
            if row.get::<String, _>("input_hash") != hash { return Err(UploadError::IdempotencyConflict); }
        } else {
            sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CompleteInventory', ?, ?, ?, '{}', ?)")
                .bind(&principal.id).bind(key).bind(hash).bind(id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        }
        sqlx::query("UPDATE upload_sessions SET inventory_complete=1, updated_at=? WHERE id=? AND mode='BareTree' AND phase='Receiving'")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.stored(id).await
    }

    async fn inventory(&self, id: &str) -> Result<Vec<InventoryEntry>, UploadError> {
        let rows = sqlx::query("SELECT path, entry_type, file_id, size, sha256, executable FROM upload_inventory WHERE session_id=? ORDER BY path COLLATE BINARY")
            .bind(id).fetch_all(&self.pool).await.map_err(db)?;
        rows.into_iter()
            .map(|row| {
                let path: String = row.get("path");
                if row.get::<String, _>("entry_type") == "Directory" {
                    Ok(InventoryEntry::Directory { path })
                } else {
                    Ok(InventoryEntry::File {
                        path,
                        file_id: row.get::<Option<String>, _>("file_id").ok_or_else(|| {
                            UploadError::Retryable("inventory file ID missing".into())
                        })?,
                        size: row.get::<Option<i64>, _>("size").ok_or_else(|| {
                            UploadError::Retryable("inventory size missing".into())
                        })? as u64,
                        sha256: row.get::<Option<String>, _>("sha256").ok_or_else(|| {
                            UploadError::Retryable("inventory digest missing".into())
                        })?,
                        executable: row.get::<Option<i64>, _>("executable").ok_or_else(|| {
                            UploadError::Retryable("inventory executable missing".into())
                        })? != 0,
                    })
                }
            })
            .collect()
    }

    async fn inventory_version(&self, id: &str) -> Result<u64, UploadError> {
        let value: i64 =
            sqlx::query_scalar("SELECT inventory_version FROM upload_sessions WHERE id=?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(db)?;
        Ok(value as u64)
    }

    async fn receive(
        &self,
        id: &str,
        file_id: &str,
        range: Interval,
        body: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        if session.public.phase != "Receiving"
            || (session.public.mode == "BareTree" && !session.public.inventory_complete)
        {
            return Err(UploadError::InvalidRange);
        }
        let path = if session.public.mode == "BareTree" {
            let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM upload_inventory WHERE session_id=? AND file_id=? AND entry_type='File'")
                .bind(id).bind(file_id).fetch_one(&self.pool).await.map_err(db)?;
            if exists != 1 {
                return Err(UploadError::NotFound);
            }
            self.tree_stage_path(id, file_id)
        } else {
            if session.public.file().file_id != file_id {
                return Err(UploadError::NotFound);
            }
            self.stage_path(id)
        };
        let rows = sqlx::query(
            "SELECT start, end FROM upload_ranges WHERE session_id=? AND file_id=? ORDER BY start",
        )
        .bind(id)
        .bind(file_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let recorded: Vec<Interval> = rows
            .iter()
            .map(|row| Interval {
                start: row.get::<i64, _>("start") as u64,
                end: row.get::<i64, _>("end") as u64,
            })
            .collect();
        let missing = uncovered(&range, &recorded);
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .map_err(retry)?;
        let mut remaining = range.end - range.start;
        let mut position = range.start;
        let mut recorded_index = recorded.partition_point(|covered| covered.end <= position);
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let amount = buffer.len().min(remaining as usize);
            let count = body.read(&mut buffer[..amount]).await.map_err(retry)?;
            if count == 0 {
                return Err(UploadError::InvalidRange);
            }
            let mut offset = 0;
            while offset < count {
                while recorded_index < recorded.len() && recorded[recorded_index].end <= position {
                    recorded_index += 1;
                }
                let covered = recorded
                    .get(recorded_index)
                    .is_some_and(|saved| saved.start <= position);
                let boundary = if covered {
                    recorded[recorded_index].end
                } else {
                    recorded
                        .get(recorded_index)
                        .map(|saved| saved.start)
                        .unwrap_or(range.end)
                };
                let length = (boundary - position).min((count - offset) as u64) as usize;
                file.seek(SeekFrom::Start(position)).await.map_err(retry)?;
                if covered {
                    let mut staged = vec![0u8; length];
                    file.read_exact(&mut staged).await.map_err(retry)?;
                    if staged != buffer[offset..offset + length] {
                        return Err(UploadError::ConflictingBytes);
                    }
                } else {
                    file.write_all(&buffer[offset..offset + length])
                        .await
                        .map_err(retry)?;
                }
                position += length as u64;
                offset += length;
            }
            remaining -= count as u64;
        }
        if body.read(&mut buffer[..1]).await.map_err(retry)? != 0 {
            return Err(UploadError::InvalidRange);
        }
        if missing.is_empty() {
            return Ok(());
        }
        file.flush().await.map_err(retry)?;
        drop(file);
        let digests: Vec<String> = missing
            .iter()
            .map(|part| range_digest(&path, part))
            .collect::<Result<_, _>>()?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let phase: String = sqlx::query_scalar("SELECT phase FROM upload_sessions WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        if phase != "Receiving" {
            return Err(UploadError::InvalidRange);
        }
        for (part, digest) in missing.iter().zip(digests) {
            sqlx::query(
                "INSERT INTO upload_ranges(session_id, file_id, start, end, sha256) VALUES(?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(file_id)
            .bind(part.start as i64)
            .bind(part.end as i64)
            .bind(digest)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        sqlx::query("UPDATE upload_sessions SET progress_version=progress_version+1, updated_at=? WHERE id=?")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn progress(&self, id: &str, file_id: &str) -> Result<UploadProgress, UploadError> {
        let session = self.stored(id).await?;
        let total_size = if session.public.mode == "BareTree" {
            let size: Option<i64> = sqlx::query_scalar("SELECT size FROM upload_inventory WHERE session_id=? AND file_id=? AND entry_type='File'")
                .bind(id).bind(file_id).fetch_optional(&self.pool).await.map_err(db)?;
            size.ok_or(UploadError::NotFound)? as u64
        } else {
            if session.public.file().file_id != file_id {
                return Err(UploadError::NotFound);
            }
            session.public.file().size
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let rows = sqlx::query(
            "SELECT start, end FROM upload_ranges WHERE session_id=? AND file_id=? ORDER BY start",
        )
        .bind(id)
        .bind(file_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut ranges: Vec<Interval> = Vec::new();
        for row in rows {
            let start = row.get::<i64, _>("start") as u64;
            let end = row.get::<i64, _>("end") as u64;
            if let Some(last) = ranges.last_mut() {
                if last.end == start {
                    last.end = end;
                    continue;
                }
            }
            ranges.push(Interval { start, end });
        }
        let version: i64 =
            sqlx::query_scalar("SELECT progress_version FROM upload_sessions WHERE id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(UploadProgress {
            session_id: id.into(),
            file_id: file_id.into(),
            total_size,
            ranges,
            next_cursor: None,
            progress_version: version as u64,
        })
    }

    async fn bind_finalize(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<(), UploadError> {
        if key.is_empty() || key.len() > 255 {
            return Err(UploadError::Invalid);
        }
        let hash = hash_json(&id)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(row) = sqlx::query("SELECT input_hash, result_id FROM idempotency_bindings WHERE principal_id=? AND action='Finalize' AND key=?")
            .bind(&principal.id).bind(key).fetch_optional(&mut *tx).await.map_err(db)? {
            if row.get::<String, _>("input_hash") != hash || row.get::<String, _>("result_id") != id { return Err(UploadError::IdempotencyConflict); }
        } else {
            sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'Finalize', ?, ?, ?, '{}', ?)")
                .bind(&principal.id).bind(key).bind(hash).bind(id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn freeze(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        if session.public.phase != "Receiving" {
            return Err(UploadError::OperationRunning);
        }
        if session.public.mode == "BareTree" && !session.public.inventory_complete {
            return Err(UploadError::Incomplete);
        }
        for file in self.file_claims(&session.public).await? {
            let progress = self.progress(id, &file.file_id).await?;
            if file.size != 0
                && (progress.ranges.len() != 1
                    || progress.ranges[0].start != 0
                    || progress.ranges[0].end != file.size)
            {
                return Err(UploadError::Incomplete);
            }
        }
        sqlx::query("UPDATE upload_sessions SET phase='Verifying', updated_at=? WHERE id=? AND phase='Receiving'")
            .bind(now()).bind(id).execute(&self.pool).await.map_err(db)?;
        sqlx::query("UPDATE operations SET status='Running' WHERE id=?")
            .bind(&session.public.operation_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn verify(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        if !matches!(
            session.public.phase.as_str(),
            "Verifying" | "Importing" | "Committing"
        ) {
            return Err(UploadError::OperationRunning);
        }
        let files = if session.public.mode == "ZipImport" && session.public.phase != "Committing" {
            vec![session.public.file().clone()]
        } else {
            self.file_claims(&session.public).await?
        };
        for file in files {
            let stage = self.stage_for(&session.public, &file);
            let digest = file.sha256.clone();
            let size = file.size;
            let cas = self.root.join("cas").join(&digest[..2]).join(&digest);
            let from_cas = session.public.phase == "Committing" && cas.exists();
            let path = if from_cas { cas } else { stage };
            let (actual_size, actual_digest) =
                tokio::task::spawn_blocking(move || file_digest(&path))
                    .await
                    .map_err(retry)??;
            if actual_size != size || actual_digest != digest {
                return if from_cas {
                    Err(UploadError::Retryable(
                        "CAS identity verification failed".into(),
                    ))
                } else {
                    Err(UploadError::HashMismatch)
                };
            }
        }
        Ok(())
    }

    async fn import_zip(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        if session.public.mode != "ZipImport" {
            return Err(UploadError::Invalid);
        }
        if session.public.phase == "Committing" {
            return Ok(());
        }
        if !matches!(session.public.phase.as_str(), "Verifying" | "Importing") {
            return Err(UploadError::OperationRunning);
        }
        sqlx::query("UPDATE upload_sessions SET phase='Importing', inventory_complete=0, updated_at=? WHERE id=?")
            .bind(now()).bind(id).execute(&self.pool).await.map_err(db)?;
        let stage_dir = self.root.join("staging").join(id).join("files");
        let zip = self.stage_path(id);
        let entries = tokio::task::spawn_blocking(move || {
            if stage_dir.exists() {
                fs::remove_dir_all(&stage_dir).map_err(retry)?;
            }
            crate::zip_import::extract(&zip, &stage_dir)
        })
        .await
        .map_err(retry)??;
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("DELETE FROM upload_inventory WHERE session_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for entry in &entries {
            match entry {
                InventoryEntry::Directory { path } => {
                    sqlx::query("INSERT INTO upload_inventory(session_id, path, entry_type) VALUES(?, ?, 'Directory')")
                        .bind(id).bind(path).execute(&mut *tx).await.map_err(db)?;
                }
                InventoryEntry::File {
                    path,
                    file_id,
                    size,
                    sha256,
                    executable,
                } => {
                    sqlx::query("INSERT INTO upload_inventory(session_id, path, entry_type, file_id, size, sha256, executable) VALUES(?, ?, 'File', ?, ?, ?, ?)")
                        .bind(id).bind(path).bind(file_id).bind(*size as i64).bind(sha256).bind(*executable as i64)
                        .execute(&mut *tx).await.map_err(db)?;
                }
            }
        }
        sqlx::query("UPDATE upload_sessions SET inventory_complete=1, inventory_version=inventory_version+1, updated_at=? WHERE id=?")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn prepare_claim(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        let files = self.file_claims(&session.public).await?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        for file in files {
            sqlx::query("INSERT OR IGNORE INTO prepared_claims(operation_id, digest) VALUES(?, ?)")
                .bind(&session.public.operation_id)
                .bind(&file.sha256)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query("UPDATE upload_sessions SET phase='Committing', updated_at=? WHERE id=? AND phase IN ('Verifying', 'Importing', 'Committing')")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn promote(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        if session.public.phase != "Committing" {
            return Err(UploadError::OperationRunning);
        }
        for file in self.file_claims(&session.public).await? {
            let root = self.root.clone();
            let stage = self.stage_for(&session.public, &file);
            let digest = file.sha256;
            let size = file.size;
            tokio::task::spawn_blocking(move || promote_file(&root, &stage, &digest, size))
                .await
                .map_err(retry)??;
        }
        Ok(())
    }

    async fn register(
        &self,
        id: &str,
        principal: &Principal,
    ) -> Result<FinalizationResult, UploadError> {
        let session = self.stored(id).await?;
        let files = self.file_claims(&session.public).await?;
        let inventory = if session.public.mode == "BareTree" || session.public.mode == "ZipImport" {
            self.inventory(id).await?
        } else {
            Vec::new()
        };
        let artifact_id = if session.public.mode == "BareTree" || session.public.mode == "ZipImport"
        {
            let entries = inventory
                .iter()
                .map(InventoryEntry::tree_entry)
                .collect::<Result<Vec<_>, _>>()?;
            app_depot_domain::tree::identity(&entries)
                .map_err(|_| UploadError::Retryable("invalid frozen tree inventory".into()))?
        } else {
            format!("adfile:sha256:{}", session.public.file().sha256)
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let operation_result: Option<String> =
            sqlx::query_scalar("SELECT result_json FROM operations WHERE id=?")
                .bind(&session.public.operation_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if let Some(result) = operation_result {
            return serde_json::from_str(&result).map_err(retry);
        }
        for file in &files {
            let claim: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM prepared_claims WHERE operation_id=? AND digest=?",
            )
            .bind(&session.public.operation_id)
            .bind(&file.sha256)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if claim != 1 {
                return Err(UploadError::Retryable("prepared CAS claim missing".into()));
            }
        }
        let (app_id, release_id) = match &session.public.target {
            Target::Release { release_id } => {
                let row = sqlx::query("SELECT r.app_id, r.state, a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
                    .bind(release_id).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(UploadError::NotFound)?;
                if !principal.may_manage(&row.get::<String, _>("owner_id")) {
                    return Err(UploadError::NotFound);
                }
                if row.get::<String, _>("state") != "Draft" {
                    return Err(UploadError::ImmutableRelease);
                }
                (row.get::<String, _>("app_id"), release_id.clone())
            }
            Target::NewRelease { app_id, .. } => {
                let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
                    .bind(app_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db)?
                    .ok_or(UploadError::NotFound)?;
                if !principal.may_manage(&owner) {
                    return Err(UploadError::NotFound);
                }
                let release_id = Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO releases(id, app_id) VALUES(?, ?)")
                    .bind(&release_id)
                    .bind(app_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                crate::catalog::create_initial_declaration(&mut tx, "Release", &release_id)
                    .await
                    .map_err(db)?;
                (app_id.clone(), release_id)
            }
            Target::NewApp { new_app_name } => {
                let grant: i64 = sqlx::query_scalar(
                    "SELECT server_admin OR publish_permission FROM users WHERE id=?",
                )
                .bind(&principal.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
                if !principal.may_create_app() || grant == 0 {
                    return Err(UploadError::Forbidden);
                }
                let app_id = Uuid::new_v4().to_string();
                let base =
                    base_slug(new_app_name).unwrap_or_else(|| format!("app-{}", &app_id[..8]));
                let mut slug = base.clone();
                let mut suffix = 2;
                while sqlx::query_scalar::<_, i64>("SELECT count(*) FROM apps WHERE slug=?")
                    .bind(&slug)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db)?
                    != 0
                {
                    slug = format!("{base}-{suffix}");
                    suffix += 1;
                }
                sqlx::query("INSERT INTO apps(id, name, slug, owner_id) VALUES(?, ?, ?, ?)")
                    .bind(&app_id)
                    .bind(new_app_name)
                    .bind(&slug)
                    .bind(&principal.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                crate::catalog::create_initial_declaration(&mut tx, "App", &app_id)
                    .await
                    .map_err(db)?;
                let release_id = Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO releases(id, app_id) VALUES(?, ?)")
                    .bind(&release_id)
                    .bind(&app_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                crate::catalog::create_initial_declaration(&mut tx, "Release", &release_id)
                    .await
                    .map_err(db)?;
                (app_id, release_id)
            }
        };
        for file in &files {
            let object = sqlx::query("SELECT size, verified FROM cas_objects WHERE digest=?")
                .bind(&file.sha256)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            if let Some(row) = object {
                if row.get::<i64, _>("size") as u64 != file.size
                    || row.get::<i64, _>("verified") != 1
                {
                    return Err(UploadError::Retryable(
                        "catalog CAS identity mismatch".into(),
                    ));
                }
            } else {
                sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, ?, 1)")
                    .bind(&file.sha256)
                    .bind(file.size as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
        }
        if session.public.mode == "BareTree" || session.public.mode == "ZipImport" {
            let total = files
                .iter()
                .try_fold(0u64, |sum, file| sum.checked_add(file.size))
                .ok_or(UploadError::Invalid)?;
            if total > i64::MAX as u64 {
                return Err(UploadError::Invalid);
            }
            sqlx::query("INSERT OR IGNORE INTO artifacts(id, digest, kind, size) VALUES(?, NULL, 'Tree', ?)")
                .bind(&artifact_id).bind(total as i64).execute(&mut *tx).await.map_err(db)?;
            for entry in &inventory {
                match entry {
                    InventoryEntry::Directory { path } => {
                        sqlx::query("INSERT OR IGNORE INTO artifact_entries(artifact_id, path, entry_type) VALUES(?, ?, 'Directory')")
                            .bind(&artifact_id).bind(path).execute(&mut *tx).await.map_err(db)?;
                    }
                    InventoryEntry::File {
                        path,
                        size,
                        sha256,
                        executable,
                        ..
                    } => {
                        sqlx::query("INSERT OR IGNORE INTO artifact_entries(artifact_id, path, entry_type, size, sha256, executable, file_id) VALUES(?, ?, 'File', ?, ?, ?, ?)")
                            .bind(&artifact_id).bind(path).bind(*size as i64).bind(sha256).bind(*executable as i64).bind(Uuid::new_v4().to_string())
                            .execute(&mut *tx).await.map_err(db)?;
                    }
                }
            }
        } else {
            let file = session.public.file();
            sqlx::query(
                "INSERT OR IGNORE INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', ?)",
            )
            .bind(&artifact_id)
            .bind(&file.sha256)
            .bind(file.size as i64)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        let association_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
            .bind(&association_id)
            .bind(&release_id)
            .bind(&artifact_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        crate::catalog::create_initial_declaration(&mut tx, "Association", &association_id)
            .await
            .map_err(db)?;
        let result = FinalizationResult {
            session_id: id.into(),
            operation_id: session.public.operation_id.clone(),
            phase: "Completed".into(),
            cancellation_reason: None,
            artifact_id: Some(artifact_id),
            app_id: Some(app_id),
            release_id: Some(release_id),
            association_id: Some(association_id),
            problem: None,
        };
        let result_json = serde_json::to_string(&result).map_err(retry)?;
        sqlx::query("INSERT INTO cleanup_obligations(operation_id, staging_path, state) VALUES(?, ?, 'Pending')")
            .bind(&session.public.operation_id).bind(format!("staging/{id}")).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("UPDATE operations SET status='Completed', result_json=? WHERE id=?")
            .bind(result_json)
            .bind(&session.public.operation_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE upload_sessions SET phase='Completed', updated_at=? WHERE id=?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
            .bind(&session.public.operation_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        match tx.commit().await {
            Ok(()) => {
                #[cfg(test)]
                if self
                    .lose_commit_response
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(UploadError::Retryable(
                        "injected lost commit response".into(),
                    ));
                }
                Ok(result)
            }
            Err(error) => match self.result(id).await? {
                Some(result) => Ok(result),
                None => Err(db(error)),
            },
        }
    }

    async fn fail_hash(&self, id: &str) -> Result<FinalizationResult, UploadError> {
        self.mark_failed(id, "hash_mismatch", "Hash mismatch", 422)
            .await
    }

    async fn result(&self, id: &str) -> Result<Option<FinalizationResult>, UploadError> {
        let result: Option<Option<String>> = sqlx::query_scalar("SELECT o.result_json FROM upload_sessions s JOIN operations o ON o.id=s.operation_id WHERE s.id=?")
            .bind(id).fetch_optional(&self.pool).await.map_err(db)?;
        match result.flatten() {
            Some(json) => Ok(Some(serde_json::from_str(&json).map_err(retry)?)),
            None => Ok(None),
        }
    }

    async fn fail_precommit(
        &self,
        id: &str,
        reason: UploadError,
    ) -> Result<FinalizationResult, UploadError> {
        match reason {
            UploadError::ImmutableRelease => {
                self.mark_failed(id, "immutable_release", "Immutable release", 409)
                    .await
            }
            UploadError::Forbidden => self.mark_failed(id, "forbidden", "Forbidden", 403).await,
            UploadError::NotFound => self.mark_failed(id, "not_found", "Not found", 404).await,
            UploadError::InvalidZip => {
                self.mark_failed(id, "invalid_zip", "Invalid ZIP", 422)
                    .await
            }
            UploadError::UnsupportedZipMethod => {
                self.mark_failed(id, "unsupported_zip_method", "Unsupported ZIP method", 422)
                    .await
            }
            _ => Err(UploadError::Invalid),
        }
    }

    async fn recovery_sessions(&self) -> Result<Vec<String>, UploadError> {
        let legacy: i64 = sqlx::query_scalar("SELECT count(*) FROM association_delete_operations WHERE id NOT IN (SELECT id FROM deletion_operations)")
            .fetch_one(&self.pool).await.map_err(db)?;
        if legacy != 0 {
            return Err(UploadError::Retryable(
                "legacy deletion operations require evidence-based offline reconciliation".into(),
            ));
        }
        sqlx::query_scalar("SELECT id FROM upload_sessions WHERE phase IN ('Receiving','Verifying','Importing','Committing') ORDER BY id")
            .fetch_all(&self.pool).await.map_err(db)
    }

    async fn owner_principal(&self, id: &str) -> Result<Principal, UploadError> {
        let row = sqlx::query("SELECT u.id, u.username, u.server_admin, u.publish_permission FROM upload_sessions s JOIN users u ON u.id=s.owner_id WHERE s.id=?")
            .bind(id).fetch_optional(&self.pool).await.map_err(db)?.ok_or(UploadError::NotFound)?;
        Ok(Principal {
            id: row.get("id"),
            username: row.get("username"),
            server_admin: row.get::<i64, _>("server_admin") != 0,
            publish_permission: row.get::<i64, _>("publish_permission") != 0,
        })
    }

    async fn reconcile_progress(&self, id: &str) -> Result<(), UploadError> {
        let session = self.stored(id).await?;
        let files = self.file_claims(&session.public).await?;
        if session.public.phase == "Committing" {
            let mut all_installed = true;
            for file in &files {
                let cas = self
                    .root
                    .join("cas")
                    .join(&file.sha256[..2])
                    .join(&file.sha256);
                if !cas.exists() {
                    all_installed = false;
                    break;
                }
                let (size, hash) = file_digest(&cas)?;
                if size != file.size || hash != file.sha256 {
                    all_installed = false;
                    break;
                }
            }
            if all_installed {
                return Ok(());
            }
        }
        if session.public.mode == "ZipImport"
            && matches!(session.public.phase.as_str(), "Importing" | "Committing")
        {
            let transport = session.public.file();
            let complete_zip = file_digest(&self.stage_path(id))
                .map(|(size, digest)| size == transport.size && digest == transport.sha256)
                .unwrap_or(false);
            let mut tx = self.pool.begin().await.map_err(db)?;
            sqlx::query("DELETE FROM upload_inventory WHERE session_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            if complete_zip {
                sqlx::query("UPDATE upload_sessions SET phase='Verifying', inventory_complete=0, updated_at=? WHERE id=?")
                    .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
            } else {
                sqlx::query("DELETE FROM upload_ranges WHERE session_id=?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
                    .bind(&session.public.operation_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query("UPDATE upload_sessions SET phase='Receiving', inventory_complete=0, progress_version=progress_version+1, updated_at=? WHERE id=?")
                    .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
                sqlx::query("UPDATE operations SET status='Accepted' WHERE id=?")
                    .bind(&session.public.operation_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            tx.commit().await.map_err(db)?;
            if !complete_zip {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(self.stage_path(id))
                    .map_err(retry)?;
            }
            return Ok(());
        }
        let mut invalid = Vec::new();
        let mut missing_paths = Vec::new();
        for claim in &files {
            let path = self.stage_for(&session.public, claim);
            let rows = sqlx::query("SELECT start, end, sha256 FROM upload_ranges WHERE session_id=? AND file_id=? ORDER BY start")
                .bind(id).bind(&claim.file_id).fetch_all(&self.pool).await.map_err(db)?;
            let opened = match File::open(&path) {
                Ok(file) => Some(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(retry(error)),
            };
            if let Some(mut file) = opened {
                for row in &rows {
                    let start: i64 = row.get("start");
                    let end: i64 = row.get("end");
                    let expected: String = row.get("sha256");
                    file.seek(SeekFrom::Start(start as u64)).map_err(retry)?;
                    let mut hasher = Sha256::new();
                    let mut remaining = end - start;
                    let mut buffer = [0u8; 65536];
                    let mut complete = true;
                    while remaining > 0 {
                        let amount = buffer.len().min(remaining as usize);
                        let count = file.read(&mut buffer[..amount]).map_err(retry)?;
                        if count == 0 {
                            complete = false;
                            break;
                        }
                        hasher.update(&buffer[..count]);
                        remaining -= count as i64;
                    }
                    if !complete || format!("{:x}", hasher.finalize()) != expected {
                        invalid.push((claim.file_id.clone(), start, end));
                    }
                }
            } else {
                invalid.extend(rows.iter().map(|row| {
                    (
                        claim.file_id.clone(),
                        row.get::<i64, _>("start"),
                        row.get::<i64, _>("end"),
                    )
                }));
                missing_paths.push(path);
            }
        }
        if invalid.is_empty() && missing_paths.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        for (file_id, start, end) in &invalid {
            sqlx::query(
                "DELETE FROM upload_ranges WHERE session_id=? AND file_id=? AND start=? AND end=?",
            )
            .bind(id)
            .bind(file_id)
            .bind(start)
            .bind(end)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        if !invalid.is_empty() || session.public.phase != "Receiving" {
            sqlx::query("UPDATE upload_sessions SET phase='Receiving', progress_version=progress_version+1, updated_at=? WHERE id=?")
                .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
            sqlx::query("UPDATE operations SET status='Accepted' WHERE id=?")
                .bind(&session.public.operation_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
                .bind(&session.public.operation_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        for path in missing_paths {
            let parent = path.parent().ok_or(UploadError::Invalid)?;
            fs::create_dir_all(parent).map_err(retry)?;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(retry)?;
        }
        Ok(())
    }

    async fn cleanup_pending(&self) -> Result<(), UploadError> {
        let rows = sqlx::query("SELECT c.operation_id, s.id FROM cleanup_obligations c JOIN upload_sessions s ON s.operation_id=c.operation_id OR s.cancel_operation_id=c.operation_id WHERE c.state!='Completed'")
            .fetch_all(&self.pool).await.map_err(db)?;
        for row in rows {
            let operation: String = row.get("operation_id");
            let id: String = row.get("id");
            let path = self.root.join("staging").join(&id);
            let removed = if path.exists() {
                fs::remove_dir_all(&path)
                    .and_then(|_| File::open(self.root.join("staging"))?.sync_all())
            } else {
                Ok(())
            };
            let state = if removed.is_ok() {
                "Completed"
            } else {
                "RetryableFailure"
            };
            sqlx::query("UPDATE cleanup_obligations SET state=? WHERE operation_id=?")
                .bind(state)
                .bind(&operation)
                .execute(&self.pool)
                .await
                .map_err(db)?;
            if removed.is_ok() {
                sqlx::query(
                    "UPDATE operations SET status='Completed' WHERE id=? AND kind='CancelUpload'",
                )
                .bind(&operation)
                .execute(&self.pool)
                .await
                .map_err(db)?;
            }
        }
        let staging = self.root.join("staging");
        for entry in fs::read_dir(&staging).map_err(retry)? {
            let entry = entry.map_err(retry)?;
            if !entry.file_type().map_err(retry)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let known: i64 = sqlx::query_scalar("SELECT count(*) FROM upload_sessions WHERE id=?")
                .bind(&id)
                .fetch_one(&self.pool)
                .await
                .map_err(db)?;
            if known == 0 {
                fs::remove_dir_all(entry.path()).map_err(retry)?;
                File::open(&staging)
                    .and_then(|dir| dir.sync_all())
                    .map_err(retry)?;
            }
        }
        Ok(())
    }

    async fn gc_unreferenced(&self) -> Result<(), UploadError> {
        let cas = self.root.join("cas");
        for parent in fs::read_dir(&cas).map_err(retry)? {
            let parent = parent.map_err(retry)?;
            if !parent.file_type().map_err(retry)?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(parent.path()).map_err(retry)? {
                let entry = entry.map_err(retry)?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(".candidate-") {
                    // A concurrent promotion may still be writing this candidate.
                    if entry
                        .metadata()
                        .map_err(retry)?
                        .modified()
                        .map_err(retry)?
                        .elapsed()
                        .unwrap_or_default()
                        > std::time::Duration::from_secs(86400)
                    {
                        fs::remove_file(entry.path()).map_err(retry)?;
                    }
                    continue;
                }
                if name.len() != 64
                    || !name
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                {
                    continue;
                }
                let mut tx = self.pool.begin().await.map_err(db)?;
                // Acquire SQLite's write lock before checking references. A new artwork claim
                // cannot race between the reference check and physical removal.
                sqlx::query("UPDATE cas_objects SET size=size WHERE digest=?")
                    .bind(&name)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                let references: i64 =
                    sqlx::query_scalar("SELECT (SELECT count(*) FROM artifacts WHERE digest=?) + (SELECT count(*) FROM artifact_entries WHERE sha256=?) + (SELECT count(*) FROM artwork_bindings WHERE digest=?)")
                        .bind(&name)
                        .bind(&name)
                        .bind(&name)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(db)?;
                let claims: i64 =
                    sqlx::query_scalar("SELECT (SELECT count(*) FROM prepared_claims WHERE digest=?) + (SELECT count(*) FROM artwork_claims WHERE digest=?)")
                        .bind(&name)
                        .bind(&name)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(db)?;
                let backup_pins: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM backup_pins WHERE digest=?")
                        .bind(&name)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(db)?;
                if references == 0 && claims == 0 && backup_pins == 0 {
                    fs::remove_file(entry.path()).map_err(retry)?;
                    File::open(parent.path())
                        .and_then(|f| f.sync_all())
                        .map_err(retry)?;
                    sqlx::query("DELETE FROM cas_objects WHERE digest=? AND NOT EXISTS (SELECT 1 FROM artifacts WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM artifact_entries WHERE sha256=?) AND NOT EXISTS (SELECT 1 FROM artwork_bindings WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM prepared_claims WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM artwork_claims WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM backup_pins WHERE digest=?)")
                        .bind(&name).bind(&name).bind(&name).bind(&name).bind(&name).bind(&name).bind(&name).execute(&mut *tx).await.map_err(db)?;
                }
                tx.commit().await.map_err(db)?;
            }
        }
        let orphaned = sqlx::query("SELECT digest FROM cas_objects c WHERE NOT EXISTS (SELECT 1 FROM artifacts a WHERE a.digest=c.digest) AND NOT EXISTS (SELECT 1 FROM artifact_entries e WHERE e.sha256=c.digest) AND NOT EXISTS (SELECT 1 FROM artwork_bindings w WHERE w.digest=c.digest) AND NOT EXISTS (SELECT 1 FROM prepared_claims p WHERE p.digest=c.digest) AND NOT EXISTS (SELECT 1 FROM artwork_claims w WHERE w.digest=c.digest) AND NOT EXISTS (SELECT 1 FROM backup_pins b WHERE b.digest=c.digest)")
            .fetch_all(&self.pool).await.map_err(db)?;
        for row in orphaned {
            let digest: String = row.get("digest");
            if !cas.join(&digest[..2]).join(&digest).exists() {
                sqlx::query("DELETE FROM cas_objects WHERE digest=? AND NOT EXISTS (SELECT 1 FROM artifacts WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM artifact_entries WHERE sha256=?) AND NOT EXISTS (SELECT 1 FROM artwork_bindings WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM prepared_claims WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM artwork_claims WHERE digest=?) AND NOT EXISTS (SELECT 1 FROM backup_pins WHERE digest=?)")
                    .bind(&digest).bind(&digest).bind(&digest).bind(&digest).bind(&digest).bind(&digest).bind(&digest).execute(&self.pool).await.map_err(db)?;
            }
        }
        Ok(())
    }

    async fn cleanup_deletions(&self) -> Result<(), UploadError> {
        let pending = sqlx::query(
            "SELECT id FROM deletion_operations WHERE status!='Completed' ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        for row in pending {
            let operation_id: String = row.get("id");
            sqlx::query("UPDATE deletion_operations SET status='Running', physical_cleanup='Pending', updated_at=? WHERE id=?")
                .bind(now()).bind(&operation_id).execute(&self.pool).await.map_err(db)?;
            let mut tx = self.pool.begin().await.map_err(db)?;
            sqlx::query("DELETE FROM artifacts WHERE NOT EXISTS (SELECT 1 FROM associations WHERE artifact_id=artifacts.id)")
                .execute(&mut *tx).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
            let staging =
                sqlx::query("SELECT session_id FROM deletion_staging WHERE operation_id=?")
                    .bind(&operation_id)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(db)?;
            let mut cleanup_error = {
                #[cfg(test)]
                {
                    self.fail_deletion_cleanup
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                }
                #[cfg(not(test))]
                {
                    false
                }
            };
            for stage in staging {
                let session_id: String = stage.get("session_id");
                let path = self.root.join("staging").join(&session_id);
                if path.exists()
                    && fs::remove_dir_all(&path)
                        .and_then(|_| File::open(self.root.join("staging"))?.sync_all())
                        .is_err()
                {
                    cleanup_error = true;
                    break;
                }
            }
            if !cleanup_error && self.gc_unreferenced().await.is_err() {
                cleanup_error = true;
            }
            if cleanup_error {
                sqlx::query("UPDATE deletion_operations SET physical_cleanup='RetryableFailure', updated_at=? WHERE id=?")
                    .bind(now()).bind(&operation_id).execute(&self.pool).await.map_err(db)?;
                continue;
            }
            let unresolved: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM deletion_artifacts d WHERE d.operation_id=? AND EXISTS (SELECT 1 FROM artifacts a WHERE a.id=d.artifact_id AND NOT EXISTS (SELECT 1 FROM associations x WHERE x.artifact_id=a.id))) + (SELECT count(*) FROM deletion_cas d WHERE d.operation_id=? AND NOT EXISTS (SELECT 1 FROM artifacts a WHERE a.digest=d.digest) AND NOT EXISTS (SELECT 1 FROM artifact_entries e WHERE e.sha256=d.digest) AND NOT EXISTS (SELECT 1 FROM artwork_bindings w WHERE w.digest=d.digest) AND (EXISTS (SELECT 1 FROM cas_objects c WHERE c.digest=d.digest) OR EXISTS (SELECT 1 FROM prepared_claims p WHERE p.digest=d.digest) OR EXISTS (SELECT 1 FROM artwork_claims w WHERE w.digest=d.digest)))")
                .bind(&operation_id).bind(&operation_id).fetch_one(&self.pool).await.map_err(db)?;
            if unresolved != 0 {
                continue;
            }
            let mut completed = self.pool.begin().await.map_err(db)?;
            sqlx::query("DELETE FROM deletion_staging WHERE operation_id=?")
                .bind(&operation_id)
                .execute(&mut *completed)
                .await
                .map_err(db)?;
            sqlx::query("UPDATE deletion_operations SET status='Completed', physical_cleanup='Completed', updated_at=? WHERE id=?")
                .bind(now()).bind(&operation_id).execute(&mut *completed).await.map_err(db)?;
            completed.commit().await.map_err(db)?;
        }
        Ok(())
    }

    async fn cancel(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<CancelOutcome, UploadError> {
        if key.is_empty() || key.len() > 255 {
            return Err(UploadError::Invalid);
        }
        let hash = hash_json(&id)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(row) = sqlx::query("SELECT input_hash FROM idempotency_bindings WHERE principal_id=? AND action='CancelUpload' AND key=?")
            .bind(&principal.id).bind(key).fetch_optional(&mut *tx).await.map_err(db)? {
            if row.get::<String, _>("input_hash") != hash { return Err(UploadError::IdempotencyConflict); }
        }
        let row = sqlx::query(
            "SELECT operation_id, cancel_operation_id, phase FROM upload_sessions WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or(UploadError::NotFound)?;
        let finalization_id: String = row.get("operation_id");
        let phase: String = row.get("phase");
        if phase == "Completed" || phase == "Failed" {
            sqlx::query("INSERT OR IGNORE INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CancelUpload', ?, ?, ?, '{}', ?)")
                .bind(&principal.id).bind(key).bind(&hash).bind(&finalization_id).bind(now()).execute(&mut *tx).await.map_err(db)?;
            let result: String =
                sqlx::query_scalar("SELECT result_json FROM operations WHERE id=?")
                    .bind(&finalization_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(CancelOutcome::Completed(
                serde_json::from_str(&result).map_err(retry)?,
            ));
        }
        if let Some(cancel_id) = row.get::<Option<String>, _>("cancel_operation_id") {
            sqlx::query("INSERT OR IGNORE INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CancelUpload', ?, ?, ?, '{}', ?)")
                .bind(&principal.id).bind(key).bind(&hash).bind(&cancel_id).bind(now()).execute(&mut *tx).await.map_err(db)?;
            let status: String = sqlx::query_scalar("SELECT status FROM operations WHERE id=?")
                .bind(&cancel_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(CancelOutcome::Accepted {
                operation_id: cancel_id,
                status,
            });
        }
        let cancel_id = Uuid::new_v4().to_string();
        let problem = serde_json::json!({"type":"urn:app-depot:problem:session_cancelled","title":"Upload session cancelled","status":410,"code":"session_cancelled"});
        let result = FinalizationResult {
            session_id: id.into(),
            operation_id: finalization_id.clone(),
            phase: "Cancelled".into(),
            cancellation_reason: None,
            artifact_id: None,
            app_id: None,
            release_id: None,
            association_id: None,
            problem: Some(problem.clone()),
        };
        sqlx::query("INSERT INTO operations(id, owner_id, kind, status) VALUES(?, ?, 'CancelUpload', 'Accepted')")
            .bind(&cancel_id).bind(&principal.id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("UPDATE upload_sessions SET phase='Cancelled', terminal_code='session_cancelled', cancel_operation_id=?, updated_at=? WHERE id=?")
            .bind(&cancel_id).bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query(
            "UPDATE operations SET status='Failed', result_json=?, problem_json=? WHERE id=?",
        )
        .bind(serde_json::to_string(&result).map_err(retry)?)
        .bind(problem.to_string())
        .bind(&finalization_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM prepared_claims WHERE operation_id=?")
            .bind(&finalization_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("INSERT INTO cleanup_obligations(operation_id, staging_path, state) VALUES(?, ?, 'Pending')")
            .bind(&cancel_id).bind(format!("staging/{id}")).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO idempotency_bindings(principal_id, action, key, input_hash, result_id, result_json, created_at) VALUES(?, 'CancelUpload', ?, ?, ?, '{}', ?)")
            .bind(&principal.id).bind(key).bind(hash).bind(&cancel_id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(CancelOutcome::Accepted {
            operation_id: cancel_id,
            status: "Accepted".into(),
        })
    }

    async fn expire_idle(&self, cutoff: i64) -> Result<(), UploadError> {
        let rows = sqlx::query("SELECT id, operation_id, owner_id FROM upload_sessions WHERE phase='Receiving' AND updated_at<?")
            .bind(cutoff).fetch_all(&self.pool).await.map_err(db)?;
        for row in rows {
            let id: String = row.get("id");
            let finalization_id: String = row.get("operation_id");
            let owner: String = row.get("owner_id");
            let cancel_id = Uuid::new_v4().to_string();
            let problem = serde_json::json!({"type":"urn:app-depot:problem:session_expired","title":"Upload session expired","status":410,"code":"session_expired"});
            let result = FinalizationResult {
                session_id: id.clone(),
                operation_id: finalization_id.clone(),
                phase: "Cancelled".into(),
                cancellation_reason: None,
                artifact_id: None,
                app_id: None,
                release_id: None,
                association_id: None,
                problem: Some(problem.clone()),
            };
            let mut tx = self.pool.begin().await.map_err(db)?;
            sqlx::query("INSERT INTO operations(id, owner_id, kind, status) VALUES(?, ?, 'CancelUpload', 'Accepted')")
                .bind(&cancel_id).bind(&owner).execute(&mut *tx).await.map_err(db)?;
            let changed = sqlx::query("UPDATE upload_sessions SET phase='Cancelled', terminal_code='session_expired', cancel_operation_id=? WHERE id=? AND phase='Receiving'")
                .bind(&cancel_id).bind(&id).execute(&mut *tx).await.map_err(db)?.rows_affected();
            if changed == 0 {
                continue;
            }
            sqlx::query(
                "UPDATE operations SET status='Failed', result_json=?, problem_json=? WHERE id=?",
            )
            .bind(serde_json::to_string(&result).map_err(retry)?)
            .bind(problem.to_string())
            .bind(&finalization_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            sqlx::query("INSERT INTO cleanup_obligations(operation_id, staging_path, state) VALUES(?, ?, 'Pending')")
                .bind(&cancel_id).bind(format!("staging/{id}")).execute(&mut *tx).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::bootstrap_admin, init_local, lock_local, open_catalog, LocalMode};
    use app_depot_application::upload::UploadCoordinator;

    #[tokio::test]
    async fn bare_file_bytes_commit_once_with_authoritative_progress() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "upload", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "upload", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        let digest = format!("{:x}", Sha256::digest(b"A"));
        let session = coordinator
            .create(
                &user,
                "create-1",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Demo".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 1,
                        sha256: digest.clone(),
                    }),
                },
            )
            .await
            .unwrap();
        let interval = Interval { start: 0, end: 1 };
        let mut first: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                interval.clone(),
                &mut first,
            )
            .await
            .unwrap();
        let progress = coordinator
            .progress(&user, &session.id, &session.file().file_id)
            .await
            .unwrap();
        assert_eq!(progress.ranges.len(), 1);
        assert_eq!(progress.ranges[0].end, 1);
        let mut same: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                interval.clone(),
                &mut same,
            )
            .await
            .unwrap();
        let mut different: &[u8] = b"B";
        assert!(matches!(
            coordinator
                .receive(
                    &user,
                    &session.id,
                    &session.file().file_id,
                    interval,
                    &mut different
                )
                .await,
            Err(UploadError::ConflictingBytes)
        ));
        assert_eq!(
            coordinator
                .progress(&user, &session.id, &session.file().file_id)
                .await
                .unwrap()
                .progress_version,
            progress.progress_version
        );
        let result = coordinator
            .finalize(&user, &session.id, "finalize-1")
            .await
            .unwrap();
        assert_eq!(result.phase, "Completed");
        assert_eq!(
            result.artifact_id.as_deref(),
            Some(format!("adfile:sha256:{digest}").as_str())
        );
        let repeated = coordinator
            .finalize(&user, &session.id, "finalize-1")
            .await
            .unwrap();
        assert_eq!(repeated.association_id, result.association_id);
        assert!(
            matches!(coordinator.cancel(&user, &session.id, "cancel-after-commit").await.unwrap(), CancelOutcome::Completed(saved) if saved.association_id == result.association_id)
        );
        let cas = lock.path.join("cas").join(&digest[..2]).join(&digest);
        assert_eq!(fs::read(cas).unwrap(), b"A");
        let associations: i64 = sqlx::query_scalar("SELECT count(*) FROM associations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(associations, 1);
    }

    #[tokio::test]
    async fn bare_tree_inventory_registers_canonical_identity_and_file_references() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "tree", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "tree", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        let session = coordinator
            .create(
                &user,
                "tree-create",
                &UploadCreate {
                    mode: "BareTree".into(),
                    target: Target::NewApp {
                        new_app_name: "Tree".into(),
                    },
                    file: None,
                },
            )
            .await
            .unwrap();
        assert!(session.file.is_none());
        assert!(!session.inventory_complete);
        let batch = InventoryBatch {
            entries: vec![InventoryEntryInput::File {
                path: "a.txt".into(),
                size: 1,
                sha256: format!("{:x}", Sha256::digest(b"A")),
                executable: false,
            }],
        };
        let appended = coordinator
            .append_inventory(&user, &session.id, "tree-batch", &batch)
            .await
            .unwrap();
        let replay = coordinator
            .append_inventory(&user, &session.id, "tree-batch", &batch)
            .await
            .unwrap();
        let file_id = match &appended.entries[0] {
            InventoryEntry::File { file_id, .. } => file_id.clone(),
            _ => panic!("expected file"),
        };
        assert!(
            matches!(&replay.entries[0], InventoryEntry::File { file_id: repeated, .. } if repeated == &file_id)
        );
        assert!(
            coordinator
                .complete_inventory(&user, &session.id, "tree-complete")
                .await
                .unwrap()
                .inventory_complete
        );
        let mut bytes: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &session.id,
                &file_id,
                Interval { start: 0, end: 1 },
                &mut bytes,
            )
            .await
            .unwrap();
        let result = coordinator
            .finalize(&user, &session.id, "tree-finalize")
            .await
            .unwrap();
        assert_eq!(result.phase, "Completed");
        assert_eq!(
            result.artifact_id.as_deref(),
            Some(
                "adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1"
            )
        );
        let entries: i64 =
            sqlx::query_scalar("SELECT count(*) FROM artifact_entries WHERE artifact_id=?")
                .bind(result.artifact_id.as_ref().unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(entries, 1);
        coordinator.recover().await.unwrap();
        let digest = format!("{:x}", Sha256::digest(b"A"));
        assert_eq!(
            fs::read(lock.path.join("cas").join(&digest[..2]).join(&digest)).unwrap(),
            b"A"
        );
    }

    #[tokio::test]
    async fn restart_reconciles_progress_and_interrupted_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "restart", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "restart", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let digest = format!("{:x}", Sha256::digest(b"A"));
        let adapter = UploadAdapter::new(pool.clone(), lock.path.clone());
        let coordinator = UploadCoordinator::new(adapter.clone());
        let create = |name: &str| UploadCreate {
            mode: "BareFile".into(),
            target: Target::NewApp {
                new_app_name: name.into(),
            },
            file: Some(app_depot_application::upload::FileClaim {
                size: 1,
                sha256: digest.clone(),
            }),
        };
        let damaged = coordinator
            .create(&user, "damaged", &create("Damaged"))
            .await
            .unwrap();
        let mut bytes: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &damaged.id,
                &damaged.file().file_id,
                Interval { start: 0, end: 1 },
                &mut bytes,
            )
            .await
            .unwrap();
        fs::write(adapter.stage_path(&damaged.id), b"B").unwrap();
        let committing = coordinator
            .create(&user, "committing", &create("Committing"))
            .await
            .unwrap();
        let mut bytes: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &committing.id,
                &committing.file().file_id,
                Interval { start: 0, end: 1 },
                &mut bytes,
            )
            .await
            .unwrap();
        adapter.freeze(&committing.id).await.unwrap();
        adapter.verify(&committing.id).await.unwrap();
        adapter.prepare_claim(&committing.id).await.unwrap();
        adapter.promote(&committing.id).await.unwrap();
        adapter.gc_unreferenced().await.unwrap();
        let partial = coordinator
            .create(
                &user,
                "partial",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Partial".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 4,
                        sha256: format!("{:x}", Sha256::digest(b"ABCD")),
                    }),
                },
            )
            .await
            .unwrap();
        let mut first_half: &[u8] = b"AB";
        coordinator
            .receive(
                &user,
                &partial.id,
                &partial.file().file_id,
                Interval { start: 0, end: 2 },
                &mut first_half,
            )
            .await
            .unwrap();
        let mut second_half: &[u8] = b"CD";
        coordinator
            .receive(
                &user,
                &partial.id,
                &partial.file().file_id,
                Interval { start: 2, end: 4 },
                &mut second_half,
            )
            .await
            .unwrap();
        let mut staged = OpenOptions::new()
            .write(true)
            .open(adapter.stage_path(&partial.id))
            .unwrap();
        staged.write_all(b"X").unwrap();
        staged.sync_all().unwrap();
        assert!(lock
            .path
            .join("cas")
            .join(&digest[..2])
            .join(&digest)
            .exists());
        drop(coordinator);
        drop(adapter);
        pool.close().await;
        drop(lock);

        let lock = lock_local(dir.path(), "restart", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let orphan = lock
            .path
            .join("staging")
            .join("orphan-before-sqlite-commit");
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("file"), b"unregistered").unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        coordinator.recover().await.unwrap();
        assert!(!orphan.exists());
        let progress = coordinator
            .progress(&user, &damaged.id, &damaged.file().file_id)
            .await
            .unwrap();
        assert!(progress.ranges.is_empty());
        let partial_progress = coordinator
            .progress(&user, &partial.id, &partial.file().file_id)
            .await
            .unwrap();
        assert_eq!(
            partial_progress
                .ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            vec![(2, 4)]
        );
        assert_eq!(
            coordinator.get(&user, &damaged.id).await.unwrap().phase,
            "Receiving"
        );
        let result = coordinator
            .result(&user, &committing.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.phase, "Completed");
        assert_eq!(result.operation_id, committing.operation_id);
        assert_eq!(
            coordinator
                .finalize(&user, &committing.id, "retry-after-crash")
                .await
                .unwrap()
                .association_id,
            result.association_id
        );
        let mut resend: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &damaged.id,
                &damaged.file().file_id,
                Interval { start: 0, end: 1 },
                &mut resend,
            )
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .finalize(&user, &damaged.id, "after-reconcile")
                .await
                .unwrap()
                .phase,
            "Completed"
        );
        let mut repair: &[u8] = b"AB";
        coordinator
            .receive(
                &user,
                &partial.id,
                &partial.file().file_id,
                Interval { start: 0, end: 2 },
                &mut repair,
            )
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .finalize(&user, &partial.id, "partial-repaired")
                .await
                .unwrap()
                .phase,
            "Completed"
        );
    }

    #[tokio::test]
    async fn cancellation_and_expiry_preserve_keys_and_hide_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "terminal", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "terminal", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        let input = UploadCreate {
            mode: "BareFile".into(),
            target: Target::NewApp {
                new_app_name: "Terminal".into(),
            },
            file: Some(app_depot_application::upload::FileClaim {
                size: 1,
                sha256: format!("{:x}", Sha256::digest(b"A")),
            }),
        };
        let first = coordinator.create(&user, "first", &input).await.unwrap();
        let second = coordinator.create(&user, "second", &input).await.unwrap();
        let outcome = coordinator
            .cancel(&user, &first.id, "cancel-first")
            .await
            .unwrap();
        let operation_id = match outcome {
            CancelOutcome::Accepted { operation_id, .. } => operation_id,
            _ => panic!("unexpected cancellation outcome"),
        };
        assert!(
            matches!(coordinator.cancel(&user, &first.id, "cancel-first").await.unwrap(), CancelOutcome::Accepted { operation_id: id, .. } if id == operation_id)
        );
        assert!(matches!(
            coordinator.cancel(&user, &second.id, "cancel-first").await,
            Err(UploadError::IdempotencyConflict)
        ));
        assert_eq!(
            coordinator
                .result(&user, &first.id)
                .await
                .unwrap()
                .unwrap()
                .phase,
            "Cancelled"
        );
        coordinator.maintenance(now() - 86400).await.unwrap();
        assert!(!lock.path.join("staging").join(&first.id).exists());
        coordinator.maintenance(now() + 1).await.unwrap();
        let stranger = Principal {
            id: "stranger".into(),
            username: "stranger".into(),
            server_admin: false,
            publish_permission: false,
        };
        assert!(matches!(
            coordinator.get(&stranger, &second.id).await,
            Err(UploadError::NotFound)
        ));
        assert!(matches!(
            coordinator.get(&user, &second.id).await,
            Err(UploadError::SessionExpired)
        ));
        assert!(!lock.path.join("staging").join(&second.id).exists());
    }

    #[tokio::test]
    async fn uncertain_sqlite_commit_resolves_to_one_durable_registration() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "uncertain", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "uncertain", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let adapter = UploadAdapter::new(pool.clone(), lock.path.clone());
        let coordinator = UploadCoordinator::new(adapter.clone());
        let session = coordinator
            .create(
                &user,
                "create-uncertain",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Uncertain".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 1,
                        sha256: format!("{:x}", Sha256::digest(b"A")),
                    }),
                },
            )
            .await
            .unwrap();
        let mut bytes: &[u8] = b"A";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                Interval { start: 0, end: 1 },
                &mut bytes,
            )
            .await
            .unwrap();
        assert!(coordinator
            .result(&user, &session.id)
            .await
            .unwrap()
            .is_none());
        adapter
            .lose_commit_response
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let completed = coordinator
            .finalize(&user, &session.id, "finalize-uncertain")
            .await
            .unwrap();
        assert_eq!(completed.phase, "Completed");
        drop(coordinator);
        drop(adapter);
        pool.close().await;
        drop(lock);

        let lock = lock_local(dir.path(), "uncertain", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        coordinator.recover().await.unwrap();
        let repeated = coordinator
            .finalize(&user, &session.id, "second-key")
            .await
            .unwrap();
        assert_eq!(repeated.operation_id, completed.operation_id);
        assert_eq!(repeated.association_id, completed.association_id);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM associations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn out_of_order_and_mixed_overlap_ranges_are_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "ranges", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "ranges", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        let session = coordinator
            .create(
                &user,
                "ranges-create",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Ranges".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 8,
                        sha256: format!("{:x}", Sha256::digest(b"ABCDEFGH")),
                    }),
                },
            )
            .await
            .unwrap();
        let send = |start, end| Interval { start, end };
        let mut tail: &[u8] = b"EFGH";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                send(4, 8),
                &mut tail,
            )
            .await
            .unwrap();
        let mut head: &[u8] = b"AB";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                send(0, 2),
                &mut head,
            )
            .await
            .unwrap();
        let before = coordinator
            .progress(&user, &session.id, &session.file().file_id)
            .await
            .unwrap();
        assert_eq!(
            before
                .ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            vec![(0, 2), (4, 8)]
        );
        let first_page = coordinator
            .progress_page(&user, &session.id, &session.file().file_id, Some(1), None)
            .await
            .unwrap();
        assert_eq!(first_page.ranges.len(), 1);
        let cursor = first_page.next_cursor.unwrap();
        let second_page = coordinator
            .progress_page(
                &user,
                &session.id,
                &session.file().file_id,
                Some(1),
                Some(&cursor),
            )
            .await
            .unwrap();
        assert_eq!(
            second_page
                .ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            vec![(4, 8)]
        );
        let mut mixed: &[u8] = b"BCDE";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                send(1, 5),
                &mut mixed,
            )
            .await
            .unwrap();
        let progress = coordinator
            .progress(&user, &session.id, &session.file().file_id)
            .await
            .unwrap();
        assert!(matches!(
            coordinator
                .progress_page(
                    &user,
                    &session.id,
                    &session.file().file_id,
                    Some(1),
                    Some(&cursor)
                )
                .await,
            Err(UploadError::ProgressChanged)
        ));
        assert!(matches!(
            coordinator
                .progress_page(
                    &user,
                    &session.id,
                    &session.file().file_id,
                    Some(1),
                    Some("broken")
                )
                .await,
            Err(UploadError::InvalidCursor)
        ));
        assert_eq!(
            progress
                .ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            vec![(0, 8)]
        );
        let mut retry: &[u8] = b"BCDE";
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                send(1, 5),
                &mut retry,
            )
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .progress(&user, &session.id, &session.file().file_id)
                .await
                .unwrap()
                .progress_version,
            progress.progress_version
        );
        let mut conflict: &[u8] = b"BCXE";
        assert!(matches!(
            coordinator
                .receive(
                    &user,
                    &session.id,
                    &session.file().file_id,
                    send(1, 5),
                    &mut conflict
                )
                .await,
            Err(UploadError::ConflictingBytes)
        ));
        assert_eq!(
            coordinator
                .progress(&user, &session.id, &session.file().file_id)
                .await
                .unwrap()
                .progress_version,
            progress.progress_version
        );
        assert_eq!(
            coordinator
                .finalize(&user, &session.id, "ranges-finalize")
                .await
                .unwrap()
                .phase,
            "Completed"
        );
    }

    #[tokio::test]
    async fn nonoverlapping_transfers_can_progress_concurrently() {
        use tokio::io::AsyncWriteExt;
        struct SignalReader<R> {
            inner: R,
            started: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for SignalReader<R> {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if let Some(started) = self.started.take() {
                    let _ = started.send(());
                }
                std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "parallel", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "parallel", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = dir.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let user = crate::auth::verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let coordinator = std::sync::Arc::new(UploadCoordinator::new(UploadAdapter::new(
            pool.clone(),
            lock.path.clone(),
        )));
        let session = coordinator
            .create(
                &user,
                "parallel-create",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Parallel".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 4,
                        sha256: format!("{:x}", Sha256::digest(b"ABCD")),
                    }),
                },
            )
            .await
            .unwrap();
        let (mut writer, mut blocked_reader) = tokio::io::duplex(8);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let first_coordinator = coordinator.clone();
        let first_user = user.clone();
        let first_id = session.id.clone();
        let first_file = session.file().file_id.clone();
        let first = tokio::spawn(async move {
            let mut reader = SignalReader {
                inner: &mut blocked_reader,
                started: Some(started_tx),
            };
            first_coordinator
                .receive(
                    &first_user,
                    &first_id,
                    &first_file,
                    Interval { start: 0, end: 2 },
                    &mut reader,
                )
                .await
        });
        started_rx.await.unwrap();
        let mut second: &[u8] = b"CD";
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            coordinator.receive(
                &user,
                &session.id,
                &session.file().file_id,
                Interval { start: 2, end: 4 },
                &mut second,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        writer.write_all(b"AB").await.unwrap();
        writer.shutdown().await.unwrap();
        first.await.unwrap().unwrap();
        let progress = coordinator
            .progress(&user, &session.id, &session.file().file_id)
            .await
            .unwrap();
        assert_eq!(
            progress
                .ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            vec![(0, 4)]
        );
        assert_eq!(
            coordinator
                .finalize(&user, &session.id, "parallel-finalize")
                .await
                .unwrap()
                .phase,
            "Completed"
        );
        let overlap = coordinator
            .create(
                &user,
                "overlap-create",
                &UploadCreate {
                    mode: "BareFile".into(),
                    target: Target::NewApp {
                        new_app_name: "Overlap".into(),
                    },
                    file: Some(app_depot_application::upload::FileClaim {
                        size: 4,
                        sha256: format!("{:x}", Sha256::digest(b"ABCD")),
                    }),
                },
            )
            .await
            .unwrap();
        let (mut writer, blocked_reader) = tokio::io::duplex(8);
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let first_coordinator = coordinator.clone();
        let first_user = user.clone();
        let first_id = overlap.id.clone();
        let first_file = overlap.file().file_id.clone();
        let first = tokio::spawn(async move {
            let mut reader = SignalReader {
                inner: blocked_reader,
                started: Some(first_started_tx),
            };
            first_coordinator
                .receive(
                    &first_user,
                    &first_id,
                    &first_file,
                    Interval { start: 0, end: 2 },
                    &mut reader,
                )
                .await
        });
        first_started_rx.await.unwrap();
        let (second_started_tx, mut second_started_rx) = tokio::sync::oneshot::channel();
        let second_coordinator = coordinator.clone();
        let second_user = user.clone();
        let second_id = overlap.id.clone();
        let second_file = overlap.file().file_id.clone();
        let second = tokio::spawn(async move {
            let mut reader = SignalReader {
                inner: &b"BC"[..],
                started: Some(second_started_tx),
            };
            second_coordinator
                .receive(
                    &second_user,
                    &second_id,
                    &second_file,
                    Interval { start: 1, end: 3 },
                    &mut reader,
                )
                .await
        });
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            &mut second_started_rx
        )
        .await
        .is_err());
        writer.write_all(b"AB").await.unwrap();
        writer.shutdown().await.unwrap();
        first.await.unwrap().unwrap();
        second_started_rx.await.unwrap();
        second.await.unwrap().unwrap();
        let mut last: &[u8] = b"D";
        coordinator
            .receive(
                &user,
                &overlap.id,
                &overlap.file().file_id,
                Interval { start: 3, end: 4 },
                &mut last,
            )
            .await
            .unwrap();
        assert_eq!(
            coordinator
                .finalize(&user, &overlap.id, "overlap-finalize")
                .await
                .unwrap()
                .phase,
            "Completed"
        );
    }
}
