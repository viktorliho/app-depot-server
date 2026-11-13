use app_depot_application::upload::{InventoryEntry, UploadError};
use app_depot_domain::tree::{canonical_stream, validate_path};
use crc32fast::Hasher as Crc32;
use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use uuid::Uuid;

#[derive(Clone)]
struct Member {
    path: String,
    method: u16,
    flags: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local_offset: u64,
    executable: bool,
    directory: bool,
}

fn invalid() -> UploadError {
    UploadError::InvalidZip
}
fn storage(error: impl std::fmt::Display) -> UploadError {
    UploadError::Retryable(error.to_string())
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, UploadError> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, UploadError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn read_at(file: &mut File, offset: u64, size: usize) -> Result<Vec<u8>, UploadError> {
    file.seek(SeekFrom::Start(offset)).map_err(|_| invalid())?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|_| invalid())?;
    Ok(bytes)
}

fn members(file: &mut File) -> Result<Vec<Member>, UploadError> {
    let length = file.metadata().map_err(|_| invalid())?.len();
    let tail_len = length.min(65_557) as usize;
    let tail = read_at(file, length - tail_len as u64, tail_len)?;
    let eocd = (0..tail_len.saturating_sub(21))
        .rev()
        .find(|&at| {
            tail.get(at..at + 4) == Some(&b"PK\x05\x06"[..])
                && u16_at(&tail, at + 20)
                    .ok()
                    .map(|comment| at + 22 + comment as usize == tail_len)
                    .unwrap_or(false)
        })
        .ok_or_else(invalid)?;
    if u16_at(&tail, eocd + 4)? != 0
        || u16_at(&tail, eocd + 6)? != 0
        || u16_at(&tail, eocd + 8)? != u16_at(&tail, eocd + 10)?
    {
        return Err(invalid());
    }
    let count = u16_at(&tail, eocd + 10)? as usize;
    let cd_size = u32_at(&tail, eocd + 12)? as u64;
    let cd_start = u32_at(&tail, eocd + 16)? as u64;
    if count == 0
        || count == 0xffff
        || cd_size == 0xffff_ffff
        || cd_start == 0xffff_ffff
        || cd_start
            .checked_add(cd_size)
            .filter(|end| *end <= length - tail_len as u64 + eocd as u64)
            .is_none()
    {
        return Err(invalid());
    }
    let mut at = cd_start;
    let mut result = Vec::with_capacity(count);
    let mut explicit = HashSet::new();
    for _ in 0..count {
        let head = read_at(file, at, 46)?;
        if head.get(..4) != Some(&b"PK\x01\x02"[..]) {
            return Err(invalid());
        }
        let made_by = u16_at(&head, 4)?;
        let flags = u16_at(&head, 8)?;
        let method = u16_at(&head, 10)?;
        if flags & !0x080e != 0 {
            return Err(invalid());
        }
        if !matches!(method, 0 | 8) {
            return Err(UploadError::UnsupportedZipMethod);
        }
        let crc = u32_at(&head, 16)?;
        let compressed = u32_at(&head, 20)? as u64;
        let size = u32_at(&head, 24)? as u64;
        let name_len = u16_at(&head, 28)? as usize;
        let extra_len = u16_at(&head, 30)? as usize;
        let comment_len = u16_at(&head, 32)? as usize;
        let disk = u16_at(&head, 34)?;
        let external = u32_at(&head, 38)?;
        let local_offset = u32_at(&head, 42)? as u64;
        if disk != 0
            || compressed == 0xffff_ffff
            || size == 0xffff_ffff
            || local_offset == 0xffff_ffff
        {
            return Err(invalid());
        }
        let end = at
            .checked_add(46 + name_len as u64 + extra_len as u64 + comment_len as u64)
            .ok_or_else(invalid)?;
        if end > cd_start + cd_size {
            return Err(invalid());
        }
        let raw_name = read_at(file, at + 46, name_len)?;
        if flags & 0x0800 == 0 && !raw_name.is_ascii() {
            return Err(invalid());
        }
        let name = std::str::from_utf8(&raw_name).map_err(|_| invalid())?;
        let directory = name.ends_with('/');
        let path = if directory {
            name.strip_suffix('/').unwrap()
        } else {
            name
        };
        validate_path(path).map_err(|_| invalid())?;
        if !explicit.insert(path.to_owned()) {
            return Err(invalid());
        }
        let unix_mode = if made_by >> 8 == 3 {
            (external >> 16) as u16
        } else {
            0
        };
        let mode_type = unix_mode & 0o170000;
        if mode_type != 0 && mode_type != if directory { 0o040000 } else { 0o100000 } {
            return Err(invalid());
        }
        if external & 0x10 != 0 && !directory {
            return Err(invalid());
        }
        if directory && (size != 0 || crc != 0) {
            return Err(invalid());
        }
        result.push(Member {
            path: path.to_owned(),
            method,
            flags,
            crc,
            compressed,
            size,
            local_offset,
            executable: !directory && unix_mode & 0o111 != 0,
            directory,
        });
        at = end;
    }
    if at != cd_start + cd_size {
        return Err(invalid());
    }
    Ok(result)
}

