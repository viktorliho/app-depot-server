use crate::Principal;
use app_depot_domain::tree::{validate_path, TreeEntry, TreeKind};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncRead;
use tokio::sync::{Notify, RwLock, Semaphore};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Target {
    Release {
        #[serde(rename = "releaseId")]
        release_id: String,
    },
    NewApp {
        #[serde(rename = "newAppName")]
        new_app_name: String,
    },
    NewRelease {
        #[serde(rename = "appId")]
        app_id: String,
        #[serde(rename = "createRelease")]
        create_release: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileClaim {
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadCreate {
    pub mode: String,
    pub target: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFile {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub file_id: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum InventoryEntryInput {
    File {
        path: String,
        size: u64,
        sha256: String,
        executable: bool,
    },
    Directory {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum InventoryEntry {
    File {
        path: String,
        #[serde(rename = "fileId")]
        file_id: String,
        size: u64,
        sha256: String,
        executable: bool,
    },
    Directory {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryBatch {
    pub entries: Vec<InventoryEntryInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryBatchResult {
    pub entries: Vec<InventoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPage {
    pub items: Vec<InventoryEntry>,
    pub next_cursor: Option<String>,
    pub inventory_version: u64,
}

impl InventoryEntryInput {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path } => path,
        }
    }
    pub fn tree_entry(&self) -> Result<TreeEntry, UploadError> {
        let (path, kind) = match self {
            Self::File {
                path,
                size,
                sha256,
                executable,
            } => (
                path.clone(),
                TreeKind::File {
                    size: *size,
                    sha256: app_depot_domain::tree::parse_sha256(sha256)
                        .map_err(|_| UploadError::Invalid)?,
                    executable: *executable,
                },
            ),
            Self::Directory { path } => (path.clone(), TreeKind::Directory),
        };
        validate_path(&path).map_err(|_| UploadError::Invalid)?;
        Ok(TreeEntry { path, kind })
    }
}

impl InventoryEntry {
    pub fn tree_entry(&self) -> Result<TreeEntry, UploadError> {
        match self {
            Self::File {
                path,
                size,
                sha256,
                executable,
                ..
            } => InventoryEntryInput::File {
                path: path.clone(),
                size: *size,
                sha256: sha256.clone(),
                executable: *executable,
            }
            .tree_entry(),
            Self::Directory { path } => {
                InventoryEntryInput::Directory { path: path.clone() }.tree_entry()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSession {
    pub id: String,
    pub operation_id: String,
    pub mode: String,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_reason: Option<String>,
    pub inventory_complete: bool,
    pub target: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<UploadFile>,
}

impl UploadSession {
    pub fn file(&self) -> &UploadFile {
        self.file
            .as_ref()
            .expect("transport file required for this mode")
    }
}

#[derive(Debug, Clone)]
pub struct StoredSession {
    pub public: UploadSession,
    pub owner_id: String,
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgress {
    pub session_id: String,
    pub file_id: String,
    pub total_size: u64,
    pub ranges: Vec<Interval>,
    pub next_cursor: Option<String>,
    pub progress_version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizationResult {
    pub session_id: String,
    pub operation_id: String,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub association_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum CancelOutcome {
    Completed(FinalizationResult),
    Accepted {
        operation_id: String,
        status: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("invalid request")]
    Invalid,
    #[error("unsupported artifact kind")]
    UnsupportedKind,
    #[error("invalid ZIP archive")]
    InvalidZip,
    #[error("unsupported ZIP method")]
    UnsupportedZipMethod,
    #[error("not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("incomplete upload")]
    Incomplete,
    #[error("conflicting upload bytes")]
    ConflictingBytes,
    #[error("invalid upload range")]
    InvalidRange,
    #[error("hash mismatch")]
    HashMismatch,
    #[error("immutable release")]
    ImmutableRelease,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("upload progress changed")]
    ProgressChanged,
    #[error("operation running")]
    OperationRunning,
    #[error("session cancelled")]
    SessionCancelled,
    #[error("session invalidated by restore")]
    RestoreInvalidated,
    #[error("session expired")]
    SessionExpired,
    #[error("retryable failure: {0}")]
    Retryable(String),
}

#[allow(async_fn_in_trait)]
pub trait UploadPort: Send + Sync {
    async fn create(
        &self,
        principal: &Principal,
        key: &str,
        input: &UploadCreate,
    ) -> Result<StoredSession, UploadError>;
    async fn get(&self, id: &str) -> Result<StoredSession, UploadError>;
    async fn append_inventory(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
        batch: &InventoryBatch,
    ) -> Result<InventoryBatchResult, UploadError>;
    async fn complete_inventory(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<StoredSession, UploadError>;
    async fn inventory(&self, id: &str) -> Result<Vec<InventoryEntry>, UploadError>;
    async fn inventory_version(&self, id: &str) -> Result<u64, UploadError>;
    async fn receive(
        &self,
        id: &str,
        file_id: &str,
        range: Interval,
        body: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<(), UploadError>;
    async fn progress(&self, id: &str, file_id: &str) -> Result<UploadProgress, UploadError>;
    async fn bind_finalize(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<(), UploadError>;
    async fn freeze(&self, id: &str) -> Result<(), UploadError>;
    async fn verify(&self, id: &str) -> Result<(), UploadError>;
    async fn import_zip(&self, id: &str) -> Result<(), UploadError>;
    async fn prepare_claim(&self, id: &str) -> Result<(), UploadError>;
    async fn promote(&self, id: &str) -> Result<(), UploadError>;
    async fn register(
        &self,
        id: &str,
        principal: &Principal,
    ) -> Result<FinalizationResult, UploadError>;
    async fn fail_hash(&self, id: &str) -> Result<FinalizationResult, UploadError>;
    async fn fail_precommit(
        &self,
        id: &str,
        reason: UploadError,
    ) -> Result<FinalizationResult, UploadError>;
    async fn result(&self, id: &str) -> Result<Option<FinalizationResult>, UploadError>;
    async fn recovery_sessions(&self) -> Result<Vec<String>, UploadError>;
    async fn owner_principal(&self, id: &str) -> Result<Principal, UploadError>;
    async fn reconcile_progress(&self, id: &str) -> Result<(), UploadError>;
    async fn cleanup_pending(&self) -> Result<(), UploadError>;
    async fn gc_unreferenced(&self) -> Result<(), UploadError>;
    async fn cleanup_deletions(&self) -> Result<(), UploadError>;
    async fn cancel(
        &self,
        id: &str,
        principal: &Principal,
        key: &str,
    ) -> Result<CancelOutcome, UploadError>;
    async fn expire_idle(&self, cutoff: i64) -> Result<(), UploadError>;
}

pub struct UploadCoordinator<P> {
    port: P,
    serial: RwLock<()>,
    range_fence: Arc<RangeFence>,
    range_limit: Semaphore,
}

#[derive(Default)]
struct RangeFence {
    active: Mutex<Vec<(String, Interval)>>,
    changed: Notify,
}

struct RangePermit {
    fence: Arc<RangeFence>,
    session_id: String,
    range: Interval,
}

impl Drop for RangePermit {
    fn drop(&mut self) {
        let mut active = self.fence.active.lock().unwrap();
        if let Some(index) = active.iter().position(|(id, range)| {
            id == &self.session_id && range.start == self.range.start && range.end == self.range.end
        }) {
            active.swap_remove(index);
        }
        drop(active);
        self.fence.changed.notify_waiters();
    }
}

impl RangeFence {
    async fn acquire(self: &Arc<Self>, session_id: &str, range: Interval) -> RangePermit {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let acquired = {
                let mut active = self.active.lock().unwrap();
                if active.iter().all(|(id, current)| {
                    id != session_id || range.end <= current.start || range.start >= current.end
                }) {
                    active.push((session_id.into(), range.clone()));
                    true
                } else {
                    false
                }
            };
            if acquired {
                return RangePermit {
                    fence: self.clone(),
                    session_id: session_id.into(),
                    range,
                };
            }
            notified.await;
        }
    }
}

impl<P: UploadPort> UploadCoordinator<P> {
    pub async fn serialize_catalog_mutation<T>(&self, operation: impl Future<Output = T>) -> T {
        let _guard = self.serial.write().await;
        operation.await
    }
    pub fn new(port: P) -> Self {
        Self {
            port,
            serial: RwLock::new(()),
            range_fence: Arc::new(RangeFence::default()),
            range_limit: Semaphore::new(8),
        }
    }

    pub async fn create(
        &self,
        principal: &Principal,
        key: &str,
        input: &UploadCreate,
    ) -> Result<UploadSession, UploadError> {
        let _guard = self.serial.write().await;
        match input.mode.as_str() {
            "BareFile" | "ZipImport" => {
                let file = input.file.as_ref().ok_or(UploadError::Invalid)?;
                if file.size > 1_125_899_906_842_624
                    || app_depot_domain::tree::parse_sha256(&file.sha256).is_err()
                {
                    return Err(UploadError::Invalid);
                }
            }
            "BareTree" => {
                if input.file.is_some() {
                    return Err(UploadError::Invalid);
                }
            }
            _ => return Err(UploadError::UnsupportedKind),
        }
        if let Target::NewApp { new_app_name } = &input.target {
            if new_app_name.trim().is_empty() {
                return Err(UploadError::Invalid);
            }
        }
        if let Target::NewRelease { create_release, .. } = &input.target {
            if !create_release {
                return Err(UploadError::Invalid);
            }
        }
        let session = self.port.create(principal, key, input).await?;
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        Ok(session.public)
    }

    pub async fn append_inventory(
        &self,
        principal: &Principal,
        id: &str,
        key: &str,
        batch: &InventoryBatch,
    ) -> Result<InventoryBatchResult, UploadError> {
        let _guard = self.serial.write().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.mode != "BareTree"
            || session.public.phase != "Receiving"
            || session.public.inventory_complete
        {
            return Err(UploadError::Invalid);
        }
        if batch.entries.is_empty() || batch.entries.len() > 1000 {
            return Err(UploadError::Invalid);
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &batch.entries {
            let tree = entry.tree_entry()?;
            if let TreeKind::File { size, .. } = tree.kind {
                if size > 1_125_899_906_842_624 {
                    return Err(UploadError::Invalid);
                }
            }
            if !seen.insert(entry.path()) {
                return Err(UploadError::Invalid);
            }
        }
        self.port.append_inventory(id, principal, key, batch).await
    }

    pub async fn inventory(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<Vec<InventoryEntry>, UploadError> {
        let _guard = self.serial.read().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.mode != "BareTree" {
            return Err(UploadError::Invalid);
        }
        self.port.inventory(id).await
    }

    pub async fn inventory_page(
        &self,
        principal: &Principal,
        id: &str,
        page_size: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<InventoryPage, UploadError> {
        let size = page_size.unwrap_or(100);
        if !(1..=1000).contains(&size) {
            return Err(UploadError::Invalid);
        }
        let _guard = self.serial.read().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.mode != "BareTree" {
            return Err(UploadError::Invalid);
        }
        let items = self.port.inventory(id).await?;
        let version = self.port.inventory_version(id).await?;
        let offset = if let Some(cursor) = cursor {
            let parts: Vec<&str> = cursor.split('.').collect();
            if parts.len() != 6
                || parts[0] != "i1"
                || parts[1] != id
                || parts[2] != principal.id
                || parts[5] != size.to_string()
            {
                return Err(UploadError::InvalidCursor);
            }
            let expected: u64 = parts[3].parse().map_err(|_| UploadError::InvalidCursor)?;
            let offset: usize = parts[4].parse().map_err(|_| UploadError::InvalidCursor)?;
            if expected != version {
                return Err(UploadError::ProgressChanged);
            }
            if offset == 0 || offset >= items.len() {
                return Err(UploadError::InvalidCursor);
            }
            offset
        } else {
            0
        };
        let end = offset.saturating_add(size).min(items.len());
        let next_cursor =
            (end < items.len()).then(|| format!("i1.{id}.{}.{version}.{end}.{size}", principal.id));
        Ok(InventoryPage {
            items: items[offset..end].to_vec(),
            next_cursor,
            inventory_version: version,
        })
    }

    pub async fn complete_inventory(
        &self,
        principal: &Principal,
        id: &str,
        key: &str,
    ) -> Result<UploadSession, UploadError> {
        let _guard = self.serial.write().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.mode != "BareTree" || session.public.phase != "Receiving" {
            return Err(UploadError::Invalid);
        }
        let entries = self.port.inventory(id).await?;
        let tree: Vec<TreeEntry> = entries
            .iter()
            .map(InventoryEntry::tree_entry)
            .collect::<Result<_, _>>()?;
        app_depot_domain::tree::canonical_stream(&tree).map_err(|_| UploadError::Invalid)?;
        Ok(self
            .port
            .complete_inventory(id, principal, key)
            .await?
            .public)
    }

    pub async fn get(&self, principal: &Principal, id: &str) -> Result<UploadSession, UploadError> {
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        Ok(session.public)
    }

    pub async fn receive(
        &self,
        principal: &Principal,
        id: &str,
        file_id: &str,
        range: Interval,
        body: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<(), UploadError> {
        let _guard = self.serial.read().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.phase != "Receiving" {
            return if session.public.phase == "Cancelled" {
                Err(UploadError::SessionCancelled)
            } else {
                Err(UploadError::InvalidRange)
            };
        }
        if !session.public.inventory_complete && session.public.mode == "BareTree" {
            return Err(UploadError::Incomplete);
        }
        let size = if session.public.mode == "BareTree" {
            self.port
                .inventory(id)
                .await?
                .iter()
                .find_map(|entry| match entry {
                    InventoryEntry::File {
                        file_id: candidate,
                        size,
                        ..
                    } if candidate == file_id => Some(*size),
                    _ => None,
                })
                .ok_or(UploadError::NotFound)?
        } else {
            if session.public.file().file_id != file_id {
                return Err(UploadError::NotFound);
            }
            session.public.file().size
        };
        if range.start >= range.end || range.end > size {
            return Err(UploadError::InvalidRange);
        }
        let _range = self.range_fence.acquire(id, range.clone()).await;
        let _capacity = self
            .range_limit
            .acquire()
            .await
            .map_err(|_| UploadError::Retryable("transfer capacity unavailable".into()))?;
        self.port.receive(id, file_id, range, body).await
    }

    pub async fn progress(
        &self,
        principal: &Principal,
        id: &str,
        file_id: &str,
    ) -> Result<UploadProgress, UploadError> {
        let _guard = self.serial.read().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.phase == "Cancelled" {
            return Err(UploadError::SessionCancelled);
        }
        if !session.public.inventory_complete && session.public.mode == "BareTree" {
            return Err(UploadError::Incomplete);
        }
        self.port.progress(id, file_id).await
    }

    pub async fn progress_page(
        &self,
        principal: &Principal,
        id: &str,
        file_id: &str,
        page_size: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<UploadProgress, UploadError> {
        let size = page_size.unwrap_or(100);
        if !(1..=1000).contains(&size) {
            return Err(UploadError::Invalid);
        }
        let mut progress = self.progress(principal, id, file_id).await?;
        let offset = if let Some(cursor) = cursor {
            let parts: Vec<&str> = cursor.split('.').collect();
            if parts.len() != 7
                || parts[0] != "v1"
                || parts[1] != id
                || parts[2] != file_id
                || parts[3] != principal.id
                || parts[6] != size.to_string()
            {
                return Err(UploadError::InvalidCursor);
            }
            let version: u64 = parts[4].parse().map_err(|_| UploadError::InvalidCursor)?;
            let offset: usize = parts[5].parse().map_err(|_| UploadError::InvalidCursor)?;
            if version != progress.progress_version {
                return Err(UploadError::ProgressChanged);
            }
            if offset == 0 || offset >= progress.ranges.len() {
                return Err(UploadError::InvalidCursor);
            }
            offset
        } else {
            0
        };
        let end = offset.saturating_add(size).min(progress.ranges.len());
        progress.next_cursor = (end < progress.ranges.len()).then(|| {
            format!(
                "v1.{id}.{file_id}.{}.{:}.{}.{size}",
                principal.id, progress.progress_version, end
            )
        });
        progress.ranges = progress.ranges[offset..end].to_vec();
        Ok(progress)
    }

    pub async fn result(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<Option<FinalizationResult>, UploadError> {
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        self.port.result(id).await
    }

    pub async fn finalize(
        &self,
        principal: &Principal,
        id: &str,
        key: &str,
    ) -> Result<FinalizationResult, UploadError> {
        let _guard = self.serial.write().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        if session.public.cancellation_reason.as_deref() == Some("RestoreInvalidated") {
            return Err(UploadError::RestoreInvalidated);
        }
        if session.public.phase == "Cancelled" {
            return Err(UploadError::SessionCancelled);
        }
        self.port.bind_finalize(id, principal, key).await?;
        self.resume_locked(principal, id).await
    }

    async fn resume_locked(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<FinalizationResult, UploadError> {
        if let Some(result) = self.port.result(id).await? {
            return Ok(result);
        }
        let session = self.port.get(id).await?;
        if session.public.phase == "Receiving" {
            self.port.freeze(id).await?;
        }
        match self.port.verify(id).await {
            Err(UploadError::HashMismatch) => {
                let result = self.port.fail_hash(id).await?;
                let _ = self.port.cleanup_pending().await;
                let _ = self.port.gc_unreferenced().await;
                return Ok(result);
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        if session.public.mode == "ZipImport" {
            match self.port.import_zip(id).await {
                Err(reason @ (UploadError::InvalidZip | UploadError::UnsupportedZipMethod)) => {
                    return self.port.fail_precommit(id, reason).await
                }
                other => other?,
            }
        }
        self.port.prepare_claim(id).await?;
        self.port.promote(id).await?;
        let result = match self.port.register(id, principal).await {
            Ok(result) => result,
            Err(
                reason @ (UploadError::NotFound
                | UploadError::Forbidden
                | UploadError::ImmutableRelease),
            ) => self.port.fail_precommit(id, reason).await?,
            Err(error) => {
                if let Some(result) = self.port.result(id).await? {
                    return Ok(result);
                }
                return Err(error);
            }
        };
        let _ = self.port.cleanup_pending().await;
        if result.phase == "Failed" {
            let _ = self.port.gc_unreferenced().await;
        }
        Ok(result)
    }

    pub async fn recover(&self) -> Result<(), UploadError> {
        let _guard = self.serial.write().await;
        for id in self.port.recovery_sessions().await? {
            self.port.reconcile_progress(&id).await?;
            let session = self.port.get(&id).await?;
            if matches!(
                session.public.phase.as_str(),
                "Verifying" | "Importing" | "Committing"
            ) {
                let principal = self.port.owner_principal(&id).await?;
                self.resume_locked(&principal, &id).await?;
            }
        }
        self.port.cleanup_pending().await?;
        self.port.gc_unreferenced().await?;
        self.port.cleanup_deletions().await?;
        Ok(())
    }

    pub async fn cancel(
        &self,
        principal: &Principal,
        id: &str,
        key: &str,
    ) -> Result<CancelOutcome, UploadError> {
        let _guard = self.serial.write().await;
        let session = self.port.get(id).await?;
        if !principal.may_manage(&session.owner_id) {
            return Err(UploadError::NotFound);
        }
        if session.expired {
            return Err(UploadError::SessionExpired);
        }
        let outcome = self.port.cancel(id, principal, key).await?;
        Ok(outcome)
    }

    pub async fn maintenance(&self, cutoff: i64) -> Result<(), UploadError> {
        let _guard = self.serial.write().await;
        self.port.expire_idle(cutoff).await?;
        self.port.cleanup_pending().await?;
        self.port.gc_unreferenced().await?;
        self.port.cleanup_deletions().await
    }
}
