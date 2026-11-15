use app_depot_domain::tree::validate_path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    env,
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        fd::FromRawFd,
        unix::ffi::OsStrExt,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::SystemTime,
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootSpec {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
}

#[derive(Clone)]
pub struct ImportRoot {
    pub id: String,
    pub display_name: String,
    pub kind: &'static str,
    pub container_path: PathBuf,
    pub allowed_user_ids: HashSet<String>,
    directory: Arc<File>,
}

pub struct ImportSources {
    pub roots: Vec<ImportRoot>,
    pub inbox_path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    pub name: String,
    pub entry_type: &'static str,
    pub size: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePage {
    pub items: Vec<SourceEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct SourceFile {
    pub source_path: String,
    pub logical_path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
    pub modified: SystemTime,
}

pub struct Scan {
    pub files: Vec<SourceFile>,
    pub directories: Vec<String>,
    pub total_bytes: u64,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid import source")
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "import source changed during scan",
    )
}

fn open_child(directory: &File, name: &std::ffi::OsStr, directory_only: bool) -> io::Result<File> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory_only { libc::O_DIRECTORY } else { 0 };
    // The directory descriptor stays open while openat resolves one component.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let kind = file.metadata()?.file_type();
    if (directory_only && !kind.is_dir()) || (!directory_only && !kind.is_file()) {
        return Err(invalid());
    }
    Ok(file)
}

fn open_absolute(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut current = File::open("/")?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => current = open_child(&current, name, true)?,
            _ => return Err(invalid()),
        }
    }
    Ok(current)
}

fn components(path: &str) -> io::Result<Vec<&str>> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    validate_path(path).map_err(|_| invalid())?;
    Ok(path.split('/').collect())
}

fn open_relative(root: &File, path: &str, directory_only: bool) -> io::Result<File> {
    let parts = components(path)?;
    if parts.is_empty() {
        if !directory_only {
            return Err(invalid());
        }
        return root.try_clone();
    }
    let mut current = root.try_clone()?;
    for (index, part) in parts.iter().enumerate() {
        current = open_child(
            &current,
            std::ffi::OsStr::new(part),
            index + 1 < parts.len() || directory_only,
        )?;
    }
    Ok(current)
}

fn fd_entries(directory: &File) -> io::Result<Vec<String>> {
    // dup shares the directory offset with the cached root descriptor. Open a new
    // file description so repeated browse and scan requests start at the beginning.
    let duplicate = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let mut entries = Vec::new();
    loop {
        #[cfg(target_os = "linux")]
        let errno = unsafe { libc::__errno_location() };
        #[cfg(target_os = "macos")]
        let errno = unsafe { libc::__error() };
        unsafe { *errno = 0 };
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = unsafe { *errno };
            unsafe { libc::closedir(stream) };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            break;
        }
        let raw = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = match raw.to_str() {
            Ok(name) => name.to_owned(),
            Err(_) => {
                unsafe { libc::closedir(stream) };
                return Err(invalid());
            }
        };
        if name == "." || name == ".." {
            continue;
        }
        if entries.len() >= 1_000_000 {
            unsafe { libc::closedir(stream) };
            return Err(invalid());
        }
        entries.push(name);
    }
    entries.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(entries)
}

fn prepare_inbox(managed: &Path) -> io::Result<(PathBuf, Option<File>)> {
    let inbox_path = managed.parent().ok_or_else(invalid)?.join("inbox");
    if !inbox_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = fs::DirBuilder::new().mode(0o700).create(&inbox_path);
        }
    }
    let inbox_directory = open_absolute(&inbox_path).ok();
    Ok((inbox_path, inbox_directory))
}

pub fn load_roots(data_root: &Path) -> Result<ImportSources, Box<dyn std::error::Error>> {
    let managed = data_root.canonicalize()?;
    let (inbox_path, inbox_directory) = prepare_inbox(&managed)?;
    let mut roots = Vec::new();
    if let Some(directory) = inbox_directory {
        roots.push(ImportRoot {
            id: "inbox".into(),
            display_name: "App Depot Inbox".into(),
            kind: "Inbox",
            container_path: inbox_path.clone(),
            allowed_user_ids: HashSet::new(),
            directory: Arc::new(directory),
        });
    }
    let specs: Vec<RootSpec> = match env::var_os("APP_DEPOT_IMPORT_ROOTS") {
        Some(raw) => serde_json::from_str(&raw.to_string_lossy())?,
        None => Vec::new(),
    };
    let mut ids = HashSet::from([String::from("inbox")]);
    for spec in specs {
        if spec.id.is_empty()
            || spec.id.len() > 64
            || !spec.id.as_bytes()[0].is_ascii_lowercase()
            || !spec
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !ids.insert(spec.id.clone())
        {
            return Err("import root IDs must be unique lowercase identifiers".into());
        }
        let directory = open_absolute(&spec.path)?;
        let source = spec.path.canonicalize()?;
        if source.starts_with(&managed)
            || managed.starts_with(&source)
            || source.starts_with(&inbox_path)
            || inbox_path.starts_with(&source)
        {
            return Err("configured import roots must not overlap managed data or Inbox".into());
        }
        for user in &spec.allowed_user_ids {
            uuid::Uuid::parse_str(user)?;
        }
        roots.push(ImportRoot {
            display_name: spec.id.clone(),
            kind: "Configured",
            container_path: source,
            id: spec.id,
            allowed_user_ids: spec.allowed_user_ids.into_iter().collect(),
            directory: Arc::new(directory),
        });
    }
    Ok(ImportSources { roots, inbox_path })
}

