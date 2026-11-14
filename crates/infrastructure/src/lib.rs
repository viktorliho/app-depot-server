use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};
use uuid::Uuid;

pub mod artwork;
pub mod auth;
pub mod backups;
pub mod catalog;
pub mod channels;
pub mod declarations;
pub mod restore;
pub mod upload;
mod zip_import;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("unsafe or invalid data root: {0}")]
    InvalidRoot(&'static str),
    #[error("data root is owned by another process")]
    Locked,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalMode {
    Dev,
    Preview,
    Production,
}

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    format: u32,
    mode: LocalMode,
}

pub struct RootLock {
    _file: File,
    pub path: PathBuf,
}

fn validate_instance_name(name: &str) -> Result<(), StorageError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StorageError::InvalidRoot(
            "instance name must be a simple path component",
        ));
    }
    Ok(())
}

fn checked_child(namespace: &Path, name: &str) -> Result<PathBuf, StorageError> {
    validate_instance_name(name)?;
    let ns = namespace.canonicalize()?;
    if !ns.is_dir() {
        return Err(StorageError::InvalidRoot("namespace is not a directory"));
    }
    let child = ns.join(name);
    if child.exists() {
        if child.symlink_metadata()?.file_type().is_symlink() {
            return Err(StorageError::InvalidRoot("instance is a symlink"));
        }
        if child.canonicalize()? != child || !child.is_dir() {
            return Err(StorageError::InvalidRoot(
                "instance escapes namespace or is not a directory",
            ));
        }
    }
    Ok(child)
}

fn write_marker(root: &Path, mode: LocalMode) -> Result<(), StorageError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(".app-depot-instance"))?;
    serde_json::to_writer(&mut file, &Marker { format: 1, mode })?;
    file.flush()?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn read_marker(root: &Path) -> Result<Marker, StorageError> {
    let marker_path = root.join(".app-depot-instance");
    if marker_path.symlink_metadata()?.file_type().is_symlink() {
        return Err(StorageError::InvalidRoot("marker is a symlink"));
    }
    let marker: Marker = serde_json::from_reader(File::open(marker_path)?)?;
    if marker.format != 1 {
        return Err(StorageError::InvalidRoot("unknown instance marker"));
    }
    Ok(marker)
}

pub fn init_local(namespace: &Path, name: &str, mode: LocalMode) -> Result<PathBuf, StorageError> {
    let root = checked_child(namespace, name)?;
    let parent_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            root.parent()
                .ok_or(StorageError::InvalidRoot("invalid instance parent"))?
                .join(".app-depot-restore-lock"),
        )?;
    parent_lock
        .try_lock_exclusive()
        .map_err(|_| StorageError::Locked)?;
    if root
        .parent()
        .ok_or(StorageError::InvalidRoot("invalid instance parent"))?
        .join(format!(".restore-{name}"))
        .exists()
    {
        return Err(StorageError::InvalidRoot(
            "restore activation is incomplete",
        ));
    }
    if root.exists() {
        return Err(StorageError::InvalidRoot("instance already exists"));
    }
    fs::create_dir(&root)?;
    if mode == LocalMode::Production {
        private_directory(&root)?;
    }
    for part in ["db", "cas", "staging", "backups", "ops"] {
        let child = root.join(part);
        fs::create_dir(&child)?;
        if mode == LocalMode::Production {
            private_directory(&child)?;
        }
    }
    File::create_new(root.join(".app-depot-lock"))?.sync_all()?;
    write_marker(&root, mode)?;
    Ok(root)
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

pub fn lock_production(data_root: &Path) -> Result<RootLock, StorageError> {
    if !data_root.is_absolute() {
        return Err(StorageError::InvalidRoot(
            "Production data root must be absolute",
        ));
    }
    let name = data_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(StorageError::InvalidRoot("invalid Production data root"))?;
    validate_instance_name(name)?;
    let parent = data_root
        .parent()
        .ok_or(StorageError::InvalidRoot("invalid Production data root"))?;
    if !data_root.exists() {
        init_local(parent, name, LocalMode::Production)?;
    }
    let lock = lock_local(parent, name, LocalMode::Production)?;
    let (mounts, cross_filesystem_cas_promotion) = production_storage_preflight(&lock.path)?;
    println!("{{\"event\":\"storage_topology\",\"mountCount\":{mounts},\"crossFilesystemCasPromotion\":{cross_filesystem_cas_promotion}}}");
    Ok(lock)
}

