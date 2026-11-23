use crate::{
    discovery,
    import_source::{ImportRoot, Scan},
};
use app_depot_domain::tree::validate_path;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const GIB: u64 = 1024 * 1024 * 1024;
const INPUT_LIMIT: u64 = 32 * GIB;
const OUTPUT_LIMIT: u64 = 48 * GIB;
const FILE_LIMIT: usize = 100_000;
const LOG_LIMIT: usize = 24 * 1024 * 1024;
const ENGINE: &str = "/usr/bin/innoextract";

#[derive(Debug)]
pub enum Error {
    Source,
    Launch,
    Changed,
    MissingVolume,
    InvalidVolume,
    Format,
    Limit,
    Environment,
    Cancelled,
}

impl Error {
    pub fn problem(&self) -> (&'static str, &'static str) {
        match self {
            Self::Source => (
                "invalid_gog_source",
                "Select a directory and a regular setup EXE.",
            ),
            Self::Launch => (
                "invalid_gog_launch",
                "Select a listed Windows EXE, an existing working directory and valid literal arguments.",
            ),
            Self::Changed => (
                "source_changed",
                "Installer files changed. Analyze the directory again.",
            ),
            Self::MissingVolume => (
                "gog_missing_volume",
                "A required installer volume is missing. Check the EXE and BIN files.",
            ),
            Self::InvalidVolume => (
                "gog_invalid_volume",
                "Installer data is corrupt or its external archive format is unsupported.",
            ),
            Self::Format => (
                "gog_unsupported_format",
                "This is not a supported Inno Setup installer or its setup metadata is malformed.",
            ),
            Self::Limit => (
                "gog_limit",
                "Installer processing exceeded a resource or storage limit.",
            ),
            Self::Environment => (
                "gog_unavailable",
                "The innoextract 1.9 backend or its required Linux sandbox is unavailable.",
            ),
            Self::Cancelled => ("session_cancelled", "Installer processing cancelled."),
        }
    }
}

pub fn cleanup(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join("gog-work");
    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new().recursive(false).create(&dir)?;
    Ok(dir)
}

fn safe_path(path: &str) -> bool {
    path.len() <= 1024
        && path.split('/').count() <= 64
        && !path.contains(':')
        && !path.chars().any(char::is_control)
        && validate_path(path).is_ok()
}

pub fn candidates(root: &ImportRoot, path: &str) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    for name in root.flat_names(path).map_err(|_| Error::Source)? {
        if !name.to_ascii_lowercase().ends_with(".exe") || !safe_path(&name) {
            continue;
        }
        if root.open_file(&source_path(path, &name)).is_ok() {
            result.push(name);
        }
        if result.len() > 64 {
            return Err(Error::Limit);
        }
    }
    Ok(result)
}

fn source_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.into()
    } else {
        format!("{dir}/{name}")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    container: &'static str,
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub setup: String,
    product_name: String,
    installer_version: String,
    installer_format: &'static str,
    gog_game_id: Option<String>,
    engine: &'static str,
    engine_version: &'static str,
    volumes: Vec<Volume>,
    installer_languages: BTreeSet<String>,
    language_tagged_files: BTreeMap<String, usize>,
    redistributable_roots: BTreeSet<String>,
    candidate_executables: BTreeSet<String>,
    optional_components: Option<()>,
    missing_volumes: Vec<String>,
    listed_file_count: usize,
    approximate_output_bytes: Option<u64>,
    unknown_actions: &'static [&'static str],
}

const UNKNOWN: &[&str] = &[
    "Setup scripts and conditional or post-install actions",
    "Registry, configuration, shortcuts and working-directory changes",
    "Prerequisite requirements, ordering and command-line arguments",
    "Complete game language variants and optional component relationships",
];

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hints {
    #[serde(default)]
    product_name: String,
    #[serde(default)]
    game_version: String,
    #[serde(default)]
    candidate_executable: String,
    #[serde(default)]
    notes: String,
}