impl ImportRoot {
    pub fn available(&self) -> bool {
        let Ok(current) = open_absolute(&self.container_path) else {
            return false;
        };
        match (self.directory.metadata(), current.metadata()) {
            (Ok(held), Ok(current)) => held.dev() == current.dev() && held.ino() == current.ino(),
            _ => false,
        }
    }

    pub fn allows(&self, user_id: &str, server_admin: bool) -> bool {
        server_admin || self.allowed_user_ids.contains(user_id)
    }

    pub fn browse(
        &self,
        path: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> io::Result<SourcePage> {
        if !(1..=200).contains(&page_size) {
            return Err(invalid());
        }
        if let Some(cursor) = cursor {
            if cursor.is_empty() || cursor.contains('/') || cursor == "." || cursor == ".." {
                return Err(invalid());
            }
        }
        let directory = open_relative(&self.directory, path, true)?;
        let mut items = Vec::new();
        let mut has_more = false;
        for name in fd_entries(&directory)? {
            if cursor.is_some_and(|cursor| name.as_bytes() <= cursor.as_bytes()) {
                continue;
            }
            let child = match open_child(&directory, std::ffi::OsStr::new(&name), false) {
                Ok(file) => file,
                Err(_) => match open_child(&directory, std::ffi::OsStr::new(&name), true) {
                    Ok(dir) => dir,
                    Err(_) => continue,
                },
            };
            let metadata = child.metadata()?;
            let (entry_type, size) = if metadata.is_dir() {
                ("Directory", None)
            } else if metadata.is_file() {
                ("File", Some(metadata.len()))
            } else {
                continue;
            };
            if items.len() == page_size {
                has_more = true;
                break;
            }
            items.push(SourceEntry {
                name,
                entry_type,
                size,
            });
        }
        let next_cursor = has_more.then(|| items.last().unwrap().name.clone());
        Ok(SourcePage { items, next_cursor })
    }

    pub fn open_file(&self, path: &str) -> io::Result<File> {
        open_relative(&self.directory, path, false)
    }

    pub fn scan_with_cancel(
        &self,
        path: &str,
        mode: &str,
        cancelled: &AtomicBool,
    ) -> io::Result<Scan> {
        let mut scan = Scan {
            files: Vec::new(),
            directories: Vec::new(),
            total_bytes: 0,
        };
        match mode {
            "BareFile" | "ZipImport" => {
                let file = self.open_file(path)?;
                let (size, sha256, modified) = hash_file(file, cancelled)?;
                scan.total_bytes = size;
                scan.files.push(SourceFile {
                    source_path: path.into(),
                    logical_path: String::new(),
                    size,
                    sha256,
                    executable: false,
                    modified,
                });
            }
            "BareTree" => {
                let directory = open_relative(&self.directory, path, true)?;
                scan_directory(&directory, path, "", &mut scan, 0, cancelled)?;
                if scan.files.is_empty() {
                    return Err(invalid());
                }
                scan.files
                    .sort_by(|a, b| a.logical_path.as_bytes().cmp(b.logical_path.as_bytes()));
                scan.directories
                    .sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            }
            _ => return Err(invalid()),
        }
        Ok(scan)
    }
}

impl SourceFile {
    pub fn matches_metadata(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_file()
            && metadata.len() == self.size
            && metadata.modified().ok() == Some(self.modified)
    }
}

fn hash_file(mut file: File, cancelled: &AtomicBool) -> io::Result<(u64, String, SystemTime)> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() > 1_125_899_906_842_624 {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "import cancelled",
            ));
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size = size.checked_add(count as u64).ok_or_else(invalid)?;
    }
    let after = file.metadata()?;
    let modified = before.modified()?;
    if size != before.len() || size != after.len() || modified != after.modified()? {
        return Err(changed());
    }
    Ok((size, hex::encode(hash.finalize()), modified))
}