fn production_storage_preflight(root: &Path) -> Result<(usize, bool), StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return Err(StorageError::InvalidRoot(
                "Production must run as a non-root user",
            ));
        }
        for part in ["", "db", "cas", "staging", "backups", "ops"] {
            let path = root.join(part);
            if path.metadata()?.permissions().mode() & 0o077 != 0 {
                return Err(StorageError::InvalidRoot(
                    "Production directories must be owner-only",
                ));
            }
        }
    }
    reject_remote_sqlite_fs(&root.join("db"))?;
    let topology = mount_topology(root)?;
    for part in ["db", "cas", "staging", "backups", "ops"] {
        let directory = root.join(part);
        let id = Uuid::new_v4();
        let file = directory.join(format!(".probe-{id}"));
        let renamed = directory.join(format!(".probe-renamed-{id}"));
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)?;
        handle.write_all(b"storage preflight")?;
        handle.sync_all()?;
        fs::rename(&file, &renamed)?;
        File::open(&directory)?.sync_all()?;
        fs::remove_file(&renamed)?;
        File::open(&directory)?.sync_all()?;
    }
    let cas = root.join("cas");
    let id = Uuid::new_v4();
    let candidate = cas.join(format!(".probe-candidate-{id}"));
    let installed = cas.join(format!(".probe-installed-{id}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)?;
    file.write_all(b"atomic install")?;
    file.sync_all()?;
    fs::hard_link(&candidate, &installed)?;
    File::open(&cas)?.sync_all()?;
    fs::remove_file(candidate)?;
    fs::remove_file(installed)?;
    File::open(&cas)?.sync_all()?;
    Ok(topology)
}

fn mount_topology(root: &Path) -> Result<(usize, bool), StorageError> {
    let boundaries = ["", "db", "cas", "staging", "backups", "ops"]
        .iter()
        .map(|part| mount_boundary(&root.join(part)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(summarize_mounts(&boundaries))
}

fn summarize_mounts(boundaries: &[String]) -> (usize, bool) {
    use std::collections::HashSet;
    let count = boundaries.iter().collect::<HashSet<_>>().len();
    (count, boundaries[2] != boundaries[3])
}

#[cfg(target_os = "linux")]
fn mount_boundary(path: &Path) -> Result<String, StorageError> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| StorageError::InvalidRoot("invalid managed directory path"))?;
    let mut info: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &mut info,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if info.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(StorageError::InvalidRoot(
            "mount boundary identity unavailable",
        ));
    }
    Ok(info.stx_mnt_id.to_string())
}

