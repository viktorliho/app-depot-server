//! Bounded, non-executing inspection shared by staged imports and registered Trees.
use app_depot_domain::recipe;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_PROBES: usize = 96;
const MAX_PROBE_BYTES: usize = 64 * 1024 * 1024;
const TOTAL_BYTES: usize = 256 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Package {
    pub path: String,
    pub family: &'static str,
    pub name: &'static str,
    architecture: &'static str,
    pub platforms: Vec<&'static str>,
    pub status: &'static str,
    evidence: &'static str,
    supporting_files: Vec<String>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    pub packages: Vec<Package>,
    pub suggested_launch: Option<String>,
    pub unresolved: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    DirectX,
    VisualCppX86,
    VisualCppX64,
    PhysX,
    PhysXLegacy,
    OpenAl,
}

impl Rule {
    fn identify(path: &str) -> Option<Self> {
        let (_, name) = split(path);
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "dxsetup.exe" => Some(Self::DirectX),
            "vcredist_x86.exe" | "vc_redist.x86.exe" => Some(Self::VisualCppX86),
            "vcredist_x64.exe" | "vc_redist.x64.exe" => Some(Self::VisualCppX64),
            "oalinst.exe" => Some(Self::OpenAl),
            _ if name.starts_with("physx")
                && (name.ends_with(".exe") || name.ends_with(".msi")) =>
            {
                Some(if name.contains("legacy") {
                    Self::PhysXLegacy
                } else {
                    Self::PhysX
                })
            }
            _ => None,
        }
    }

    fn identity(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::DirectX => ("directx", "Microsoft DirectX", "x86/x64 payload"),
            Self::VisualCppX86 => (
                "visual-cpp-x86",
                "Microsoft Visual C++ (x86)",
                "x86 payload",
            ),
            Self::VisualCppX64 => (
                "visual-cpp-x64",
                "Microsoft Visual C++ (x64)",
                "x64 payload",
            ),
            Self::PhysX => ("physx", "NVIDIA PhysX", "unknown payload architecture"),
            Self::PhysXLegacy => (
                "physx",
                "NVIDIA PhysX Legacy",
                "unknown payload architecture",
            ),
            Self::OpenAl => ("openal", "OpenAL", "unknown payload architecture"),
        }
    }
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn directx_support(paths: &BTreeSet<&str>, setup: &str) -> Vec<String> {
    let (directory, _) = split(setup);
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    paths
        .range(prefix.as_str()..)
        .take_while(|path| path.starts_with(&prefix))
        .filter_map(|path| {
            let (parent, file) = split(path);
            let name = file.to_ascii_lowercase();
            (parent == directory
                && (name.ends_with(".cab")
                    || matches!(name.as_str(), "dsetup.dll" | "dsetup32.dll")))
            .then(|| (*path).to_owned())
        })
        .take(256)
        .collect()
}

fn redist_path(path: &str) -> bool {
    split(path).0.split('/').any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "__redist" | "redist" | "_commonredist" | "redist64" | "redistributables"
        )
    })
}

pub(crate) fn probes(paths: &BTreeSet<&str>, launch: &BTreeSet<String>) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    for &path in paths {
        if !recipe::path(path) {
            continue;
        }
        if let Some(rule) = Rule::identify(path) {
            if candidates.len() >= MAX_PROBES - 8 {
                break;
            }
            candidates.insert(path.to_owned());
            if rule == Rule::DirectX {
                for member in directx_support(paths, path) {
                    if matches!(
                        split(&member).1.to_ascii_lowercase().as_str(),
                        "dsetup.dll" | "dsetup32.dll" | "dxupdate.cab"
                    ) {
                        candidates.insert(member);
                    }
                }
            }
        }
    }
    candidates
        .into_iter()
        .take(MAX_PROBES - 8)
        .chain(
            launch
                .iter()
                .filter(|path| recipe::path(path))
                .take(8)
                .cloned(),
        )
        .collect()
}

fn contains(data: &[u8], name: &str) -> bool {
    let needle: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    memchr::memmem::find(data, name.as_bytes()).is_some()
        || memchr::memmem::find(data, &needle).is_some()
}

// Return a supported Windows executable architecture. Check the header and section
// table bounds before inspecting product markers. This is classification, not trust.
fn pe(data: &[u8], length: u64, dll: bool) -> bool {
    let Some(offset) = data.get(60..64) else {
        return false;
    };
    let at = u32::from_le_bytes(offset.try_into().unwrap()) as usize;
    let Some(header) = data.get(at..at.saturating_add(26)) else {
        return false;
    };
    let machine = u16::from_le_bytes([header[4], header[5]]);
    let sections = u16::from_le_bytes([header[6], header[7]]) as usize;
    let optional = u16::from_le_bytes([header[20], header[21]]) as usize;
    let flags = u16::from_le_bytes([header[22], header[23]]);
    let magic = u16::from_le_bytes([header[24], header[25]]);
    data.starts_with(b"MZ")
        && at >= 64
        && header.starts_with(b"PE\0\0")
        && matches!((machine, magic), (0x14c, 0x10b) | (0x8664, 0x20b))
        && (1..=96).contains(&sections)
        && optional >= 96
        && flags & 2 != 0
        && (flags & 0x2000 != 0) == dll
        && u64::try_from(at + 24 + optional + 40 * sections).is_ok_and(|end| end <= length)
}