fn data_offset(file: &mut File, member: &Member, cd_start: u64) -> Result<u64, UploadError> {
    let local = read_at(file, member.local_offset, 30)?;
    if local.get(..4) != Some(&b"PK\x03\x04"[..])
        || u16_at(&local, 6)? != member.flags
        || u16_at(&local, 8)? != member.method
    {
        return Err(invalid());
    }
    if member.flags & 0x0008 == 0
        && (u32_at(&local, 14)? != member.crc
            || u32_at(&local, 18)? as u64 != member.compressed
            || u32_at(&local, 22)? as u64 != member.size)
    {
        return Err(invalid());
    }
    let name_len = u16_at(&local, 26)? as usize;
    let extra_len = u16_at(&local, 28)? as u64;
    if read_at(file, member.local_offset + 30, name_len)? != member.path.as_bytes()
        && read_at(file, member.local_offset + 30, name_len)?
            != format!("{}/", member.path).as_bytes()
    {
        return Err(invalid());
    }
    let offset = member
        .local_offset
        .checked_add(30 + name_len as u64 + extra_len)
        .ok_or_else(invalid)?;
    if offset
        .checked_add(member.compressed)
        .filter(|end| *end <= cd_start)
        .is_none()
    {
        return Err(invalid());
    }
    Ok(offset)
}

fn copy_checked(
    mut input: impl Read,
    output: &mut File,
    expected: u64,
) -> Result<(String, u32), UploadError> {
    let mut sha = Sha256::new();
    let mut crc = Crc32::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer).map_err(|_| invalid())?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or_else(invalid)?;
        if size > expected {
            return Err(invalid());
        }
        output.write_all(&buffer[..count]).map_err(storage)?;
        sha.update(&buffer[..count]);
        crc.update(&buffer[..count]);
    }
    if size != expected {
        return Err(invalid());
    }
    Ok((format!("{:x}", sha.finalize()), crc.finalize()))
}

