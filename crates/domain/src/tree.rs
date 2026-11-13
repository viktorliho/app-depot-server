use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAGIC: &[u8; 13] = b"APPDEPOT-TREE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKind {
    File {
        size: u64,
        sha256: [u8; 32],
        executable: bool,
    },
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub kind: TreeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("invalid tree path")]
    Path,
    #[error("invalid tree structure")]
    Structure,
    #[error("invalid canonical stream")]
    Stream,
}

pub fn parse_sha256(hex: &str) -> Result<[u8; 32], TreeError> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TreeError::Stream);
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .map_err(|_| TreeError::Stream)?;
    }
    Ok(digest)
}

pub fn validate_path(path: &str) -> Result<(), TreeError> {
    let bytes = path.as_bytes();
    if bytes.is_empty()
        || bytes.len() > u32::MAX as usize
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains(['\\', '\0'])
    {
        return Err(TreeError::Path);
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(TreeError::Path);
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(TreeError::Path);
    }
    Ok(())
}

fn validate(entries: &[TreeEntry]) -> Result<(), TreeError> {
    let mut seen = HashSet::new();
    let mut directories = HashSet::new();
    let mut has_file = false;
    let mut previous: Option<&[u8]> = None;
    for entry in entries {
        validate_path(&entry.path)?;
        let bytes = entry.path.as_bytes();
        if previous.is_some_and(|prior| prior >= bytes) || !seen.insert(entry.path.as_str()) {
            return Err(TreeError::Structure);
        }
        previous = Some(bytes);
        if let Some((parent, _)) = entry.path.rsplit_once('/') {
            if !directories.contains(parent) {
                return Err(TreeError::Structure);
            }
        }
        match &entry.kind {
            TreeKind::Directory => {
                directories.insert(entry.path.as_str());
            }
            TreeKind::File { .. } => has_file = true,
        }
    }
    if !has_file {
        return Err(TreeError::Structure);
    }
    Ok(())
}

pub fn canonical_stream(entries: &[TreeEntry]) -> Result<Vec<u8>, TreeError> {
    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    validate(&entries)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        match entry.kind {
            TreeKind::File {
                size,
                sha256,
                executable,
            } => {
                bytes.push(1);
                bytes.extend_from_slice(&(entry.path.len() as u32).to_le_bytes());
                bytes.extend_from_slice(entry.path.as_bytes());
                bytes.extend_from_slice(&size.to_le_bytes());
                bytes.extend_from_slice(&sha256);
                bytes.push(u8::from(executable));
            }
            TreeKind::Directory => {
                bytes.push(2);
                bytes.extend_from_slice(&(entry.path.len() as u32).to_le_bytes());
                bytes.extend_from_slice(entry.path.as_bytes());
            }
        }
    }
    Ok(bytes)
}

pub fn identity(entries: &[TreeEntry]) -> Result<String, TreeError> {
    Ok(format!(
        "adtree:v1:sha256:{:x}",
        Sha256::digest(canonical_stream(entries)?)
    ))
}