impl Hints {
    pub fn valid(&self) -> bool {
        [
            (&self.product_name, 255),
            (&self.game_version, 255),
            (&self.candidate_executable, 1024),
            (&self.notes, 4096),
        ]
        .into_iter()
        .all(|(value, limit)| value.len() <= limit && !value.chars().any(char::is_control))
            && (self.candidate_executable.is_empty() || safe_path(&self.candidate_executable))
    }
}

pub struct Work {
    _guard: tempfile::TempDir,
    input: PathBuf,
    output: PathBuf,
}

fn cancellation(flag: &AtomicBool) -> Result<(), Error> {
    if flag.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

type Stamp = (u64, u64, u64, i64, i64, i64, i64);

fn stamp(file: &File) -> io::Result<Stamp> {
    let m = file.metadata()?;
    Ok((
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}

fn same_stamps(root: &ImportRoot, files: &[(String, File, u64)], initial: &[Stamp]) -> bool {
    files
        .iter()
        .zip(initial)
        .all(|((name, file, _), expected)| {
            stamp(file).is_ok_and(|current| &current == expected)
                && root
                    .open_file(name)
                    .and_then(|current| stamp(&current))
                    .is_ok_and(|current| &current == expected)
        })
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallationProposal {
    #[serde(flatten)]
    discovery: discovery::Report,
    source_observations: SourceObservations,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceObservations {
    reported_application_languages: Vec<String>,
    setup_actions: Vec<SetupObservation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupObservation {
    action: String,
    target: Option<String>,
    original_argument_text: Option<String>,
    requires_elevation: bool,
    conditions: Vec<String>,
}

impl Work {
    pub fn prepare(
        scratch: &Path,
        root: &ImportRoot,
        path: &str,
        setup: &str,
        flag: &AtomicBool,
    ) -> Result<(Self, Vec<Volume>), Error> {
        let work = tempfile::Builder::new()
            .prefix("installer-")
            .tempdir_in(scratch)
            .map_err(|_| Error::Environment)?;
        let input = work.path().join("input");
        let output = work.path().join("output");
        fs::create_dir(&input)
            .and_then(|()| fs::create_dir(&output))
            .map_err(|_| Error::Environment)?;
        let stem = setup
            .get(..setup.len().saturating_sub(4))
            .ok_or(Error::Source)?;
        if !setup.to_ascii_lowercase().ends_with(".exe") || !safe_path(setup) || setup.contains('/')
        {
            return Err(Error::Source);
        }
        let names = root.flat_names(path).map_err(|_| Error::Source)?;
        let companion = |name: &str| {
            name.strip_prefix(stem)
                .and_then(|suffix| suffix.strip_suffix(".bin"))
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('-'))
        };
        let names: Vec<_> = std::iter::once(setup.to_owned())
            .chain(names.into_iter().filter(|name| companion(name)))
            .collect();
        if names.len() > 128 {
            return Err(Error::Limit);
        }
        let mut files = Vec::new();
        let mut initial = Vec::new();
        let mut total = 0u64;
        for name in &names {
            if !safe_path(name) {
                return Err(Error::Source);
            }
            let logical = source_path(path, name);
            let file = root.open_file(&logical).map_err(|_| Error::Source)?;
            let expected = stamp(&file).map_err(|_| Error::Source)?;
            total = total
                .checked_add(expected.2)
                .filter(|size| *size <= INPUT_LIMIT)
                .ok_or(Error::Limit)?;
            initial.push(expected);
            files.push((logical, file, expected.2));
        }
        if fs2::available_space(scratch).map_err(|_| Error::Environment)? < total + GIB {
            return Err(Error::Limit);
        }
        let mut volumes = Vec::new();
        let mut buffer = vec![0; 1024 * 1024];
        for ((_, file, size), name) in files.iter_mut().zip(&names) {
            let mut dest = File::create(input.join(name)).map_err(|_| Error::Environment)?;
            let mut hash = Sha256::new();
            let mut remaining = *size;
            let mut container = "Unknown";
            let mut first = true;
            while remaining > 0 {
                cancellation(flag)?;
                let count = buffer.len().min(remaining as usize);
                let n = file
                    .read(&mut buffer[..count])
                    .map_err(|_| Error::Changed)?;
                if n == 0 {
                    return Err(Error::Changed);
                }
                if first {
                    container = match &buffer[..n] {
                        bytes if bytes.starts_with(b"MZ") => "PortableExecutable",
                        bytes
                            if bytes.starts_with(b"idska32\x1a")
                                || bytes.starts_with(b"idskb32\x1a") =>
                        {
                            "InnoDiskSlice"
                        }
                        bytes if bytes.starts_with(b"Rar!\x1a") => {
                            return Err(Error::InvalidVolume)
                        }
                        _ => "Unknown",
                    };
                    first = false;
                }
                dest.write_all(&buffer[..n]).map_err(|_| Error::Limit)?;
                hash.update(&buffer[..n]);
                remaining -= n as u64;
            }
            volumes.push(Volume {
                container,
                name: name.clone(),
                size: *size,
                sha256: hex::encode(hash.finalize()),
            });
        }
        if !same_stamps(root, &files, &initial) {
            return Err(Error::Changed);
        }
        let current = root.flat_names(path).map_err(|_| Error::Changed)?;
        if !current
            .into_iter()
            .filter(|name| companion(name))
            .eq(names.iter().skip(1).cloned())
        {
            return Err(Error::Changed);
        }
        Ok((
            Self {
                _guard: work,
                input,
                output,
            },
            volumes,
        ))
    }

    fn run(
        &self,
        args: &[&str],
        flag: &AtomicBool,
        extracting: bool,
    ) -> Result<(String, String, bool), Error> {
        cancellation(flag)?;
        let mut cmd = Command::new(ENGINE);
        cmd.env_clear()
            .env("LC_ALL", "C.UTF-8")
            .current_dir(&self.output)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args([
                "--color=0",
                "--progress=0",
                "--no-extract-unknown",
                "--collisions=rename",
                "--timestamps=none",
            ])
            .args(args);
        crate::extractor_sandbox::configure(&mut cmd, Path::new(ENGINE), &self.input, &self.output)
            .map_err(|_| Error::Environment)?;
        let mut child = cmd.spawn().map_err(|_| Error::Environment)?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        thread::scope(|scope| {
            let out = scope.spawn(move || drain(stdout));
            let err = scope.spawn(move || drain(stderr));
            let begin = Instant::now();
            let mut next_check = begin;
            let status = loop {
                let interrupted = cancellation(flag).err().or_else(|| {
                    (begin.elapsed() > Duration::from_secs(if extracting { 1200 } else { 900 }))
                        .then_some(Error::Limit)
                });
                if let Some(error) = interrupted {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => {}
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(Error::Environment);
                    }
                }
                if extracting && Instant::now() >= next_check {
                    if let Err(error) = output_limits(&self.output) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                    next_check = Instant::now() + Duration::from_secs(1);
                }
                thread::sleep(Duration::from_millis(100));
            };
            Ok((
                out.join().map_err(|_| Error::Environment)??,
                err.join().map_err(|_| Error::Environment)??,
                status.success(),
            ))
        })
    }

    pub fn inspect(
        &self,
        setup: String,
        volumes: Vec<Volume>,
        flag: &AtomicBool,
        verify: bool,
    ) -> Result<(Analysis, BTreeSet<String>), Error> {
        let (version, _, success) = self.run(&["--version"], flag, false)?;
        if !success || version.lines().next() != Some("innoextract 1.9") {
            return Err(Error::Environment);
        }
        let setup_path = self.input.join(&setup);
        let path = setup_path.to_str().ok_or(Error::Source)?;
        let (info, _, success) = self.run(
            &["--info", "--list-languages", "--gog-game-id", "--", path],
            flag,
            false,
        )?;
        let (product, version) = info
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Inspecting \""))
            .and_then(|line| line.rsplit_once("\" - setup data version "))
            .filter(|_| success)
            .ok_or(Error::Format)?;
        if !info.lines().any(|line| line == "Setup is not passworded!") {
            return Err(Error::Format);
        }
        let (list, errors, success) =
            self.run(&["--list", "--list-sizes", "--", path], flag, false)?;
        check_data(&errors, success)?;
        let listing = parse_listing(&list)?;
        if verify {
            let (_, errors, success) =
                self.run(&["--test", "--silent", "--", path], flag, false)?;
            check_data(&errors, success)?;
        }
        let languages = info
            .lines()
            .skip_while(|line| *line != "Languages:")
            .skip(1)
            .take_while(|line| line.starts_with(" - "))
            .map(|line| line[3..].to_owned())
            .collect();
        let game_id = info
            .lines()
            .find_map(|line| line.strip_prefix("GOG.com game ID is "))
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
        let manifest = Analysis {
            setup,
            product_name: product.into(),
            installer_version: version.into(),
            installer_format: "InnoSetup",
            gog_game_id: game_id.map(str::to_owned),
            engine: "innoextract",
            engine_version: "1.9",
            volumes,
            installer_languages: languages,
            language_tagged_files: listing.languages,
            redistributable_roots: redistributables(&listing.paths),
            candidate_executables: executables(&listing.paths),
            optional_components: None,
            missing_volumes: Vec::new(),
            listed_file_count: listing.paths.len(),
            approximate_output_bytes: listing.bytes,
            unknown_actions: UNKNOWN,
        };
        Ok((manifest, listing.paths))
    }

    fn sidecar(report: &Analysis, root: &ImportRoot, extension: &str) -> Option<Value> {
        let id = report.gog_game_id.as_deref()?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let file = root.open_file(&format!("goggame-{id}.{extension}")).ok()?;
        if file.metadata().ok()?.len() > 65_536 {
            return None;
        }
        serde_json::from_reader(file.take(65_537)).ok()
    }

    fn primary_launch(report: &Analysis, metadata: Option<&Value>) -> BTreeSet<String> {
        let primary = metadata.and_then(|metadata| {
            let mut tasks = metadata
                .get("playTasks")?
                .as_array()?
                .iter()
                .filter_map(|task| {
                    if task["isPrimary"].as_bool() != Some(true)
                        || task["category"] != "game"
                        || task["type"] != "FileTask"
                    {
                        return None;
                    }
                    let path = task["path"].as_str()?.replace('\\', "/");
                    report.candidate_executables.contains(&path).then_some(path)
                });
            let one = tasks.next()?;
            tasks.next().is_none().then_some(one)
        });
        primary.map_or_else(
            || report.candidate_executables.clone(),
            |path| BTreeSet::from([path]),
        )
    }

    fn proposal(
        report: &Analysis,
        root: &ImportRoot,
        paths: &BTreeSet<&str>,
        flag: &AtomicBool,
    ) -> InstallationProposal {
        let info = Self::sidecar(report, root, "info");
        let script = Self::sidecar(report, root, "script");
        let reported_application_languages = info
            .as_ref()
            .and_then(|info| info["languages"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|tag| tag.len() <= 80 && app_depot_domain::installation::language_id(tag))
            .take(32)
            .map(str::to_owned)
            .collect();
        fn bounded(value: &Value) -> Option<String> {
            value
                .as_str()
                .filter(|text| text.len() <= 1024 && !text.chars().any(char::is_control))
                .map(str::to_owned)
        }
        let setup_actions = script
            .as_ref()
            .and_then(|script| script["actions"].as_array())
            .into_iter()
            .flatten()
            .take(64)
            .filter_map(|item| {
                let install = item.get("install")?;
                let action = bounded(&install["action"])?;
                let args = &install["arguments"];
                let target = bounded(&args["executable"]).and_then(|path| {
                    let relative = path.strip_prefix("{app}/")?.replace('\\', "/");
                    paths.contains(relative.as_str()).then_some(relative)
                });
                Some(SetupObservation {
                    action,
                    original_argument_text: target
                        .as_ref()
                        .and_then(|_| bounded(&args["argument"])),
                    target,
                    requires_elevation: args["runAsAdmin"].as_bool() == Some(true),
                    conditions: args["conditions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(16)
                        .filter_map(bounded)
                        .collect(),
                })
            })
            .collect();
        InstallationProposal {
            discovery: discovery::inspect(
                paths,
                &Self::primary_launch(report, info.as_ref()),
                |path| {
                    root.open_file(path)
                        .map(|file| Box::new(file) as Box<dyn Read>)
                },
                flag,
                None,
            ),
            source_observations: SourceObservations {
                reported_application_languages,
                setup_actions,
            },
        }
    }

    pub(crate) fn discovery_preview(
        &self,
        report: &Analysis,
        listed: &BTreeSet<String>,
        flag: &AtomicBool,
    ) -> Result<InstallationProposal, Error> {
        let paths = listed.iter().map(String::as_str).collect();
        let mut selected = discovery::probes(&paths, &report.candidate_executables);
        if let Some(id) = report.gog_game_id.as_deref() {
            for extension in ["info", "script"] {
                let sidecar = format!("goggame-{id}.{extension}");
                if paths.contains(sidecar.as_str()) {
                    selected.push(sidecar);
                }
            }
        }
        if selected.is_empty() {
            return Ok(InstallationProposal::default());
        }
        self.extract_preview(&report.setup, &selected, flag)?;
        let root =
            ImportRoot::private_staging(self.output.clone()).map_err(|_| Error::Environment)?;
        let info = Self::sidecar(report, &root, "info");
        let primary = Self::primary_launch(report, info.as_ref());
        if primary.len() == 1 && primary.first().is_some_and(|path| !selected.contains(path)) {
            self.extract_preview(
                &report.setup,
                &primary.into_iter().collect::<Vec<_>>(),
                flag,
            )?;
        }
        Ok(Self::proposal(report, &root, &paths, flag))
    }

    fn extract_preview(
        &self,
        setup: &str,
        selected: &[String],
        flag: &AtomicBool,
    ) -> Result<(), Error> {
        let filters: Vec<_> = selected.iter().map(|path| format!("/{path}")).collect();
        let setup = self.input.join(setup);
        let mut args = vec!["--extract", "--test", "--silent", "--no-warn-unused"];
        for filter in &filters {
            args.extend(["--include", filter]);
        }
        args.extend(["--", setup.to_str().ok_or(Error::Source)?]);
        let (_, errors, success) = self.run(&args, flag, true)?;
        check_data(&errors, success)
    }

    pub(crate) fn discovery_import(
        report: &Analysis,
        root: &ImportRoot,
        scan: &Scan,
        flag: &AtomicBool,
    ) -> InstallationProposal {
        let paths = scan
            .files
            .iter()
            .map(|file| file.logical_path.as_str())
            .collect();
        Self::proposal(report, root, &paths, flag)
    }

    pub fn extract(
        &self,
        report: &Analysis,
        listed: &BTreeSet<String>,
        flag: &AtomicBool,
    ) -> Result<(ImportRoot, Scan), Error> {
        if let Some(bytes) = report.approximate_output_bytes {
            let needed = bytes.saturating_mul(3).saturating_add(GIB);
            if fs2::available_space(&self.output).map_err(|_| Error::Environment)? < needed {
                return Err(Error::Limit);
            }
        }
        let setup = self.input.join(&report.setup);
        let (_, errors, success) = self.run(
            &[
                "--extract",
                "--test",
                "--quiet",
                "--",
                setup.to_str().ok_or(Error::Source)?,
            ],
            flag,
            true,
        )?;
        check_data(&errors, success)?;
        output_limits(&self.output)?;
        let root =
            ImportRoot::private_staging(self.output.clone()).map_err(|_| Error::Environment)?;
        let scan = root
            .scan_with_cancel("", "BareTree", flag)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::Interrupted {
                    Error::Cancelled
                } else {
                    Error::InvalidVolume
                }
            })?;
        let actual: BTreeSet<_> = scan
            .files
            .iter()
            .map(|file| file.logical_path.as_str())
            .collect();
        if actual.len() != listed.len() || !listed.iter().all(|path| actual.contains(path.as_str()))
        {
            return Err(Error::InvalidVolume);
        }
        Ok((root, scan))
    }
}

fn drain(mut reader: impl Read) -> Result<String, Error> {
    let mut retained = Vec::new();
    let mut buf = [0; 8192];
    let mut overflow = false;
    loop {
        let count = reader.read(&mut buf).map_err(|_| Error::Environment)?;
        if count == 0 {
            break;
        }
        if retained.len() + count <= LOG_LIMIT {
            retained.extend_from_slice(&buf[..count]);
        } else {
            overflow = true;
        }
    }
    if overflow {
        return Err(Error::Limit);
    }
    String::from_utf8(retained).map_err(|_| Error::Format)
}

fn check_data(errors: &str, success: bool) -> Result<(), Error> {
    let lower = errors.to_ascii_lowercase();
    if lower.contains("slice")
        && (lower.contains("could not open")
            || lower.contains("not found")
            || lower.contains("no such file"))
    {
        return Err(Error::MissingVolume);
    }
    if !success
        || lower.contains("checksum")
        || (lower.contains("unused bin") || lower.contains("unused .bin"))
        || lower.contains("could not")
    {
        return Err(Error::InvalidVolume);
    }
    Ok(())
}

struct Listing {
    paths: BTreeSet<String>,
    languages: BTreeMap<String, usize>,
    bytes: Option<u64>,
}

fn parse_listing(text: &str) -> Result<Listing, Error> {
    let mut listing = Listing {
        paths: BTreeSet::new(),
        languages: BTreeMap::new(),
        bytes: Some(0),
    };
    for line in text.lines().filter(|line| line.starts_with(" - ")) {
        let line = line.strip_prefix(" - \"").ok_or(Error::Format)?;
        let parts: Vec<_> = line.split('"').collect();
        if parts.len() % 2 != 0 {
            return Err(Error::Format);
        }
        let size = estimate_size(parts.last().ok_or(Error::Format)?);
        for pair in parts.chunks_exact(2) {
            let [path, annotations] = pair else {
                unreachable!()
            };
            if !safe_path(path) || !listing.paths.insert((*path).to_owned()) {
                return Err(Error::Format);
            }
            if listing.paths.len() > FILE_LIMIT {
                return Err(Error::Limit);
            }
            for tag in annotations
                .split('[')
                .skip(1)
                .filter_map(|entry| entry.split_once(']').map(|(tag, _)| tag))
            {
                for token in tag.split([',', ' ']).filter(|token| {
                    token.split_once('-').is_some_and(|(a, b)| {
                        a.len() == 2
                            && b.len() == 2
                            && token
                                .bytes()
                                .all(|ch| ch == b'-' || ch.is_ascii_alphabetic())
                    })
                }) {
                    *listing.languages.entry(token.into()).or_default() += 1;
                }
            }
            listing.bytes = listing.bytes.zip(size).and_then(|(a, b)| a.checked_add(b));
        }
    }
    if listing.paths.is_empty() {
        return Err(Error::Format);
    }
    if listing.bytes.is_some_and(|bytes| bytes > OUTPUT_LIMIT) {
        return Err(Error::Limit);
    }
    Ok(listing)
}

fn estimate_size(annotations: &str) -> Option<u64> {
    let (_, end) = annotations.rsplit_once('(')?;
    let (amount, unit) = end.strip_suffix(')')?.split_once(' ')?;
    let scale = match unit {
        "B" => 1,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        "GiB" => GIB,
        _ => return None,
    };
    let value = amount.parse::<f64>().ok()? * scale as f64;
    (value.is_finite() && value >= 0.0).then_some(value.ceil() as u64)
}

fn output_limits(root: &Path) -> Result<(), Error> {
    let mut dirs = vec![(root.to_owned(), 0)];
    let mut count = 0;
    let mut bytes = 0u64;
    while let Some((dir, depth)) = dirs.pop() {
        if depth > 64 {
            return Err(Error::Limit);
        }
        for entry in fs::read_dir(dir).map_err(|_| Error::InvalidVolume)? {
            let entry = entry.map_err(|_| Error::InvalidVolume)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .ok_or(Error::InvalidVolume)?;
            if !safe_path(relative) {
                return Err(Error::InvalidVolume);
            }
            count += 1;
            if count > FILE_LIMIT {
                return Err(Error::Limit);
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::InvalidVolume),
            };
            if metadata.is_dir() {
                dirs.push((path, depth + 1));
            } else if metadata.is_file() && metadata.nlink() == 1 {
                bytes = bytes
                    .checked_add(metadata.len())
                    .filter(|size| *size <= OUTPUT_LIMIT)
                    .ok_or(Error::Limit)?;
            } else {
                return Err(Error::InvalidVolume);
            }
        }
    }
    Ok(())
}

fn redistributables<T: AsRef<str> + Ord>(paths: &BTreeSet<T>) -> BTreeSet<String> {
    paths
        .iter()
        .filter_map(|path| {
            let mut parts = path.as_ref().split('/');
            let root = parts.next()?;
            let component = parts.next()?;
            matches!(
                root.to_ascii_lowercase().as_str(),
                "__redist" | "redist" | "_commonredist"
            )
            .then(|| format!("{root}/{component}"))
        })
        .collect()
}

fn executables<T: AsRef<str> + Ord>(paths: &BTreeSet<T>) -> BTreeSet<String> {
    paths
        .iter()
        .map(AsRef::as_ref)
        .filter(|path| {
            path.to_ascii_lowercase().ends_with(".exe")
                && !path.split('/').any(|part| {
                    matches!(
                        part.to_ascii_lowercase().as_str(),
                        "__redist" | "redist" | "_commonredist"
                    )
                })
        })
        .take(128)
        .map(str::to_owned)
        .collect()
}

pub fn analysis_id(report: &Analysis) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(report).expect("analysis is serializable"),
    ))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryPoint {
    platform: String,
    working_directory: Option<String>,
    #[serde(default)]
    arguments: Vec<String>,
}