pub fn extract(zip_path: &Path, stage_dir: &Path) -> Result<Vec<InventoryEntry>, UploadError> {
    let mut zip = File::open(zip_path).map_err(|_| invalid())?;
    let members = members(&mut zip)?;
    let cd_start = {
        let length = zip.metadata().map_err(|_| invalid())?.len();
        let tail_len = length.min(65_557) as usize;
        let tail = read_at(&mut zip, length - tail_len as u64, tail_len)?;
        let eocd = (0..tail_len.saturating_sub(21))
            .rev()
            .find(|&at| {
                tail.get(at..at + 4) == Some(&b"PK\x05\x06"[..])
                    && u16_at(&tail, at + 20)
                        .ok()
                        .map(|comment| at + 22 + comment as usize == tail_len)
                        .unwrap_or(false)
            })
            .ok_or_else(invalid)?;
        u32_at(&tail, eocd + 16)? as u64
    };
    fs::create_dir_all(stage_dir).map_err(storage)?;
    let mut paths: BTreeMap<String, InventoryEntry> = BTreeMap::new();
    let mut ranges = Vec::new();
    let mut total = 0u64;
    for member in members {
        for (at, _) in member.path.match_indices('/') {
            let parent = member.path[..at].to_owned();
            if matches!(paths.get(&parent), Some(InventoryEntry::File { .. })) {
                return Err(invalid());
            }
            paths
                .entry(parent.clone())
                .or_insert(InventoryEntry::Directory { path: parent });
        }
        let offset = data_offset(&mut zip, &member, cd_start)?;
        ranges.push((member.local_offset, offset + member.compressed));
        if member.directory {
            if matches!(paths.get(&member.path), Some(InventoryEntry::File { .. })) {
                return Err(invalid());
            }
            zip.seek(SeekFrom::Start(offset)).map_err(|_| invalid())?;
            if member.method == 0 {
                if member.compressed != 0 {
                    return Err(invalid());
                }
            } else {
                let mut decoded = DeflateDecoder::new((&mut zip).take(member.compressed));
                let mut probe = [0u8; 1];
                if decoded.read(&mut probe).map_err(|_| invalid())? != 0
                    || decoded.total_in() != member.compressed
                {
                    return Err(invalid());
                }
            }
            paths.insert(
                member.path.clone(),
                InventoryEntry::Directory { path: member.path },
            );
            continue;
        }
        if paths.contains_key(&member.path) {
            return Err(invalid());
        }
        total = total.checked_add(member.size).ok_or_else(invalid)?;
        if total > 1_099_511_627_776 {
            return Err(invalid());
        }
        let file_id = Uuid::new_v4().to_string();
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage_dir.join(&file_id))
            .map_err(storage)?;
        zip.seek(SeekFrom::Start(offset)).map_err(|_| invalid())?;
        let (digest, crc) = if member.method == 0 {
            if member.compressed != member.size {
                return Err(invalid());
            }
            copy_checked((&mut zip).take(member.compressed), &mut output, member.size)?
        } else {
            let mut decoder = DeflateDecoder::new((&mut zip).take(member.compressed));
            let result = copy_checked(&mut decoder, &mut output, member.size)?;
            if decoder.total_in() != member.compressed {
                return Err(invalid());
            }
            result
        };
        if crc != member.crc {
            return Err(invalid());
        }
        output.sync_all().map_err(storage)?;
        paths.insert(
            member.path.clone(),
            InventoryEntry::File {
                path: member.path,
                file_id,
                size: member.size,
                sha256: digest,
                executable: member.executable,
            },
        );
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid());
    }
    let entries: Vec<_> = paths.into_values().collect();
    let logical = entries
        .iter()
        .map(|entry| entry.tree_entry())
        .collect::<Result<Vec<_>, _>>()?;
    canonical_stream(&logical).map_err(|_| invalid())?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::bootstrap_admin, init_local, lock_local, open_catalog, upload::UploadAdapter,
        LocalMode,
    };
    use app_depot_application::upload::{
        FileClaim, Interval, InventoryBatch, InventoryEntryInput, Target, UploadCoordinator,
        UploadCreate,
    };
    use flate2::{write::DeflateEncoder, Compression};

    fn make_zip(path: &Path, method: u16, name: &str, mode: u16) {
        let data = b"hello tree\n";
        let body = if method == 8 {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        } else {
            data.to_vec()
        };
        let crc = crc32fast::hash(data);
        let name = name.as_bytes();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend_from_slice(&20u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&method.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&body);
        let central = bytes.len() as u32;
        bytes.extend_from_slice(b"PK\x01\x02");
        bytes.extend_from_slice(&0x0314u16.to_le_bytes());
        bytes.extend_from_slice(&20u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&method.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&((mode as u32) << 16).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(name);
        let central_size = bytes.len() as u32 - central;
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&central_size.to_le_bytes());
        bytes.extend_from_slice(&central.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn store_and_deflate_have_same_tree_identity_and_preserve_executable() {
        let root = tempfile::tempdir().unwrap();
        let mut identities = Vec::new();
        for method in [0, 8] {
            let zip = root.path().join(format!("{method}.zip"));
            make_zip(&zip, method, "dir/run.sh", 0o100755);
            let entries = extract(&zip, &root.path().join(format!("{method}-stage"))).unwrap();
            assert_eq!(entries.len(), 2);
            assert!(matches!(
                &entries[1],
                InventoryEntry::File {
                    executable: true,
                    ..
                }
            ));
            let logical = entries
                .iter()
                .map(|entry| entry.tree_entry().unwrap())
                .collect::<Vec<_>>();
            identities.push(app_depot_domain::tree::identity(&logical).unwrap());
        }
        assert_eq!(identities[0], identities[1]);
    }

    #[test]
    fn unsafe_path_and_symbolic_link_rejected() {
        let root = tempfile::tempdir().unwrap();
        let zip = root.path().join("unsafe.zip");
        make_zip(&zip, 0, "../escape", 0o100644);
        assert!(extract(&zip, &root.path().join("stage")).is_err());
        make_zip(&zip, 0, "link", 0o120777);
        assert!(extract(&zip, &root.path().join("stage")).is_err());
    }

    #[tokio::test]
    async fn zip_transport_finalizes_as_tree_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        init_local(root.path(), "zip", LocalMode::Dev).unwrap();
        let lock = lock_local(root.path(), "zip", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = root.path().join("bootstrap");
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
        let zip = root.path().join("transport.zip");
        make_zip(&zip, 8, "dir/run.sh", 0o100755);
        let bytes = fs::read(&zip).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let coordinator =
            UploadCoordinator::new(UploadAdapter::new(pool.clone(), lock.path.clone()));
        let session = coordinator
            .create(
                &user,
                "zip-create",
                &UploadCreate {
                    mode: "ZipImport".into(),
                    target: Target::NewApp {
                        new_app_name: "ZIP tree".into(),
                    },
                    file: Some(FileClaim {
                        size: bytes.len() as u64,
                        sha256: digest,
                    }),
                },
            )
            .await
            .unwrap();
        assert!(session.inventory_complete);
        let mut input = bytes.as_slice();
        coordinator
            .receive(
                &user,
                &session.id,
                &session.file().file_id,
                Interval {
                    start: 0,
                    end: bytes.len() as u64,
                },
                &mut input,
            )
            .await
            .unwrap();
        let result = coordinator
            .finalize(&user, &session.id, "zip-finalize")
            .await
            .unwrap();
        assert_eq!(result.phase, "Completed");
        let zip_artifact = result.artifact_id.unwrap();
        assert!(zip_artifact.starts_with("adtree:v1:sha256:"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artifacts WHERE kind='File'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        let bare = coordinator
            .create(
                &user,
                "bare-equivalent-create",
                &UploadCreate {
                    mode: "BareTree".into(),
                    target: Target::NewApp {
                        new_app_name: "Bare equivalent".into(),
                    },
                    file: None,
                },
            )
            .await
            .unwrap();
        let appended = coordinator
            .append_inventory(
                &user,
                &bare.id,
                "bare-equivalent-inventory",
                &InventoryBatch {
                    entries: vec![
                        InventoryEntryInput::Directory { path: "dir".into() },
                        InventoryEntryInput::File {
                            path: "dir/run.sh".into(),
                            size: 11,
                            sha256: format!("{:x}", Sha256::digest(b"hello tree\n")),
                            executable: true,
                        },
                    ],
                },
            )
            .await
            .unwrap();
        coordinator
            .complete_inventory(&user, &bare.id, "bare-equivalent-complete")
            .await
            .unwrap();
        let file_id = match &appended.entries[1] {
            InventoryEntry::File { file_id, .. } => file_id,
            _ => unreachable!(),
        };
        let mut bare_bytes: &[u8] = b"hello tree\n";
        coordinator
            .receive(
                &user,
                &bare.id,
                file_id,
                Interval { start: 0, end: 11 },
                &mut bare_bytes,
            )
            .await
            .unwrap();
        let bare_result = coordinator
            .finalize(&user, &bare.id, "bare-equivalent-finalize")
            .await
            .unwrap();
        assert_eq!(
            bare_result.artifact_id.as_deref(),
            Some(zip_artifact.as_str())
        );
        let interrupted = coordinator
            .create(
                &user,
                "interrupted-zip-create",
                &UploadCreate {
                    mode: "ZipImport".into(),
                    target: Target::NewApp {
                        new_app_name: "Recovered ZIP".into(),
                    },
                    file: Some(FileClaim {
                        size: bytes.len() as u64,
                        sha256: format!("{:x}", Sha256::digest(&bytes)),
                    }),
                },
            )
            .await
            .unwrap();
        let mut interrupted_bytes = bytes.as_slice();
        coordinator
            .receive(
                &user,
                &interrupted.id,
                &interrupted.file().file_id,
                Interval {
                    start: 0,
                    end: bytes.len() as u64,
                },
                &mut interrupted_bytes,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE upload_sessions SET phase='Importing' WHERE id=?")
            .bind(&interrupted.id)
            .execute(&pool)
            .await
            .unwrap();
        fs::write(
            lock.path
                .join("staging")
                .join(&interrupted.id)
                .join("files")
                .join("partial"),
            b"junk",
        )
        .unwrap();
        coordinator.recover().await.unwrap();
        let recovered = coordinator
            .result(&user, &interrupted.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.phase, "Completed");
        assert_eq!(
            recovered.artifact_id.as_deref(),
            Some(zip_artifact.as_str())
        );
        make_zip(&zip, 0, "../escape", 0o100644);
        let bad_bytes = fs::read(&zip).unwrap();
        let bad_session = coordinator
            .create(
                &user,
                "bad-zip-create",
                &UploadCreate {
                    mode: "ZipImport".into(),
                    target: Target::NewApp {
                        new_app_name: "Invalid ZIP".into(),
                    },
                    file: Some(FileClaim {
                        size: bad_bytes.len() as u64,
                        sha256: format!("{:x}", Sha256::digest(&bad_bytes)),
                    }),
                },
            )
            .await
            .unwrap();
        let mut bad_input = bad_bytes.as_slice();
        coordinator
            .receive(
                &user,
                &bad_session.id,
                &bad_session.file().file_id,
                Interval {
                    start: 0,
                    end: bad_bytes.len() as u64,
                },
                &mut bad_input,
            )
            .await
            .unwrap();
        let rejected = coordinator
            .finalize(&user, &bad_session.id, "bad-zip-finalize")
            .await
            .unwrap();
        assert_eq!(rejected.phase, "Failed");
        assert_eq!(rejected.problem.unwrap()["code"], "invalid_zip");
    }
}
