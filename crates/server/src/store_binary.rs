//! Static architecture checks only. No loader or acquired executable is invoked.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};
pub fn verify(path: &Path, target: &str) -> io::Result<()> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = [0; 64];
    f.read_exact(&mut h)?;
    let u16le = |s: &[u8]| u16::from_le_bytes([s[0], s[1]]);
    let u32be = |s: &[u8]| u32::from_be_bytes([s[0], s[1], s[2], s[3]]);
    let ok = if let Some(arch) = target.strip_prefix("linux/") {
        let machine = u16le(&h[18..]);
        &h[..4] == b"\x7fELF"
            && h[5] == 1
            && matches!(u16le(&h[16..]), 2 | 3)
            && matches!(
                (arch, h[4], machine),
                ("x86", 1, 3) | ("x86_64", 2, 62) | ("aarch64", 2, 183) | ("ppc64le", 2, 21)
            )
    } else if let Some(arch) = target.strip_prefix("windows/") {
        let offset = u32::from_le_bytes(h[60..64].try_into().unwrap()) as u64;
        if &h[..2] != b"MZ" || offset < 64 || offset + 24 > len {
            false
        } else {
            f.seek(SeekFrom::Start(offset))?;
            let mut pe = [0; 24];
            f.read_exact(&mut pe)?;
            &pe[..4] == b"PE\0\0"
                && matches!(
                    (arch, u16le(&pe[4..])),
                    ("x86", 0x14c) | ("x86_64", 0x8664) | ("aarch64", 0xaa64)
                )
                && u16le(&pe[22..]) & 0x2002 == 2
        }
    } else if let Some(arch) = target.strip_prefix("macos/") {
        let cpu = match arch {
            "aarch64" => 0x0100000c,
            "x86_64" => 0x01000007,
            _ => return Err(io::Error::other("Unsupported Mach-O CPU")),
        };
        if h[..4] == [0xcf, 0xfa, 0xed, 0xfe] {
            u32::from_le_bytes(h[4..8].try_into().unwrap()) == cpu
                && u32::from_le_bytes(h[12..16].try_into().unwrap()) == 2
        } else if u32be(&h) == 0xcafebabe && (1..=16).contains(&u32be(&h[4..])) {
            f.seek(SeekFrom::Start(8))?;
            let mut found = false;
            for _ in 0..u32be(&h[4..]) {
                let mut record = [0; 20];
                f.read_exact(&mut record)?;
                let offset = u32be(&record[8..]) as u64;
                let size = u32be(&record[12..]) as u64;
                if u32be(&record) == cpu && size >= 32 && offset + size <= len {
                    let pos = f.stream_position()?;
                    f.seek(SeekFrom::Start(offset))?;
                    let mut thin = [0; 16];
                    f.read_exact(&mut thin)?;
                    found = thin[..4] == [0xcf, 0xfa, 0xed, 0xfe]
                        && u32::from_le_bytes(thin[4..8].try_into().unwrap()) == cpu
                        && u32::from_le_bytes(thin[12..16].try_into().unwrap()) == 2;
                    f.seek(SeekFrom::Start(pos))?;
                }
            }
            found
        } else {
            false
        }
    } else {
        false
    };
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Executable does not match {target}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn accepts_matching_native_elf_and_rejects_wrong_machine_or_object_type() {
        let mut h = [0u8; 64];
        h[..6].copy_from_slice(b"\x7fELF\x02\x01");
        h[16] = 3;
        h[18] = 183;
        let mut image = tempfile::NamedTempFile::new().unwrap();
        image.write_all(&h).unwrap();
        verify(image.path(), "linux/aarch64").unwrap();
        assert!(verify(image.path(), "linux/x86_64").is_err());
        assert!(verify(image.path(), "windows/aarch64").is_err());
        h[16] = 1;
        std::fs::write(image.path(), h).unwrap();
        assert!(verify(image.path(), "linux/aarch64").is_err());
    }
}
