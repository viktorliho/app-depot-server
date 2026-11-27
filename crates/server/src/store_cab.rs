// InstallShield groups are selected by the recipe and extracted into private staging.
use crate::extractor_sandbox;
use app_depot_domain::tree::validate_path;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn invalid() -> io::Error {
    io::Error::other("Invalid InstallShield cabinet")
}
fn engine(group: &str, mode: &str, input: &Path, output: &Path, log: &fs::File) -> io::Result<()> {
    let exe = Path::new("/usr/bin/unshield");
    let mut cmd = Command::new(exe);
    cmd.current_dir(output)
        .env_clear()
        .args(["-g", group, "-d"])
        .arg(output)
        .arg(mode)
        .arg(input.join("data1.cab"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::null());
    extractor_sandbox::configure(&mut cmd, exe, input, output)?;
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() && log.metadata()?.len() <= 4 << 20 {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        if Instant::now() >= deadline || log.metadata()?.len() > 4 << 20 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn extract(
    root: &Path,
    files: &[String],
    groups: BTreeSet<&str>,
    work: &Path,
    budget: &mut u64,
    remaining: &mut usize,
) -> io::Result<(std::path::PathBuf, Vec<String>)> {
    let input = tempfile::tempdir_in(work)?;
    for path in files {
        let name = path
            .rsplit('/')
            .next()
            .ok_or_else(invalid)?
            .to_ascii_lowercase();
        if name == "data1.hdr"
            || name == "engine32.cab"
            || name
                .strip_prefix("data")
                .and_then(|n| n.strip_suffix(".cab"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            fs::hard_link(root.join(path), input.path().join(name))?;
        }
    }
    let output = tempfile::tempdir_in(work)?;
    let mut allowed = BTreeMap::new();
    for group in groups {
        if group.is_empty()
            || !group
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid());
        }
        let log = tempfile::NamedTempFile::new_in(work)?;
        engine(group, "l", input.path(), output.path(), log.as_file())?;
        let listing = fs::read_to_string(log.path())?;
        let before = allowed.len();
        for line in listing.lines() {
            let Some((size, name)) = line.trim().split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(size) = size.parse::<u64>() else {
                continue;
            };
            let name = name.trim().replace('\\', "/");
            if name == "files" {
                continue;
            }
            if name.split('/').next() != Some(group)
                || validate_path(&name).is_err()
                || name.len() > 1024
                || name.split('/').count() > 32
            {
                return Err(invalid());
            }
            *budget = budget.checked_sub(size).ok_or_else(invalid)?;
            *remaining = remaining.checked_sub(1).ok_or_else(invalid)?;
            if allowed.insert(name, size).is_some() {
                return Err(invalid());
            }
        }
        if allowed.len() == before {
            return Err(invalid());
        }
        engine(group, "x", input.path(), output.path(), log.as_file())?;
    }
    let mut dirs = vec![output.path().to_path_buf()];
    let mut names = Vec::new();
    let mut inspected = 0;
    while let Some(dir) = dirs.pop() {
        for item in fs::read_dir(dir)? {
            inspected += 1;
            if inspected > 100000 {
                return Err(invalid());
            }
            let item = item?;
            let path = item.path();
            let name = path
                .strip_prefix(output.path())
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(invalid)?;
            if validate_path(name).is_err() || name.len() > 1024 || name.split('/').count() > 32 {
                return Err(invalid());
            }
            if item.file_type()?.is_dir() {
                dirs.push(path);
                continue;
            }
            if !item.file_type()?.is_file() || allowed.remove(name) != Some(item.metadata()?.len())
            {
                return Err(invalid());
            }
            names.push(name.to_owned());
        }
    }
    if !allowed.is_empty() {
        return Err(invalid());
    }
    Ok((output.keep(), names))
}