#[cfg(target_os = "macos")]
fn mount_boundary(path: &Path) -> Result<String, StorageError> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| StorageError::InvalidRoot("invalid managed directory path"))?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mount = unsafe { std::ffi::CStr::from_ptr(info.f_mntonname.as_ptr()) };
    Ok(mount.to_string_lossy().into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn mount_boundary(_path: &Path) -> Result<String, StorageError> {
    Err(StorageError::InvalidRoot(
        "mount boundary inspection is unsupported",
    ))
}

#[cfg(target_os = "linux")]
fn reject_remote_sqlite_fs(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| StorageError::InvalidRoot("invalid SQLite path"))?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if matches!(
        info.f_type as u64,
        0x6969 | 0xff534d42 | 0xfe534d42 | 0x517b
    ) {
        return Err(StorageError::InvalidRoot(
            "SQLite on NFS or SMB is unsupported",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_remote_sqlite_fs(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| StorageError::InvalidRoot("invalid SQLite path"))?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let name = unsafe { std::ffi::CStr::from_ptr(info.f_fstypename.as_ptr()) }.to_bytes();
    if name == b"nfs" || name == b"smbfs" {
        return Err(StorageError::InvalidRoot(
            "SQLite on NFS or SMB is unsupported",
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reject_remote_sqlite_fs(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

pub fn lock_local(namespace: &Path, name: &str, mode: LocalMode) -> Result<RootLock, StorageError> {
    let root = checked_child(namespace, name)?;
    if root
        .parent()
        .ok_or(StorageError::InvalidRoot("invalid instance parent"))?
        .join(format!(".restore-{name}"))
        .exists()
    {
        return Err(StorageError::InvalidRoot(
            "restore activation is incomplete",
        ));
    }
    if !root.exists() {
        return Err(StorageError::InvalidRoot(
            "instance has not been initialized",
        ));
    }
    if read_marker(&root)?.mode != mode {
        return Err(StorageError::InvalidRoot("instance mode mismatch"));
    }
    for part in ["db", "cas", "staging", "backups", "ops"] {
        let child = root.join(part);
        if child.symlink_metadata()?.file_type().is_symlink()
            || child.canonicalize()? != child
            || !child.is_dir()
        {
            return Err(StorageError::InvalidRoot(
                "managed directory escapes data root",
            ));
        }
    }
    let path = root.join(".app-depot-lock");
    if path.exists() && path.symlink_metadata()?.file_type().is_symlink() {
        return Err(StorageError::InvalidRoot("lock is a symlink"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock_exclusive()
        .map_err(|_| StorageError::Locked)?;
    Ok(RootLock {
        _file: file,
        path: root,
    })
}

pub fn reset_dev(namespace: &Path, name: &str) -> Result<(), StorageError> {
    let lock = lock_local(namespace, name, LocalMode::Dev)?;
    // Only the direct, marked Dev child of the explicit namespace is removed.
    fs::remove_dir_all(&lock.path)?;
    File::open(namespace.canonicalize()?)?.sync_all()?;
    Ok(())
}

pub async fn open_catalog(lock: &RootLock) -> Result<SqlitePool, StorageError> {
    let options = SqliteConnectOptions::from_str("sqlite:catalog.sqlite")?
        .filename(lock.path.join("db/catalog.sqlite"))
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let check: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if check != "ok" {
        return Err(StorageError::InvalidRoot("SQLite integrity check failed"));
    }
    Ok(pool)
}

pub async fn seed_dev(pool: &SqlitePool) -> Result<(), StorageError> {
    let sample = b"App Depot M0 sample\n";
    let digest = format!("{:x}", Sha256::digest(sample));
    sqlx::query("INSERT INTO dev_fixture_versions(version, sample_name, sample_sha256) VALUES(1, 'minimal-file', ?) ON CONFLICT(version) DO UPDATE SET sample_name=excluded.sample_name, sample_sha256=excluded.sample_sha256")
        .bind(digest).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migration_and_fixture_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = init_local(dir.path(), "dev-one", LocalMode::Dev).unwrap();
        assert!(root.join("cas").is_dir());
        let lock = lock_local(dir.path(), "dev-one", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        seed_dev(&pool).await.unwrap();
        seed_dev(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM dev_fixture_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn incompatible_downgrade_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "future", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "future", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time) VALUES(9999, 'future schema', 1, x'00', 0)")
            .execute(&pool).await.unwrap();
        pool.close().await;
        assert!(open_catalog(&lock).await.is_err());
    }

    #[tokio::test]
    async fn failed_migration_and_sqlite_corruption_refuse_catalog_service() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "failed-migration", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "failed-migration", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time) VALUES(9999, 'interrupted schema', 0, x'00', 0)")
            .execute(&pool).await.unwrap();
        pool.close().await;
        assert!(open_catalog(&lock).await.is_err());
        drop(lock);

        init_local(dir.path(), "corrupt", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "corrupt", LocalMode::Dev).unwrap();
        fs::write(lock.path.join("db/catalog.sqlite"), b"invalid SQLite bytes").unwrap();
        assert!(open_catalog(&lock).await.is_err());
        drop(lock);
        let report = restore::diagnose(&dir.path().join("corrupt"))
            .await
            .unwrap();
        assert_eq!(report["status"], "Corrupt");
    }

    #[test]
    fn incomplete_restore_marker_blocks_service_lock() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "preview", LocalMode::Preview).unwrap();
        fs::write(dir.path().join(".restore-preview"), b"activation pending").unwrap();
        assert!(lock_local(dir.path(), "preview", LocalMode::Preview).is_err());
    }

    #[test]
    fn production_root_is_private_and_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("production");
        let lock = lock_production(&root).unwrap();
        assert!(root.join("db").is_dir());
        assert_eq!(mount_topology(&root).unwrap(), (1, false));
        let synthetic = ["root", "db", "cas", "staging", "db", "root"].map(str::to_owned);
        assert_eq!(summarize_mounts(&synthetic), (4, true));
        assert!(lock_production(&root).is_err());
        drop(lock);
        assert!(lock_production(&root).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(lock_production(&root).is_err());
        }
    }

    #[test]
    fn reset_rejects_preview_symlink_and_locked_dev() {
        let dir = tempfile::tempdir().unwrap();
        init_local(dir.path(), "preview", LocalMode::Preview).unwrap();
        assert!(reset_dev(dir.path(), "preview").is_err());
        init_local(dir.path(), "dev", LocalMode::Dev).unwrap();
        let lock = lock_local(dir.path(), "dev", LocalMode::Dev).unwrap();
        assert!(reset_dev(dir.path(), "dev").is_err());
        drop(lock);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("preview"), dir.path().join("escape"))
                .unwrap();
            assert!(reset_dev(dir.path(), "escape").is_err());
        }
        reset_dev(dir.path(), "dev").unwrap();
        assert!(!dir.path().join("dev").exists());
    }
}