pub fn parse_stream(bytes: &[u8]) -> Result<Vec<TreeEntry>, TreeError> {
    struct Reader<'a> {
        bytes: &'a [u8],
        offset: usize,
    }
    impl<'a> Reader<'a> {
        fn take(&mut self, length: usize) -> Result<&'a [u8], TreeError> {
            let end = self.offset.checked_add(length).ok_or(TreeError::Stream)?;
            let value = self.bytes.get(self.offset..end).ok_or(TreeError::Stream)?;
            self.offset = end;
            Ok(value)
        }
        fn u8(&mut self) -> Result<u8, TreeError> {
            Ok(self.take(1)?[0])
        }
        fn u16(&mut self) -> Result<u16, TreeError> {
            Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
        }
        fn u32(&mut self) -> Result<u32, TreeError> {
            Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
        }
        fn u64(&mut self) -> Result<u64, TreeError> {
            Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
        }
    }
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take(MAGIC.len())? != MAGIC || reader.u16()? != 1 {
        return Err(TreeError::Stream);
    }
    let count = reader.u64()?;
    if count > bytes.len() as u64 {
        return Err(TreeError::Stream);
    }
    let mut entries = Vec::new();
    for _ in 0..count {
        let entry_type = reader.u8()?;
        let path_length = reader.u32()? as usize;
        let path = std::str::from_utf8(reader.take(path_length)?)
            .map_err(|_| TreeError::Path)?
            .to_owned();
        let kind = match entry_type {
            1 => {
                let size = reader.u64()?;
                let sha256 = reader.take(32)?.try_into().unwrap();
                let executable = match reader.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(TreeError::Stream),
                };
                TreeKind::File {
                    size,
                    sha256,
                    executable,
                }
            }
            2 => TreeKind::Directory,
            _ => return Err(TreeError::Stream),
        };
        entries.push(TreeEntry { path, kind });
    }
    if reader.offset != bytes.len() {
        return Err(TreeError::Stream);
    }
    validate(&entries)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: &[u8], executable: bool) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            kind: TreeKind::File {
                size: bytes.len() as u64,
                sha256: Sha256::digest(bytes).into(),
                executable,
            },
        }
    }
    fn directory(path: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            kind: TreeKind::Directory,
        }
    }

    fn normative_vector(index: usize) -> (Vec<u8>, String) {
        let document = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/tree-identity.md"
        ));
        let heading = format!("### V{index}\n");
        let section = document
            .split_once(&heading)
            .unwrap()
            .1
            .split("### ")
            .next()
            .unwrap();
        let stream = section
            .lines()
            .find(|line| line.starts_with("`415050"))
            .unwrap()
            .trim_matches('`');
        let identity = section
            .lines()
            .find_map(|line| line.strip_prefix("Identity: `"))
            .unwrap()
            .trim_end_matches('`');
        (hex::decode(stream).unwrap(), identity.to_owned())
    }

    #[test]
    fn all_canonical_vectors_match_normative_bytes_and_identities() {
        let v1 = vec![file("a.txt", b"A", false)];
        let v2 = vec![
            file("b.txt", b"B", false),
            directory("d"),
            file("d/a.txt", b"A", false),
            file("d/z.bin", &[0, 255], false),
        ];
        let vectors = [
            v1.clone(),
            v2.clone(),
            vec![
                file("a.txt", b"A", false),
                directory("empty"),
                directory("empty/nested"),
            ],
            vec![file("a.txt", b"A", true)],
            vec![file("A.txt", b"A", false), file("a.txt", b"A", false)],
            vec![
                file("cafe\u{301}.txt", b"A", false),
                file("café.txt", b"A", false),
            ],
            v1,
            v2,
            vec![file("a.txt", b"B", false)],
        ];
        for (index, entries) in vectors.iter().enumerate() {
            let (expected_stream, expected_id) = normative_vector(index + 1);
            let encoded = canonical_stream(entries).unwrap();
            assert_eq!(encoded, expected_stream, "V{} stream", index + 1);
            assert_eq!(
                identity(entries).unwrap(),
                expected_id,
                "V{} identity",
                index + 1
            );
            assert_eq!(parse_stream(&encoded).unwrap(), {
                let mut sorted = entries.clone();
                sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
                sorted
            });
        }
    }

    #[test]
    fn negative_vectors_reject_noncanonical_streams_and_invalid_trees() {
        let (v1, _) = normative_vector(1);
        let mut n1 = v1.clone();
        n1[15] = 2;
        let mut n2 = v1.clone();
        n2[23] = 3;
        let mut n3 = v1.clone();
        *n3.last_mut().unwrap() = 2;
        let mut n4 = v1.clone();
        n4.push(0);
        let mut n5 = v1.clone();
        n5[28..33].copy_from_slice(b"../ab");
        let mut n6 = v1.clone();
        n6[28..33].copy_from_slice(b"a\xfftxt");
        let (v3, _) = normative_vector(3);
        let mut n7 = v3[..74].to_vec();
        n7.extend_from_slice(&v3[84..]);
        n7.extend_from_slice(&v3[74..84]);
        for (index, stream) in [n1, n2, n3, n4, n5, n6, n7].iter().enumerate() {
            assert!(parse_stream(stream).is_err(), "N{}", index + 1);
        }
        for entries in [
            vec![file("a.txt", b"A", false), file("d/a.txt", b"A", false)],
            vec![file("d", b"A", false), file("d/a.txt", b"A", false)],
            vec![file("a.txt", b"A", false), file("a.txt", b"A", false)],
            vec![directory("a.txt"), file("a.txt", b"A", false)],
            vec![directory("empty")],
        ] {
            assert!(canonical_stream(&entries).is_err());
        }
        for path in [
            "../ab", "a//b", "/a", "a/", "a\\b", "C:a", ".", "a/..", "a\0b",
        ] {
            assert!(validate_path(path).is_err(), "{path:?}");
        }
    }
}
