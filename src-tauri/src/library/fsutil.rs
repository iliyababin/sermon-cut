//! Small filesystem helpers: crash-safe writes, fingerprints, moves.

use super::Fingerprint;
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

const HASH_CHUNK: u64 = 4 * 1024 * 1024;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Marker in temp file names, so leftovers from a crash can be swept.
pub const TMP_MARKER: &str = ".tmp-";

/// A temp file (`write_atomic`, `move_file`) of this run of the app: one
/// still being written, never a crash's leftover.
pub fn is_own_tmp(name: &str) -> bool {
    name.rsplit_once(TMP_MARKER)
        .and_then(|(_, rest)| rest.split('-').next())
        .is_some_and(|pid| pid == std::process::id().to_string())
}

/// Write a file so that a crash leaves either the old or the new contents,
/// never half of one: write a temp file, flush it, rename it over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, false)
}

/// `write_atomic` for a file only its owner may read (mode 600 on Unix),
/// e.g. one holding sign-in tokens. The mode is set before the file has any
/// contents.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, true)
}

fn create_file(path: &Path, private: bool) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    options.open(path)
}

fn write_atomic_with(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
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
        let mut file = create_file(&tmp, private)?;
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

/// A temp name next to `dst` that no other move or write uses.
fn unique_tmp(dst: &Path) -> std::path::PathBuf {
    dst.with_file_name(format!(
        "{}{}{}-{}",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        TMP_MARKER,
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Move a file, copying when it has to cross to another volume. Safe to
/// repeat: if the source is gone and the destination exists, it's done.
pub fn move_file(src: &Path, dst: &Path) -> io::Result<()> {
    move_file_with(src, dst, |a, b| fs::rename(a, b))
}

fn move_file_with(src: &Path, dst: &Path, rename: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    if !src.exists() {
        if dst.exists() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} not found", src.display()),
        ));
    }
    match rename(src, dst) {
        Ok(()) => Ok(()),
        // Only a different volume is worth a copy; any other failure (no
        // permission, a folder in the way) would fail the copy too, or
        // worse, half-succeed.
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            let tmp = unique_tmp(dst);
            let result = (|| {
                let copied = fs::copy(src, &tmp)?;
                File::open(&tmp)?.sync_all()?;
                if copied != fs::metadata(src)?.len() {
                    return Err(io::Error::other("copy size mismatch"));
                }
                fs::rename(&tmp, dst)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            result?;
            fs::remove_file(src)
        }
        Err(e) => Err(e),
    }
}

/// `path` made absolute and free of symlinks, for a path that may not
/// exist yet (e.g. a copy's destination). The deepest part that exists is
/// canonicalised (resolving any symlink in it; a dangling one fails) and
/// the rest appended. None for a relative path, a `..` (or anything but a
/// plain name) in the part that doesn't exist, or a part of that rest that
/// has turned into a symlink by the time it's checked.
pub fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    // symlink_metadata: a dangling symlink counts as existing, so it's
    // canonicalised (and refused) rather than treated as a plain new name.
    while fs::symlink_metadata(&existing).is_err() {
        match existing.components().next_back()? {
            Component::Normal(name) => rest.push(name.to_os_string()),
            Component::CurDir => {}
            _ => return None,
        }
        existing = existing.parent()?.to_path_buf();
    }
    let mut resolved = fs::canonicalize(&existing).ok()?;
    for name in rest.into_iter().rev() {
        resolved.push(name);
        // Created meanwhile as a symlink: it could lead anywhere.
        if fs::symlink_metadata(&resolved).is_ok_and(|m| m.file_type().is_symlink()) {
            return None;
        }
    }
    Some(resolved)
}

/// `path`, resolved (see `resolve`), if it is `root` or inside it once
/// symlinks and `..` are resolved in both; the one containment check every
/// path from the webview goes through (docs/INVARIANTS.md #11). Use the
/// path it returns, not the original.
pub fn resolve_within(path: &Path, root: &Path) -> Option<PathBuf> {
    let resolved = resolve(path)?;
    let root = resolve(root)?;
    resolved.starts_with(&root).then_some(resolved)
}

/// Whether `path`'s real parent folder is `dir` (a file directly in it).
pub fn is_directly_in(path: &Path, dir: &Path) -> bool {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    match (parent.and_then(resolve), resolve(dir)) {
        (Some(parent), Some(dir)) => parent == dir,
        _ => false,
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
            // A few bytes of padding after a complete file (some muxers
            // and copy tools add them) aren't a box still being written.
            return if seen_moov { Completeness::Complete } else { Completeness::Truncated };
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
    fn resolve_within_resolves_dots_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("sub/a.jpg"), b"x").unwrap();
        fs::write(outside.join("secret"), b"x").unwrap();
        let within = |p: &Path| resolve_within(p, &root).is_some();

        let real_root = root.canonicalize().unwrap();
        assert_eq!(resolve_within(&root.join("sub/a.jpg"), &root), Some(real_root.join("sub/a.jpg")));
        assert!(within(&root));
        // Not there yet (a copy's destination), in a folder not there yet.
        assert_eq!(resolve_within(&root.join("new/dir/b.jpg"), &root), Some(real_root.join("new/dir/b.jpg")));
        assert!(!within(&outside.join("new.jpg")));
        // `..` out of the root, existing or not; `..` in what doesn't exist.
        assert!(!within(&root.join("sub/../../outside/secret")));
        assert!(!within(&root.join("missing/../../outside/x")));
        assert!(!within(&root.join("missing/../sub/a.jpg")));
        assert!(within(&root.join("sub/../sub/a.jpg")));
        // Relative paths and look-alike prefixes.
        assert!(!within(Path::new("lib/sub/a.jpg")));
        fs::create_dir_all(tmp.path().join("lib2")).unwrap();
        assert!(!within(&tmp.path().join("lib2/x")));

        assert!(is_directly_in(&root.join("sub/a.jpg"), &root.join("sub")));
        assert!(!is_directly_in(&root.join("sub/a.jpg"), &root));
        assert!(!is_directly_in(Path::new("a.jpg"), &root));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A symlink inside the root that points out of it.
            symlink(&outside, root.join("escape")).unwrap();
            assert!(!within(&root.join("escape/secret")));
            assert!(!within(&root.join("escape/new.jpg")));
            // Its real parent is `outside`, not the root.
            assert!(!is_directly_in(&root.join("escape/secret"), &root));
            // A dangling symlink as the destination itself.
            symlink(outside.join("nope"), root.join("dangling")).unwrap();
            assert!(!within(&root.join("dangling")));
            // A root reached through a symlink still counts.
            symlink(&root, tmp.path().join("alias")).unwrap();
            assert!(within(&tmp.path().join("alias/sub/a.jpg")));
            assert!(resolve_within(&root.join("sub/a.jpg"), &tmp.path().join("alias")).is_some());
        }
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
    fn trailing_padding_after_moov_is_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.mp4");
        let mut full = mp4_box(b"ftyp", 16);
        full.extend(mp4_box(b"mdat", 500));
        full.extend(mp4_box(b"moov", 100));
        for extra in 1..=7 {
            let mut padded = full.clone();
            padded.extend(std::iter::repeat(0u8).take(extra));
            fs::write(&path, &padded).unwrap();
            assert_eq!(mp4_completeness(&path, padded.len() as u64), Completeness::Complete, "{extra}");
        }
        // Without a moov, a short tail is still a cut-off box header.
        let mut no_moov = mp4_box(b"ftyp", 16);
        no_moov.extend(mp4_box(b"mdat", 500));
        no_moov.extend([0u8; 3]);
        fs::write(&path, &no_moov).unwrap();
        assert_eq!(mp4_completeness(&path, no_moov.len() as u64), Completeness::Truncated);
    }

    #[test]
    fn move_file_copies_only_across_volumes() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a");
        let dst = tmp.path().join("b");
        fs::write(&src, b"x").unwrap();

        // Any failure but EXDEV is reported, with nothing copied.
        let denied = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(move_file_with(&src, &dst, denied).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(src.exists() && !dst.exists());

        // Across volumes: copied, source removed, no temp file left.
        let exdev = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::CrossesDevices));
        move_file_with(&src, &dst, exdev).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"x");
        assert!(!src.exists());
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("b")]);

        // Temp names never repeat, so two moves to one name can't collide.
        assert_ne!(unique_tmp(&dst), unique_tmp(&dst));
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
