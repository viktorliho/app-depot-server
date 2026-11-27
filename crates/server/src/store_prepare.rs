use app_depot_domain::tree::validate_path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::TempDir;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema: String,
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub upstream: String,
    pub availability: String,
    pub rights: String,
    pub platform: String,
    pub version: String,
    pub components: Vec<Component>,
    pub prepare: Vec<Step>,
    pub required: Vec<String>,
    pub launch: Launch,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<ReleaseAsset>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAsset {
    pub repository: String,
    pub prefix: String,
    pub suffix: String,
    pub previews: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    operation: String,
    component: Option<String>,
    from_prefix: Option<String>,
    to_prefix: Option<String>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    on_conflict: String,
    from: Option<String>,
    to: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub path: String,
    pub cwd: String,
    pub args: Vec<String>,
}
fn invalid() -> io::Error {
    io::Error::other("Invalid Store recipe or prepared content")
}
pub(super) fn identifier(s: &str) -> bool {
    s.len() <= 128
        && s.split(['.', '-']).all(|n| {
            !n.is_empty()
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}
fn path(s: &str) -> bool {
    s.len() <= 1024
        && s.split('/').count() <= 32
        && !s.chars().any(char::is_control)
        && validate_path(s).is_ok()
}
fn prefix(s: &str) -> bool {
    s.is_empty() || s.strip_suffix('/').is_some_and(path)
}
fn https(s: &str) -> bool {
    let Ok(url) = s.parse::<axum::http::Uri>() else {
        return false;
    };
    url.scheme_str() == Some("https") && url.authority().is_some_and(|a| !a.as_str().contains('@'))
}
pub fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Recipe {
    pub fn validate(&self) -> io::Result<()> {
        let mut names = HashSet::new();
        if !identifier(&self.id)
            || self.rights.len() > 4096
            || self.version.trim().is_empty()
            || self.version.len() > 128
            || self.revision > i64::MAX as u64
            || self.schema != "StoreRecipeV1"
            || self.platform != "linux-x86_64"
            || !matches!(self.availability.as_str(), "available" | "reviewRequired")
            || self.title.trim().is_empty()
            || self.title.len() > 160
            || self.rights.trim().is_empty()
            || self.revision == 0
            || !https(&self.upstream)
            || !(1..=16).contains(&self.components.len())
            || self
                .components
                .iter()
                .map(|c| u128::from(c.bytes))
                .sum::<u128>()
                > 16 * 1024u128.pow(3)
            || !(1..=128).contains(&self.prepare.len())
            || self.required.len() > 128
            || self.required.iter().any(|s| !path(s))
            || !path(&self.launch.path)
            || (!self.launch.cwd.is_empty() && !path(&self.launch.cwd))
            || self.launch.args.len() > 64
            || self
                .launch
                .args
                .iter()
                .any(|s| s.len() > 4096 || s.chars().any(char::is_control))
        {
            return Err(invalid());
        }
        for c in &self.components {
            if !identifier(&c.name)
                || c.name.contains('/')
                || !names.insert(&c.name)
                || !https(&c.url)
                || c.github.as_ref().is_some_and(|g| {
                    c.discovery.is_some()
                        || g.repository.split('/').count() != 2
                        || !g.repository.split('/').all(|n| {
                            !n.is_empty()
                                && n.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                        })
                        || g.prefix.is_empty()
                        || g.suffix.is_empty()
                })
                || c.discovery.as_ref().is_some_and(|u| !https(u))
                || !valid_digest(&c.sha256)
                || !(1..=8 * 1024u64.pow(3)).contains(&c.bytes)
                || !matches!(
                    c.format.as_str(),
                    "iso9660" | "7z" | "tar.bz2" | "zip" | "raw"
                )
            {
                return Err(invalid());
            }
        }
        for s in &self.prepare {
            if !matches!(s.on_conflict.as_str(), "replace" | "reject") {
                return Err(invalid());
            }
            if s.operation == "copy" {
                if s.component.is_some()
                    || s.include.is_some()
                    || s.exclude.is_some()
                    || s.from_prefix.is_some()
                    || s.to_prefix.is_some()
                    || !s.from.as_deref().is_some_and(path)
                    || !s.to.as_deref().is_some_and(path)
                    || s.from == s.to
                {
                    return Err(invalid());
                }
                continue;
            }
            if !matches!(
                s.operation.as_str(),
                "" | "extract" | "installshield-cab" | "unreal-uz"
            ) || !s.component.as_ref().is_some_and(|c| names.contains(c))
                || !s.from_prefix.as_deref().is_some_and(prefix)
                || !s.to_prefix.as_deref().is_some_and(prefix)
                || s.from.is_some()
                || s.to.is_some()
                || s.include
                    .as_ref()
                    .is_none_or(|p| p.is_empty() || p.len() > 128)
                || s.exclude.as_ref().is_none_or(|p| p.len() > 128)
                || s.include
                    .iter()
                    .chain(&s.exclude)
                    .flatten()
                    .any(|p| !path(p))
                || (s.operation == "installshield-cab"
                    && s.from_prefix
                        .as_deref()
                        .is_none_or(|p| p.is_empty() || p.trim_end_matches('/').contains('/')))
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

pub(super) fn download(c: &Component, dest: &Path) -> io::Result<()> {
    let mut child = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--max-time",
            "1800",
            "--max-filesize",
            &c.bytes.to_string(),
            "--",
            &c.url,
        ])
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut out = File::create_new(dest)?;
        let mut hash = Sha256::new();
        let mut n = 0;
        let mut buf = [0u8; 65536];
        let mut stream = child.stdout.take().unwrap();
        loop {
            let count = stream.read(&mut buf)?;
            if count == 0 {
                break;
            }
            n += count as u64;
            if n > c.bytes {
                return Err(invalid());
            }
            hash.update(&buf[..count]);
            out.write_all(&buf[..count])?;
        }
        if n != c.bytes || hex::encode(hash.finalize()) != c.sha256 {
            return Err(io::Error::other("Component length or SHA-256 mismatch"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    if !child.wait()?.success() {
        return Err(io::Error::other("Component download failed"));
    }
    result
}

fn matches(pattern: &str, s: &str) -> bool {
    if pattern == "**" {
        return true;
    }
    if let Some(dir) = pattern.strip_suffix("/**") {
        return s.strip_prefix(dir).is_some_and(|p| p.starts_with('/'));
    }
    if let Some(ext) = pattern.strip_prefix('*') {
        return !s.contains('/') && s.ends_with(ext);
    }
    s == pattern
}

// Materialize only regular entries through libarchive's streaming interface. The parser
// never receives an output path. Internal aliases become regular copies.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod archive {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    #[link(name = "archive")]
    unsafe extern "C" {
        fn archive_read_new() -> *mut c_void;
        fn archive_read_free(a: *mut c_void) -> c_int;
        fn archive_read_support_filter_bzip2(a: *mut c_void) -> c_int;
        fn archive_read_support_format_iso9660(a: *mut c_void) -> c_int;
        fn archive_read_support_format_tar(a: *mut c_void) -> c_int;
        fn archive_read_support_format_zip(a: *mut c_void) -> c_int;
        fn archive_read_support_format_7zip(a: *mut c_void) -> c_int;
        fn archive_read_open_filename(a: *mut c_void, name: *const c_char, block: usize) -> c_int;
        fn archive_read_next_header(a: *mut c_void, entry: *mut *mut c_void) -> c_int;
        fn archive_entry_pathname(e: *mut c_void) -> *const c_char;
        fn archive_entry_filetype(e: *mut c_void) -> libc::mode_t;
        fn archive_entry_symlink(e: *mut c_void) -> *const c_char;
        fn archive_entry_perm(e: *mut c_void) -> libc::mode_t;
        fn archive_entry_hardlink(e: *mut c_void) -> *const c_char;
        fn archive_read_data(a: *mut c_void, buf: *mut c_void, n: usize) -> isize;
    }
    struct Reader(*mut c_void);
    impl Drop for Reader {
        fn drop(&mut self) {
            unsafe {
                archive_read_free(self.0);
            }
        }
    }
    pub(super) fn extract(
        input: &Path,
        format: &str,
        out: &Path,
        budget: &mut u64,
        count: &mut usize,
    ) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        let mut aliases = Vec::new();
        let mut seen = HashSet::new();
        let mut buf = [0u8; 65536];
        unsafe {
            let a = Reader(archive_read_new());
            if a.0.is_null() {
                return Err(invalid());
            }
            let enable = match format {
                "iso9660" => archive_read_support_format_iso9660(a.0),
                "tar.bz2" => {
                    archive_read_support_filter_bzip2(a.0);
                    archive_read_support_format_tar(a.0)
                }
                "zip" => archive_read_support_format_zip(a.0),
                "7z" => archive_read_support_format_7zip(a.0),
                _ => return Err(invalid()),
            };
            let file = CString::new(input.as_os_str().as_bytes())?;
            if enable != 0 || archive_read_open_filename(a.0, file.as_ptr(), 65536) != 0 {
                return Err(invalid());
            }
            loop {
                let mut entry = std::ptr::null_mut();
                let status = archive_read_next_header(a.0, &mut entry);
                if status == 1 {
                    break;
                }
                if status != 0 || entry.is_null() || *count == 0 {
                    return Err(invalid());
                }
                *count -= 1;
                let ptr = archive_entry_pathname(entry);
                if ptr.is_null() {
                    return Err(invalid());
                }
                let raw = CStr::from_ptr(ptr).to_str().map_err(|_| invalid())?;
                let name = raw.trim_start_matches("./").trim_end_matches('/');
                if archive_entry_filetype(entry) == libc::S_IFDIR && matches!(name, "" | ".") {
                    continue;
                }
                if !path(name) || !seen.insert(name.to_owned()) {
                    return Err(invalid());
                }
                if archive_entry_filetype(entry) == libc::S_IFDIR {
                    continue;
                }
                if archive_entry_filetype(entry) == libc::S_IFLNK {
                    let ptr = archive_entry_symlink(entry);
                    if ptr.is_null() {
                        return Err(invalid());
                    }
                    let target = CStr::from_ptr(ptr).to_str().map_err(|_| invalid())?;
                    let mut resolved = Path::new(name).parent().ok_or_else(invalid)?.to_path_buf();
                    for part in Path::new(target).components() {
                        match part {
                            std::path::Component::Normal(n) => resolved.push(n),
                            std::path::Component::CurDir => {}
                            std::path::Component::ParentDir if resolved.pop() => {}
                            _ => return Err(invalid()),
                        }
                    }
                    let resolved = resolved.to_str().filter(|p| path(p)).ok_or_else(invalid)?;
                    aliases.push((name.to_owned(), resolved.to_owned()));
                    continue;
                }
                if archive_entry_filetype(entry) != libc::S_IFREG
                    || !archive_entry_hardlink(entry).is_null()
                {
                    return Err(invalid());
                }
                let dest = out.join(name);
                fs::create_dir_all(dest.parent().ok_or_else(invalid)?)?;
                let mode = if archive_entry_perm(entry) & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                };
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(dest)?;
                loop {
                    let n = archive_read_data(a.0, buf.as_mut_ptr().cast(), buf.len());
                    if n == 0 {
                        break;
                    }
                    if n < 0 || n as u64 > *budget {
                        return Err(invalid());
                    }
                    *budget -= n as u64;
                    f.write_all(&buf[..n as usize])?;
                }
                names.push(name.to_owned());
            }
        }
        let regular: HashSet<_> = names.iter().cloned().collect();
        for (name, target) in aliases {
            if !regular.contains(&target) {
                return Err(invalid());
            }
            let source = out.join(target);
            let size = source.metadata()?.len();
            *budget = budget.checked_sub(size).ok_or_else(invalid)?;
            let dest = out.join(&name);
            fs::create_dir_all(dest.parent().ok_or_else(invalid)?)?;
            let mut original = File::open(source)?;
            let mut copy = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(original.metadata()?.permissions().mode() & 0o777)
                .open(dest)?;
            io::copy(&mut original, &mut copy)?;
            names.push(name);
        }
        Ok(names)
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod archive {
    use super::*;
    pub(super) fn extract(
        _: &Path,
        _: &str,
        _: &Path,
        _: &mut u64,
        _: &mut usize,
    ) -> io::Result<Vec<String>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

// Only the fixed Server helper receives the parser and its private output directory.
pub(super) fn unpack_worker(args: &[String]) -> io::Result<()> {
    let [input, format, destination, bytes, entries] = args else {
        return Err(invalid());
    };
    let mut budget = bytes.parse::<u64>().map_err(|_| invalid())?;
    let mut count = entries.parse::<usize>().map_err(|_| invalid())?;
    if budget > 16 * 1024u64.pow(3) || count > 50000 {
        return Err(invalid());
    }
    let names = archive::extract(
        Path::new(input),
        format,
        Path::new(destination),
        &mut budget,
        &mut count,
    )?;
    serde_json::to_writer(io::stdout().lock(), &(names, budget, count))?;
    Ok(())
}

fn unpack(
    input: &Path,
    format: &str,
    out: &Path,
    budget: &mut u64,
    count: &mut usize,
) -> io::Result<Vec<String>> {
    let engine = std::env::current_exe()?;
    #[cfg(test)]
    let engine = engine
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("app-depot-server"))
        .filter(|p| p.is_file())
        .unwrap_or(engine);
    let report = tempfile::NamedTempFile::new_in(out.parent().ok_or_else(invalid)?)?;
    let mut command = Command::new(&engine);
    command
        .env_clear()
        .current_dir(out)
        .arg("store-unpack-worker")
        .arg(input)
        .arg(format)
        .arg(out)
        .arg(budget.to_string())
        .arg(count.to_string())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(report.reopen()?));
    crate::extractor_sandbox::configure(
        &mut command,
        &engine,
        input.parent().ok_or_else(invalid)?,
        out,
    )?;
    let mut child = command.spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline || report.as_file().metadata()?.len() > 64 << 20 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    if !status.success() || report.as_file().metadata()?.len() > 64 << 20 {
        return Err(invalid());
    }
    let (names, remaining_bytes, remaining_entries): (Vec<String>, u64, usize) =
        serde_json::from_reader(report.reopen()?)?;
    if remaining_bytes > *budget
        || remaining_entries > *count
        || names.len() > *count - remaining_entries
        || names.iter().any(|n| !path(n))
    {
        return Err(invalid());
    }
    *budget = remaining_bytes;
    *count = remaining_entries;
    Ok(names)
}

fn cached_component(c: &Component, scratch: &Path) -> io::Result<PathBuf> {
    let cache = scratch.join("store-components");
    if !cache.is_dir() {
        fs::create_dir(&cache)?;
    }
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o700))?;
    let dest = cache.join(&c.sha256);
    if dest.is_file() {
        let mut file = File::open(&dest)?;
        let mut hash = Sha256::new();
        if file.metadata()?.len() == c.bytes
            && io::copy(&mut file, &mut hash)? == c.bytes
            && hex::encode(hash.finalize()) == c.sha256
        {
            return Ok(dest);
        }
        fs::remove_file(&dest)?;
    }
    let mut existing = Vec::new();
    let mut size = c.bytes;
    for item in fs::read_dir(&cache)? {
        let item = item?;
        let metadata = item.metadata()?;
        size += metadata.len();
        existing.push((metadata.modified()?, metadata.len(), item.path()));
    }
    existing.sort();
    for (_, bytes, path) in existing {
        if size <= 12 * 1024u64.pow(3) {
            break;
        }
        fs::remove_file(path)?;
        size -= bytes;
    }
    let pending = tempfile::tempdir_in(scratch)?;
    let blob = pending.path().join("download");
    download(c, &blob)?;
    fs::rename(blob, &dest)?;
    Ok(dest)
}

pub fn prepare(recipe: &Recipe, scratch: &Path) -> io::Result<(TempDir, PathBuf)> {
    recipe.validate()?;
    let work = tempfile::Builder::new()
        .prefix("store-")
        .tempdir_in(scratch)?;
    let output = work.path().join("payload");
    fs::create_dir(&output)?;
    let mut entries = HashMap::new();
    let mut budget = 16 * 1024u64.pow(3);
    let mut count = 50000;
    for c in &recipe.components {
        let root = work.path().join(format!("component-{}", c.name));
        fs::create_dir(&root)?;
        let blob = cached_component(c, scratch).map_err(|error| {
            io::Error::other(format!("Acquisition of {} failed: {error}", c.name))
        })?;
        let files = if c.format == "raw" {
            fs::hard_link(blob, root.join(&c.name))?;
            vec![c.name.clone()]
        } else {
            unpack(&blob, &c.format, &root, &mut budget, &mut count).map_err(|error| {
                io::Error::other(format!("Archive validation failed for {}: {error}", c.name))
            })?
        };
        let groups = recipe
            .prepare
            .iter()
            .filter(|step| {
                step.component.as_ref() == Some(&c.name) && step.operation == "installshield-cab"
            })
            .filter_map(|step| step.from_prefix.as_deref()?.strip_suffix('/'))
            .collect::<std::collections::BTreeSet<_>>();
        let contents = if groups.is_empty() {
            (root, files)
        } else {
            crate::store_cab::extract(&root, &files, groups, work.path(), &mut budget, &mut count)?
        };
        entries.insert(&c.name, contents);
    }
    let mut output_budget = 16 * 1024u64.pow(3);
    let mut spellings = HashMap::new();
    let mut copy = |source: &Path, dest: &Path, replace: bool| -> io::Result<()> {
        let key = dest.to_str().ok_or_else(invalid)?.to_lowercase();
        if let Some(old) = spellings.insert(key, dest.to_owned()) {
            if !replace {
                return Err(invalid());
            }
            if old != dest {
                fs::remove_file(old)?;
            }
        }
        if spellings.len() > 50000 {
            return Err(invalid());
        }
        let size = source.metadata()?.len();
        output_budget = output_budget.checked_sub(size).ok_or_else(invalid)?;
        fs::create_dir_all(dest.parent().ok_or_else(invalid)?)?;
        let mut target = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .create_new(!replace)
            .truncate(replace)
            .open(dest)?;
        io::copy(&mut File::open(source)?, &mut target)?;
        let mode = if source.metadata()?.permissions().mode() & 0o111 != 0 {
            0o755
        } else {
            0o644
        };
        target.set_permissions(fs::Permissions::from_mode(mode))?;
        Ok(())
    };
    for step in &recipe.prepare {
        let replace = step.on_conflict == "replace";
        if step.operation == "copy" {
            copy(
                &output.join(step.from.as_ref().unwrap()),
                &output.join(step.to.as_ref().unwrap()),
                replace,
            )?;
            continue;
        }
        if !matches!(
            step.operation.as_str(),
            "" | "extract" | "unreal-uz" | "installshield-cab"
        ) {
            return Err(invalid());
        }
        let (root, files) = &entries[step.component.as_ref().unwrap()];
        for name in files {
            let Some(rel) = name.strip_prefix(step.from_prefix.as_ref().unwrap()) else {
                continue;
            };
            if !step.include.iter().flatten().any(|p| matches(p, rel))
                || step.exclude.iter().flatten().any(|p| matches(p, rel))
            {
                continue;
            }
            let dest = format!("{}{rel}", step.to_prefix.as_ref().unwrap());
            if !path(&dest) {
                return Err(invalid());
            }
            if step.operation == "unreal-uz" {
                let stripped = dest.strip_suffix(".uz").ok_or_else(invalid)?;
                let source = root.join(name);
                if source.metadata()?.len() > 64 << 20 {
                    return Err(invalid());
                }
                let data = crate::store_uz::decode(
                    &fs::read(source)?,
                    stripped.rsplit('/').next().ok_or_else(invalid)?,
                )?;
                let tmp = tempfile::NamedTempFile::new_in(work.path())?;
                fs::write(tmp.path(), data)?;
                copy(tmp.path(), &output.join(stripped), replace)?;
            } else {
                copy(&root.join(name), &output.join(dest), replace)?;
            }
        }
    }
    for required in recipe
        .required
        .iter()
        .chain(std::iter::once(&recipe.launch.path))
    {
        if !output.join(required).is_file() {
            return Err(io::Error::other(format!(
                "Missing required file: {required}"
            )));
        }
    }
    if !output.join(&recipe.launch.cwd).is_dir() {
        return Err(invalid());
    }
    fs::set_permissions(
        output.join(&recipe.launch.path),
        fs::Permissions::from_mode(0o755),
    )?;
    Ok((work, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires STORE_RECIPE and STORE_VERIFIED_MEDIA with individually acquired components"]
    async fn prepare_verified_media() {
        let bytes = fs::read(std::env::var_os("STORE_RECIPE").unwrap()).unwrap();
        let recipe: Recipe = serde_json::from_slice(&bytes).unwrap();
        let media = PathBuf::from(std::env::var_os("STORE_VERIFIED_MEDIA").unwrap());
        let scratch = tempfile::tempdir_in(media.parent().unwrap()).unwrap();
        let cache = scratch.path().join("store-components");
        fs::create_dir(&cache).unwrap();
        for c in &recipe.components {
            fs::hard_link(media.join(&c.sha256), cache.join(&c.sha256)).unwrap();
        }
        let (_work, payload) = prepare(&recipe, scratch.path()).unwrap();
        let mut signature = [0; 4];
        File::open(payload.join(&recipe.launch.path))
            .unwrap()
            .read_exact(&mut signature)
            .unwrap();
        assert_eq!(&signature, b"\x7fELF");
        // No listener is started. Validate private local import and installation
        // resolution without making licensed media available to other users.
        use crate::{
            auth::AuthState,
            import_source::ImportRoot,
            server_import::{import_store_tree, ImportState},
        };
        use app_depot_application::upload::{Target, UploadCoordinator};
        use app_depot_infrastructure::{
            auth, init_local, installation, lock_local, open_catalog, upload::UploadAdapter,
            LocalMode,
        };
        use std::sync::{Arc, Mutex};
        use tokio::sync::{mpsc, Semaphore};
        init_local(scratch.path(), "media", LocalMode::Dev).unwrap();
        let lock = lock_local(scratch.path(), "media", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let token = auth::prepare_setup(&pool).await.unwrap().unwrap();
        let password = uuid::Uuid::new_v4().to_string();
        assert!(auth::complete_setup(&pool, &token, "verifier", &password)
            .await
            .unwrap());
        let principal = auth::verify_password(&pool, "verifier", &password)
            .await
            .unwrap()
            .unwrap();
        let state = ImportState {
            auth: AuthState::from_data_root(pool.clone(), &lock.path).unwrap(),
            coordinator: Arc::new(UploadCoordinator::new(UploadAdapter::new(
                pool.clone(),
                lock.path.clone(),
            ))),
            roots: Arc::new(vec![]),
            inbox_path: lock.path.clone(),
            active: Arc::new(Mutex::new(HashMap::new())),
            permits: Arc::new(Semaphore::new(1)),
            gog_permit: Arc::new(Semaphore::new(1)),
            gog_scratch: Arc::new(scratch.path().to_path_buf()),
        };
        let target = Target::NewApp {
            new_app_name: recipe.title.clone(),
        };
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let result = import_store_tree(
            &state,
            &principal,
            ImportRoot::private_staging(payload).unwrap(),
            target,
            "local-media-check",
            crate::store::association_metadata(&recipe.upstream, &recipe),
            &tx,
        )
        .await
        .unwrap();
        let release = result.release_id.unwrap();
        let plan = installation::resolve(
            &pool,
            Some(&principal),
            &release,
            &installation::Request {
                platform: "linux/x86_64".into(),
                selections: Default::default(),
                revision_id: None,
                manifest: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(plan["launch"]["path"], recipe.launch.path);
        println!(
            "{}: private draft {}, artifact {}, launch {}",
            recipe.title,
            release,
            result.artifact_id.unwrap(),
            recipe.launch.path
        );
    }
}