fn cabinet(data: &[u8]) -> bool {
    if data.len() < 36 || !data.starts_with(b"MSCF") {
        return false;
    }
    let offset = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    let length = offset(8);
    if length == data.len() {
        return true;
    }
    // Signed Microsoft cabinets can append a PKCS#7 SignedData blob outside the
    // cabinet's declared length. Require matching reserved header bounds.
    let Some(trailer) = data.get(length..) else {
        return false;
    };
    length >= 60
        && data[30] & 4 != 0
        && u16::from_le_bytes([data[36], data[37]]) >= 20
        && offset(44) == length
        && offset(48) == trailer.len()
        && trailer.starts_with(&[0x30, 0x82])
        && trailer.get(4..15)
            == Some(&[
                0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
            ])
}

struct Evidence {
    executable: bool,
    library: bool,
    cabinet: bool,
    compound: bool,
    directx: bool,
    cpp: bool,
    physx: bool,
    openal: bool,
    hash: String,
}

fn observe(
    path: &str,
    read: &mut impl FnMut(&str) -> io::Result<Box<dyn Read>>,
    budget: &mut usize,
) -> Option<Evidence> {
    let mut data = Vec::new();
    read(path)
        .ok()?
        .take((MAX_PROBE_BYTES.min(*budget) + 1) as u64)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() > MAX_PROBE_BYTES.min(*budget) {
        *budget = budget.saturating_sub(data.len());
        return None;
    }
    *budget -= data.len();
    let rule = Rule::identify(path);
    let info = Evidence {
        executable: pe(&data, data.len() as u64, false),
        library: pe(&data, data.len() as u64, true),
        cabinet: cabinet(&data),
        compound: data.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])
            && data.len() >= 512
            && data[28..30] == [0xfe, 0xff]
            && contains(&data, "SummaryInformation")
            && contains(&data, "ProductCode"),
        directx: rule == Some(Rule::DirectX)
            && (contains(&data, "Microsoft DirectX") || contains(&data, "DirectX Setup")),
        cpp: matches!(rule, Some(Rule::VisualCppX86 | Rule::VisualCppX64))
            && contains(&data, "Microsoft Visual C++")
            && contains(&data, "Redistributable"),
        physx: matches!(rule, Some(Rule::PhysX | Rule::PhysXLegacy))
            && contains(&data, "NVIDIA")
            && contains(&data, "PhysX"),
        openal: rule == Some(Rule::OpenAl)
            && contains(&data, "OpenAL")
            && (contains(&data, "Creative") || contains(&data, "OpenAL Installer")),
        hash: hex::encode(Sha256::digest(&data)),
    };
    Some(info)
}