pub fn declaration(
    report: Analysis,
    hints: &Hints,
    scan: &Scan,
    launch: Option<&EntryPoint>,
) -> Result<Value, Error> {
    let paths: BTreeSet<_> = scan
        .files
        .iter()
        .map(|file| file.logical_path.as_str())
        .collect();
    let candidates = executables(&paths);
    let target = &hints.candidate_executable;
    if !target.is_empty() && !paths.contains(target.as_str()) {
        return Err(Error::Launch);
    }
    let reviewed_candidate = report.candidate_executables.contains(target);
    let mut data = json!({"appDepot":{"label":"Recovered offline installer payload","gogOfflineInstaller":{
        "version":1,
        "analysis":report,
        "detected":{
            "platformFamily":"windows",
            "evidence":"InnoSetup and file names",
            "redistributableRoots":redistributables(&paths),
            "candidateExecutables":candidates,
        },
        "user":hints,
        "payload":{"kind":"Tree","root":"","fileCount":scan.files.len(),"size":scan.total_bytes},
        "installReadiness":"Unknown"
    }}});
    if let Some(launch) = launch {
        if !reviewed_candidate
            || !candidates.contains(target)
            || !matches!(
                launch.platform.as_str(),
                "windows/x86_64" | "windows/aarch64"
            )
            || launch.working_directory.as_ref().is_some_and(|path| {
                !path.is_empty() && (!safe_path(path) || !scan.directories.contains(path))
            })
            || launch.arguments.len() > 64
            || launch.arguments.iter().map(String::len).sum::<usize>() > 16384
            || launch
                .arguments
                .iter()
                .any(|arg| arg.len() > 4096 || arg.chars().any(char::is_control))
        {
            return Err(Error::Launch);
        }
        let mut method = json!({"method":"NativePortableV1", "arguments":launch.arguments});
        if let Some(directory) = &launch.working_directory {
            method["workingDirectory"] = json!(directory);
        }
        let mut entry = json!({
            "id":"gog-launch", "target":{"type":"TreeFile","path":target},
            "platforms":[launch.platform], "launch":method
        });
        if target.chars().count() <= 160 {
            entry["label"] = json!(target);
        }
        data["appDepot"]["entryPoints"] = json!([entry]);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_keeps_aliases_languages_and_redistributable_paths() {
        let report = parse_listing(
            " - \"game.exe\" (20 B)\n - \"data.txt\" [en-US], \"data#ru-RU.txt\" [ru-RU] (10 B)\n - \"__redist/example/setup.exe\" (30 B)",
        ).unwrap();
        assert_eq!(report.paths.len(), 4);
        assert_eq!(report.bytes, Some(70));
        assert_eq!(
            report.languages,
            BTreeMap::from([("en-US".into(), 1), ("ru-RU".into(), 1)])
        );
        assert_eq!(
            redistributables(&report.paths),
            BTreeSet::from(["__redist/example".into()])
        );
        assert_eq!(
            executables(&report.paths),
            BTreeSet::from(["game.exe".into()])
        );
        for name in ["../outside", "C:/root", "/absolute", "path\\escape"] {
            assert!(parse_listing(&format!(" - \"{name}\" (8 B)")).is_err());
        }
        assert!(parse_listing(" - \"same\" (1 B)\n - \"same\" (2 B)").is_err());
    }

    #[test]
    #[ignore = "requires innoextract 1.9, Linux Landlock and GOG_TEST_FIXTURES"]
    fn supported_inno_engine_extracts_single_and_spanning_installers() {
        let fixtures = PathBuf::from(std::env::var_os("GOG_TEST_FIXTURES").unwrap());
        let scratch = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let root = ImportRoot::private_staging(source.path().to_owned()).unwrap();
        let flag = AtomicBool::new(false);
        for (dir, setup, count) in [("single", "setup.exe", 1), ("multi", "spanning.exe", 3)] {
            let dest = source.path().join(dir);
            fs::create_dir(&dest).unwrap();
            for file in fs::read_dir(fixtures.join(dir)).unwrap() {
                let file = file.unwrap();
                fs::copy(file.path(), dest.join(file.file_name())).unwrap();
            }
            assert_eq!(candidates(&root, dir).unwrap(), [setup]);
            let (work, volumes) = Work::prepare(scratch.path(), &root, dir, setup, &flag).unwrap();
            assert_eq!(volumes.len(), count);
            let (report, listed) = work.inspect(setup.into(), volumes, &flag, true).unwrap();
            assert_eq!(report.installer_version, "5.5.7");
            let (_, scan) = work.extract(&report, &listed, &flag).unwrap();
            assert!(!scan.files.is_empty());
            assert_eq!(scan.files.len(), listed.len());
            assert!(scan.files.iter().all(|file| file.sha256.len() == 64));
            let manifest = declaration(report, &Hints::default(), &scan, None).unwrap();
            assert_eq!(
                manifest["appDepot"]["gogOfflineInstaller"]["installReadiness"],
                "Unknown"
            );
        }
        let multi = source.path().join("multi");
        fs::remove_file(multi.join("spanning-1b.bin")).unwrap();
        let inspect = || {
            let (work, volumes) =
                Work::prepare(scratch.path(), &root, "multi", "spanning.exe", &flag)?;
            work.inspect("spanning.exe".into(), volumes, &flag, true)
        };
        assert!(matches!(inspect(), Err(Error::MissingVolume)));
        fs::write(multi.join("spanning-1b.bin"), b"this is not an Inno volume").unwrap();
        assert!(matches!(inspect(), Err(Error::InvalidVolume)));
        fs::write(source.path().join("broken.exe"), b"MZ invalid setup data").unwrap();
        let (work, volumes) =
            Work::prepare(scratch.path(), &root, "", "broken.exe", &flag).unwrap();
        assert!(matches!(
            work.inspect("broken.exe".into(), volumes, &flag, true),
            Err(Error::Format)
        ));
        drop(work);
        flag.store(true, Ordering::Relaxed);
        assert!(matches!(
            Work::prepare(scratch.path(), &root, "single", "setup.exe", &flag),
            Err(Error::Cancelled)
        ));
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}
