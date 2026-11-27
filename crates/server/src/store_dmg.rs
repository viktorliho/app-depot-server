//! Import a selected HFS+ subtree from a DMG. 7-Zip only writes stdout. The
//! parent owns every output path and checks the entry type before creating it.
use crate::{extractor_sandbox, store_prepare::path};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn invalid() -> io::Error {
    io::Error::other("Invalid or unsupported disk image")
}
struct Reader<'a> {
    file: &'a Path,
    kind: &'a str,
    sandbox: &'a Path,
    deadline: Instant,
}
impl Reader<'_> {
    fn run(&self, name: Option<&str>, output: &File, limit: u64) -> io::Result<()> {
        let engine = if Path::new("/usr/bin/7zz").is_file() {
            Path::new("/usr/bin/7zz")
        } else {
            Path::new("/usr/lib/7zip/7z")
        };
        let mut cmd = Command::new(engine);
        cmd.env_clear()
            .env("LANG", "C.UTF-8")
            .current_dir(self.sandbox)
            .args(if name.is_some() {
                ["e", "-so"]
            } else {
                ["l", "-slt"]
            })
            .args(["-ba", "-bd", "-mmt1", "-sns-", "-spd", "-sccUTF-8"])
            .arg(self.kind)
            .arg("--")
            .arg(self.file);
        if let Some(name) = name {
            cmd.arg(name);
        }
        cmd.stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::from(output.try_clone()?));
        extractor_sandbox::configure(
            &mut cmd,
            engine,
            self.file.parent().ok_or_else(invalid)?,
            self.sandbox,
        )?;
        unsafe {
            cmd.pre_exec(move || {
                let lim = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() && output.metadata()?.len() <= limit {
                    Ok(())
                } else {
                    Err(invalid())
                };
            }
            if Instant::now() >= self.deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("Disk image extraction timed out"));
            }
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    fn list(&self) -> io::Result<Vec<HashMap<String, String>>> {
        let listing = tempfile::NamedTempFile::new_in(self.sandbox.parent().ok_or_else(invalid)?)?;
        self.run(None, listing.as_file(), 16 << 20)?;
        let mut text = String::new();
        listing.reopen()?.read_to_string(&mut text)?;
        text.trim_end_matches('\n')
            .split("\n\n")
            .map(|section| {
                let mut entry = HashMap::new();
                for line in section.lines() {
                    let (k, v) = line.split_once(" = ").ok_or_else(invalid)?;
                    if entry.insert(k.to_owned(), v.to_owned()).is_some() {
                        return Err(invalid());
                    }
                }
                Ok(entry)
            })
            .collect()
    }
}
pub fn extract(
    input: &Path,
    root: &str,
    out: &Path,
    budget: &mut u64,
    count: &mut usize,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<Vec<String>> {
    if !path(root) {
        return Err(invalid());
    }
    let parent = out.parent().ok_or_else(invalid)?;
    let sandbox = tempfile::tempdir_in(parent)?;
    let reader = Reader {
        file: input,
        kind: "-tdmg",
        sandbox: sandbox.path(),
        deadline: Instant::now() + Duration::from_secs(600),
    };
    let partitions = reader.list()?;
    let mut hfs = partitions
        .iter()
        .filter(|e| e.get("Path").is_some_and(|n| n.ends_with(".hfs")));
    let part = hfs.next().ok_or_else(invalid)?;
    let name = &part["Path"];
    if hfs.next().is_some() || !path(name) || name.contains('/') {
        return Err(invalid());
    }
    let size = part
        .get("Size")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(invalid)?;
    if size > *budget || size > 4 << 30 {
        return Err(invalid());
    }
    *budget -= size;
    let disk = tempfile::NamedTempFile::new_in(parent)?;
    reader.run(Some(name), disk.as_file(), size)?;
    if disk.as_file().metadata()?.len() != size {
        return Err(invalid());
    }
    let reader = Reader {
        file: disk.path(),
        kind: "-thfs",
        ..reader
    };
    let files = selected_files(reader.list()?, root, out, budget, count)?;
    let total = files.len() as u64;
    for (i, (name, size, executable)) in files.iter().enumerate() {
        let dest = out.join(name);
        fs::create_dir_all(dest.parent().ok_or_else(invalid)?)?;
        let file = File::create_new(dest)?;
        reader.run(Some(name), &file, *size)?;
        if file.metadata()?.len() != *size {
            return Err(invalid());
        }
        file.set_permissions(fs::Permissions::from_mode(if *executable {
            0o755
        } else {
            0o644
        }))?;
        progress(i as u64 + 1, total);
    }
    Ok(files.into_iter().map(|(name, _, _)| name).collect())
}

fn selected_files(
    entries: Vec<HashMap<String, String>>,
    root: &str,
    out: &Path,
    budget: &mut u64,
    count: &mut usize,
) -> io::Result<Vec<(String, u64, bool)>> {
    let prefix = format!("{root}/");
    let mut names = HashSet::new();
    let mut files = Vec::new();
    for e in entries {
        let entry = e.get("Path").ok_or_else(invalid)?;
        if !entry.starts_with(&prefix) && entry != root {
            continue;
        }
        if !path(entry)
            || !names.insert(entry.to_owned())
            || e.get("Alternate Stream").map(String::as_str) != Some("-")
        {
            return Err(invalid());
        }
        *count = count.checked_sub(1).ok_or_else(invalid)?;
        let mode = e.get("Mode").ok_or_else(invalid)?;
        let dest = out.join(entry);
        if mode.starts_with('d') {
            fs::create_dir_all(dest)?;
            continue;
        }
        if !mode.starts_with('-') {
            return Err(io::Error::other(
                "Selected subtree contains a link or special entry unsupported by Tree Identity v1",
            ));
        }
        let size = e
            .get("Size")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        *budget = budget.checked_sub(size).ok_or_else(invalid)?;
        files.push((entry.clone(), size, mode.contains('x')));
    }
    if files.is_empty() {
        return Err(invalid());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, mode: &str) -> HashMap<String, String> {
        [
            ("Path", path),
            ("Mode", mode),
            ("Alternate Stream", "-"),
            ("Size", "4"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    #[test]
    fn bundle_selection_excludes_the_volume_shortcut_and_keeps_executable_mode() {
        let out = tempfile::tempdir().unwrap();
        let files = selected_files(
            vec![
                entry("Volume/Applications", "lrwxrwxrwx"),
                entry("Volume/Game.app/Contents/MacOS/Game", "-rwxr-xr-x"),
            ],
            "Volume/Game.app",
            out.path(),
            &mut 4,
            &mut 1,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![("Volume/Game.app/Contents/MacOS/Game".into(), 4, true)]
        );
        assert!(!out.path().join("Volume/Applications").exists());
    }

    #[test]
    fn selected_bundle_rejects_internal_links_special_entries_and_traversal() {
        let out = tempfile::tempdir().unwrap();
        for (path, mode) in [
            ("Volume/Game.app/alias", "lrwxrwxrwx"),
            ("Volume/Game.app/socket", "srwxrwxrwx"),
            ("Volume/Game.app/../outside", "-rw-r--r--"),
        ] {
            assert!(selected_files(
                vec![
                    entry("Volume/Game.app/Game", "-rwxr-xr-x"),
                    entry(path, mode)
                ],
                "Volume/Game.app",
                out.path(),
                &mut 10,
                &mut 3
            )
            .is_err());
        }
    }
}
