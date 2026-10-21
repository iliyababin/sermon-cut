//! Small filesystem helpers: crash-safe writes, fingerprints, moves.

use super::Fingerprint;
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

const HASH_CHUNK: u64 = 4 * 1024 * 1024;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Marker in temp file names, so leftovers from a crash can be swept.
pub const TMP_MARKER: &str = ".tmp-";

/// Write a file so that a crash leaves either the old or the new contents,
/// never half of one: write a temp file, flush it, rename it over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let tmp = dir.join(format!(
        "{}{}{}-{}",
        name.to_string_lossy(),
        TMP_MARKER,
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;

    // Make the rename itself durable. Opening a directory fails on Windows,
    // which is fine to ignore there.
    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("Failed to serialize: {}", e))?;
    write_atomic(path, &bytes).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

#[derive(Debug)]
pub enum ReadJsonError {
    Io(io::Error),
    TooLarge,
    Invalid(String),
}

impl std::fmt::Display for ReadJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadJsonError::Io(e) => write!(f, "{}", e),
            ReadJsonError::TooLarge => write!(f, "file is too large"),
            ReadJsonError::Invalid(e) => write!(f, "{}", e),
        }
    }
}

/// Read a JSON file. Ok(None) when it doesn't exist.
pub fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> Result<Option<T>, ReadJsonError> {
    let bytes = match read_limited(path, max_bytes) {
        Ok(b) => b,
        Err(ReadJsonError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| ReadJsonError::Invalid(e.to_string()))
}

pub fn read_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ReadJsonError> {
    let file = File::open(path).map_err(ReadJsonError::Io)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadJsonError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ReadJsonError::TooLarge);
    }
    Ok(bytes)
}

pub fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hash the size plus the first and last 4 MB. Cheap even for 10 GB files,
/// and different for any real replacement or re-encode.
pub fn content_hash(path: &Path, size: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut ctx = md5::Context::new();
    ctx.consume(size.to_le_bytes());

    let mut buf = vec![0u8; HASH_CHUNK as usize];
    if size <= 2 * HASH_CHUNK {
        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;
        ctx.consume(&rest);
    } else {
        file.read_exact(&mut buf)?;
        ctx.consume(&buf);
        file.seek(SeekFrom::Start(size - HASH_CHUNK))?;
        file.read_exact(&mut buf)?;
        ctx.consume(&buf);
    }
    Ok(format!("{:x}", ctx.compute()))
}

/// Fingerprint a file as it is right now. Refuses symlinks.
pub fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("Can't read {}: {}", path.display(), e))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let size = meta.len();
    let hash = content_hash(path, size).map_err(|e| format!("Can't read {}: {}", path.display(), e))?;
    Ok(Fingerprint {
        size,
        mtime_ms: mtime_ms(&meta),
        hash,
    })
}

/// Move a file, copying across volumes when rename can't. Safe to repeat:
/// if the source is gone and the destination exists, it's already done.
pub fn move_file(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.exists() {
        if dst.exists() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} not found", src.display()),
        ));
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Different volume (EXDEV) or similar: copy, verify, then delete.
            let tmp = dst.with_file_name(format!(
                "{}{}{}",
                dst.file_name().unwrap_or_default().to_string_lossy(),
                TMP_MARKER,
                std::process::id()
            ));
            let copied = fs::copy(src, &tmp)?;
            File::open(&tmp)?.sync_all()?;
            if copied != fs::metadata(src)?.len() {
                let _ = fs::remove_file(&tmp);
                return Err(io::Error::other("copy size mismatch"));
            }
            fs::rename(&tmp, dst)?;
            fs::remove_file(src)
        }
    }
}

/// Is an ISO-BMFF file (mp4/mov/m4v) complete? Walks the top-level boxes:
/// a copy still in progress has a last box that runs past the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Truncated,
    Unknown,
}