pub(crate) fn inspect(
    paths: &BTreeSet<&str>,
    launch: &BTreeSet<String>,
    mut read: impl FnMut(&str) -> io::Result<Box<dyn Read>>,
    cancel: &AtomicBool,
    expected: Option<&BTreeMap<&str, &str>>,
) -> Report {
    let mut budget = TOTAL_BYTES;
    let mut evidence = BTreeMap::new();
    for path in probes(paths, launch) {
        if cancel.load(Ordering::Relaxed) || budget == 0 {
            break;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = evidence.entry(path.clone()) {
            if let Some(item) = observe(&path, &mut read, &mut budget).filter(|item| {
                expected.is_none_or(|hashes| {
                    hashes.get(path.as_str()).copied() == Some(item.hash.as_str())
                })
            }) {
                entry.insert(item);
            }
        }
    }
    let mut candidates = Vec::new();
    let mut unresolved = Vec::new();
    for &path in paths {
        let Some(rule) = Rule::identify(path) else {
            if redist_path(path)
                && path.to_ascii_lowercase().ends_with(".exe")
                && unresolved.len() < 24
            {
                unresolved.push(format!(
                    "Unclassified setup executable: {}",
                    path.chars().take(200).collect::<String>()
                ));
            }
            continue;
        };
        let (family, name, architecture) = rule.identity();
        let info = evidence.get(path);
        let supporting_files = if rule == Rule::DirectX {
            directx_support(paths, path)
        } else {
            Vec::new()
        };
        let support_ok = rule != Rule::DirectX
            || ["dsetup.dll", "dsetup32.dll", "dxupdate.cab"]
                .into_iter()
                .all(|name| {
                    supporting_files
                        .iter()
                        .filter(|member| split(member).1.eq_ignore_ascii_case(name))
                        .count()
                        == 1
                        && supporting_files.iter().any(|member| {
                            split(member).1.eq_ignore_ascii_case(name)
                                && evidence.get(member).is_some_and(|item| {
                                    if name.ends_with(".dll") {
                                        item.library
                                    } else {
                                        item.cabinet
                                    }
                                })
                        })
                });
        let product = info.is_some_and(|item| match rule {
            Rule::DirectX => item.directx,
            Rule::VisualCppX86 | Rule::VisualCppX64 => item.cpp,
            Rule::PhysX | Rule::PhysXLegacy => item.physx,
            Rule::OpenAl => item.openal,
        });
        let status = if product && info.is_some_and(|item| item.executable) && support_ok {
            "ready"
        } else if product
            && info.is_some_and(|item| item.compound)
            && path.to_ascii_lowercase().ends_with(".msi")
        {
            "requiresMsi"
        } else {
            "unresolved"
        };
        candidates.push((rule, Package { path: path.into(), family, name, architecture,
            platforms: if status == "ready" { vec!["windows/x86_64"] } else { vec![] },
            status, evidence: match status {
                "ready" => "Recognized filename and PE product evidence; runtime compatibility unverified",
                "requiresMsi" => "Recognized compound MSI and package product evidence; no direct executable",
                _ => "Filename matched; executable product or supporting files could not be verified",
            }, supporting_files: supporting_files.into_iter().take(256).collect() }));
        if candidates.len() == 64 {
            break;
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    // A multi-file package needs the complete companion set to establish equality.
    // Treat repeated DirectX trees as alternatives rather than deduplicating by the launcher alone.
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (_, item)) in candidates.iter().enumerate() {
        groups.entry(item.family).or_default().push(i);
    }
    for group in groups.values().filter(|group| group.len() > 1) {
        let first = group[0];
        let same = candidates[first].0 != Rule::DirectX
            && group.iter().all(|&i| {
                candidates[i].1.status == "ready"
                    && evidence[&candidates[i].1.path].hash
                        == evidence[&candidates[first].1.path].hash
            });
        for &i in group {
            if candidates[i].1.status == "ready" {
                let item = &mut candidates[i].1;
                if !same {
                    item.status = "alternative";
                } else if i != first {
                    item.status = "duplicate";
                }
            }
        }
    }
    let root_launch: Vec<_> = launch.iter().filter(|path| !path.contains('/')).collect();
    let only = if root_launch.len() == 1 {
        root_launch.first().copied()
    } else if launch.len() == 1 {
        launch.first()
    } else {
        None
    };
    let suggested_launch = only
        .filter(|path| {
            Rule::identify(path).is_none()
                && evidence.get(*path).is_some_and(|item| item.executable)
        })
        .cloned();
    Report {
        packages: candidates.into_iter().map(|(_, item)| item).collect(),
        suggested_launch,
        unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(marker: &[u8], dll: bool) -> Vec<u8> {
        let mut data = vec![0; 512];
        data[..2].copy_from_slice(b"MZ");
        data[60] = 64;
        data[64..68].copy_from_slice(b"PE\0\0");
        data[68..70].copy_from_slice(&0x14cu16.to_le_bytes());
        data[70] = 1;
        data[84] = 224;
        data[86..88].copy_from_slice(&(if dll { 0x2002u16 } else { 2 }).to_le_bytes());
        data[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        data.extend_from_slice(marker);
        data
    }

    #[test]
    fn classifies_orders_and_deduplicates_packages() {
        let vc = executable(b"Microsoft Visual C++ Redistributable", false);
        let mut cab = vec![0; 36];
        cab[..4].copy_from_slice(b"MSCF");
        cab[8] = 36;
        let files = BTreeMap::from([
            ("game.exe", executable(b"", false)),
            ("redist/dx/DXSETUP.exe", executable(b"DirectX Setup", false)),
            ("redist/dx/dsetup.dll", executable(b"", true)),
            ("redist/dx/dsetup32.dll", executable(b"", true)),
            ("redist/dx/dxupdate.cab", cab),
            ("redist/vc/vcredist_x86.exe", vc.clone()),
            ("second/vc_redist.x86.exe", vc),
            ("redist/p/PhysX.exe", executable(b"NVIDIA PhysX", false)),
            ("redist/helper.exe", executable(b"", false)),
        ]);
        let inspect_files = |files: &BTreeMap<&str, Vec<u8>>| {
            inspect(
                &files.keys().copied().collect(),
                &BTreeSet::from(["game.exe".into()]),
                |path| Ok(Box::new(io::Cursor::new(files[path].clone()))),
                &AtomicBool::new(false),
                None,
            )
        };
        let report = inspect_files(&files);
        assert_eq!(report.suggested_launch.as_deref(), Some("game.exe"));
        assert_eq!(
            report
                .packages
                .iter()
                .map(|item| item.status)
                .collect::<Vec<_>>(),
            ["ready", "ready", "duplicate", "ready"]
        );
        assert_eq!(report.packages[0].supporting_files.len(), 3);
        assert_eq!(
            report.unresolved,
            ["Unclassified setup executable: redist/helper.exe"]
        );
        let mut different = files;
        different
            .get_mut("second/vc_redist.x86.exe")
            .unwrap()
            .push(0);
        let report = inspect_files(&different);
        assert_eq!(report.packages[1].status, "alternative");
        assert_eq!(report.packages[2].status, "alternative");
    }
}