fn scan_directory(
    directory: &File,
    source_prefix: &str,
    logical_prefix: &str,
    scan: &mut Scan,
    depth: usize,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    if depth > 128 {
        return Err(invalid());
    }
    let before = directory.metadata()?;
    for name in fd_entries(directory)? {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "import cancelled",
            ));
        }
        let logical = if logical_prefix.is_empty() {
            name.clone()
        } else {
            format!("{logical_prefix}/{name}")
        };
        validate_path(&logical).map_err(|_| invalid())?;
        if scan.files.len() + scan.directories.len() >= 1_000_000 {
            return Err(invalid());
        }
        let source = if source_prefix.is_empty() {
            name.clone()
        } else {
            format!("{source_prefix}/{name}")
        };
        if let Ok(child) = open_child(directory, std::ffi::OsStr::new(&name), true) {
            scan.directories.push(logical.clone());
            scan_directory(&child, &source, &logical, scan, depth + 1, cancelled)?;
        } else {
            let child = open_child(directory, std::ffi::OsStr::new(&name), false)?;
            let executable = child.metadata()?.permissions().mode() & 0o111 != 0;
            let (size, sha256, modified) = hash_file(child, cancelled)?;
            scan.total_bytes = scan.total_bytes.checked_add(size).ok_or_else(invalid)?;
            if scan.total_bytes > 1_125_899_906_842_624 {
                return Err(invalid());
            }
            scan.files.push(SourceFile {
                source_path: source,
                logical_path: logical,
                size,
                sha256,
                executable,
                modified,
            });
        }
    }
    let after = directory.metadata()?;
    if before.modified()? != after.modified()? {
        return Err(changed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn no_symlink_or_special_file_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let directory = open_absolute(&root.path().canonicalize().unwrap()).unwrap();
        assert!(open_relative(&directory, "escape/secret", false).is_err());
        assert!(open_relative(&directory, "../secret", false).is_err());
        assert!(open_relative(&directory, "/etc/passwd", false).is_err());
    }

    #[test]
    fn scan_preserves_nested_and_empty_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("tree/empty/nested")).unwrap();
        fs::write(root.path().join("tree/a"), b"A").unwrap();
        let source = ImportRoot {
            id: "test".into(),
            display_name: "Test".into(),
            kind: "Configured",
            container_path: root.path().to_path_buf(),
            allowed_user_ids: HashSet::new(),
            directory: Arc::new(open_absolute(&root.path().canonicalize().unwrap()).unwrap()),
        };
        let scan = source
            .scan_with_cancel("tree", "BareTree", &AtomicBool::new(false))
            .unwrap();
        assert_eq!(scan.directories, ["empty", "empty/nested"]);
        assert_eq!(scan.files[0].logical_path, "a");
        assert_eq!(
            scan.files[0].sha256,
            "559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"
        );
        for _ in 0..3 {
            let page = source.browse("tree", None, 100).unwrap();
            assert_eq!(page.items.len(), 2);
            assert_eq!(page.items[0].name, "a");
            assert_eq!(page.items[1].name, "empty");
        }
        let first = source.browse("tree", None, 1).unwrap();
        assert_eq!(first.items[0].name, "a");
        let second = source
            .browse("tree", first.next_cursor.as_deref(), 1)
            .unwrap();
        assert_eq!(second.items[0].name, "empty");
        assert!(second.next_cursor.is_none());
        assert_eq!(
            source
                .scan_with_cancel("tree", "BareTree", &AtomicBool::new(false))
                .unwrap()
                .files
                .len(),
            1
        );
        let claim = &scan.files[0];
        assert!(claim.matches_metadata(&fs::metadata(root.path().join("tree/a")).unwrap()));
        fs::write(root.path().join("tree/a"), b"changed bytes").unwrap();
        assert!(!claim.matches_metadata(&fs::metadata(root.path().join("tree/a")).unwrap()));
    }

    #[test]
    fn inbox_is_sibling_persistent_and_rejects_symlink_to_managed_root() {
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().canonicalize().unwrap();
        let managed = base.join("production");
        fs::create_dir(&managed).unwrap();
        let (path, opened) = prepare_inbox(&managed).unwrap();
        assert_eq!(path, base.join("inbox"));
        assert!(opened.is_some());
        fs::write(path.join("original.bin"), b"original").unwrap();
        let (_, reopened) = prepare_inbox(&managed).unwrap();
        assert!(reopened.is_some());
        assert_eq!(fs::read(path.join("original.bin")).unwrap(), b"original");
        drop(opened);
        drop(reopened);
        fs::remove_file(path.join("original.bin")).unwrap();
        fs::remove_dir(&path).unwrap();
        std::os::unix::fs::symlink(&managed, &path).unwrap();
        let (_, unsafe_root) = prepare_inbox(&managed).unwrap();
        assert!(unsafe_root.is_none());
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"occupied").unwrap();
        let (_, unavailable) = prepare_inbox(&managed).unwrap();
        assert!(unavailable.is_none());
    }
}