pub fn mp4_completeness(path: &Path, size: u64) -> Completeness {
    let Ok(mut file) = File::open(path) else {
        return Completeness::Unknown;
    };
    let mut offset = 0u64;
    let mut seen_moov = false;
    for _ in 0..100_000 {
        if offset == size {
            return if seen_moov { Completeness::Complete } else { Completeness::Unknown };
        }
        if offset > size {
            return Completeness::Truncated;
        }
        if size - offset < 8 {
            return Completeness::Truncated;
        }
        let mut header = [0u8; 16];
        if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut header[..8]).is_err() {
            return Completeness::Unknown;
        }
        let small = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind: [u8; 4] = header[4..8].try_into().unwrap();
        if !kind.iter().all(|b| b.is_ascii_alphanumeric() || *b == b' ' || *b == 0xa9) {
            return Completeness::Unknown;
        }
        // Not ISO-BMFF at all (e.g. a Matroska file named .mp4): let ffprobe decide.
        const FIRST_BOXES: [&[u8; 4]; 8] = [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid"];
        if offset == 0 && !FIRST_BOXES.contains(&&kind) {
            return Completeness::Unknown;
        }
        let box_size = match small {
            0 => size - offset,
            1 => {
                if size - offset < 16 || file.read_exact(&mut header[8..16]).is_err() {
                    return Completeness::Truncated;
                }
                u64::from_be_bytes(header[8..16].try_into().unwrap())
            }
            n => n,
        };
        if box_size < 8 {
            return Completeness::Unknown;
        }
        if &kind == b"moov" {
            seen_moov = true;
        }
        offset = match offset.checked_add(box_size) {
            Some(o) => o,
            None => return Completeness::Unknown,
        };
    }
    Completeness::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], payload: usize) -> Vec<u8> {
        let mut v = ((payload + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend(std::iter::repeat(0u8).take(payload));
        v
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn interrupted_write_keeps_old_contents() {
        // Simulate a crash after the temp file was written but before rename.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.json");
        write_atomic(&path, b"good").unwrap();
        fs::write(tmp.path().join(format!("library.json{}99-0", TMP_MARKER)), b"half").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"good");
    }

    #[test]
    fn read_json_limits_size() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.json");
        fs::write(&path, b"[1,2,3]").unwrap();
        assert!(matches!(read_json::<Vec<u8>>(&path, 3), Err(ReadJsonError::TooLarge)));
        assert_eq!(read_json::<Vec<u8>>(&path, 100).unwrap(), Some(vec![1, 2, 3]));
        assert!(read_json::<Vec<u8>>(&tmp.path().join("missing"), 100).unwrap().is_none());
    }

    #[test]
    fn hash_changes_with_content_and_covers_both_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        let mut data = vec![7u8; (HASH_CHUNK * 3) as usize];
        fs::write(&path, &data).unwrap();
        let a = content_hash(&path, data.len() as u64).unwrap();
        let last = data.len() - 1;
        data[last] = 8;
        fs::write(&path, &data).unwrap();
        let b = content_hash(&path, data.len() as u64).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn mp4_box_walk_detects_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        let mut full = mp4_box(b"ftyp", 16);
        full.extend(mp4_box(b"moov", 100));
        full.extend(mp4_box(b"mdat", 5000));
        fs::write(&path, &full).unwrap();
        assert_eq!(mp4_completeness(&path, full.len() as u64), Completeness::Complete);

        let cut = &full[..full.len() - 1000];
        fs::write(&path, cut).unwrap();
        assert_eq!(mp4_completeness(&path, cut.len() as u64), Completeness::Truncated);

        // moov at the end (camera files): truncated means moov is missing.
        let mut camera = mp4_box(b"ftyp", 16);
        camera.extend(mp4_box(b"mdat", 5000));
        let complete_len = camera.len() + 108;
        camera.extend(mp4_box(b"moov", 100));
        assert_eq!(camera.len(), complete_len);
        fs::write(&path, &camera[..complete_len - 50]).unwrap();
        assert_eq!(mp4_completeness(&path, (complete_len - 50) as u64), Completeness::Truncated);

        fs::write(&path, b"not an mp4 file at all").unwrap();
        assert_eq!(mp4_completeness(&path, 22), Completeness::Unknown);
    }

    #[test]
    fn move_file_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a");
        let dst = tmp.path().join("b");
        fs::write(&src, b"x").unwrap();
        move_file(&src, &dst).unwrap();
        move_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"x");
        assert!(move_file(&tmp.path().join("nope"), &tmp.path().join("nope2")).is_err());
    }
}
