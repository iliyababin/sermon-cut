//! The watched library folder.
//!
//! Every video directly inside the folder the user picks is part of the
//! library. The app keeps everything it knows and makes in a hidden
//! `.sermon-cut/` folder next to the videos:
//!
//! ```text
//! <library>/
//!   Sunday Service.mp4          the user's files
//!   .sermon-cut/
//!     library.json              one small entry per video (see manifest.rs)
//!     lock.json                 which Mac has the library open
//!     tmp/                      downloads and jobs in progress
//!     videos/<uuid>/            transcript.json, video.json, trimmed.*, thumbnails
//! ```
//!
//! Everything read from `.sermon-cut/` is treated as untrusted: the folder
//! is visible in Finder and may be copied or synced, so ids and file names
//! are validated before they are ever joined into a path.

pub mod commands;
pub mod fsutil;
pub mod lock;
pub mod manifest;
pub mod migrate;
pub mod runtime;
pub mod scan;

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const DATA_DIR: &str = ".sermon-cut";
pub const SCHEMA_VERSION: u32 = 1;

/// Extensions (compared case-insensitively) that count as videos.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi", "mts"];

/// Longest video the app will send for transcription. The server
/// (backend/app.py) has a 30-minute limit per request; a 2-hour service
/// takes well under 10 minutes on its GPU.
pub const MAX_TRANSCRIBE_SECS: f64 = 2.0 * 60.0 * 60.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ms: i64,
    /// md5 over the size plus the first and last 4 MB (see fsutil::content_hash).
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    New,
    Processing,
    ReadyForReview,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRecord {
    pub url: String,
    pub uploaded_at: String,
    /// Fingerprint hash of the file that was uploaded, so a replaced file
    /// can still show "a previous version was uploaded".
    pub file_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<String>,
    /// YouTube's id for the video, for "Set thumbnail again".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    /// What didn't work after the upload (thumbnail, captions, playlist).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<crate::upload::UploadWarning>,
}

/// One video in library.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// Exactly as on disk (not normalized), so it can be opened.
    pub file_name: String,
    pub fingerprint: Fingerprint,
    pub status: EntryStatus,
    #[serde(default)]
    pub error: Option<String>,
    /// Title from YouTube for downloaded videos; otherwise the file name is shown.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub youtube_id: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub uploads: Vec<UploadRecord>,
    /// An unfinished resumable upload, continued by the next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_session: Option<crate::upload::UploadSession>,
    pub added_at: String,
    /// Id from the pre-0.4 state.json, so migration can be re-run safely.
    #[serde(default)]
    pub legacy_id: Option<String>,
}

impl Entry {
    pub fn new(file_name: String, fingerprint: Fingerprint, duration: Option<f64>) -> Self {
        Entry {
            id: new_id(),
            file_name,
            fingerprint,
            status: EntryStatus::New,
            error: None,
            title: None,
            source_url: None,
            youtube_id: None,
            channel: None,
            duration,
            hidden: false,
            uploads: Vec::new(),
            upload_session: None,
            added_at: chrono::Utc::now().to_rfc3339(),
            legacy_id: None,
        }
    }

    pub fn display_title(&self) -> String {
        if let Some(title) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return title.to_string();
        }
        Path::new(&self.file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file_name.clone())
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// Ids are UUIDs the app generated. Anything else (`../..`, absolute paths,
/// odd spellings of a UUID) is rejected before it can reach a path.
pub fn is_valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id)
        .map(|u| u.hyphenated().to_string() == id)
        .unwrap_or(false)
}

/// A name that is safe to join onto a directory: exactly one normal path
/// component, not hidden, no separators.
pub fn is_valid_file_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0'])
    {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// Whether a top-level file name looks like a video the library should show.
/// Partial downloads (`.part`, `.crdownload`, …) fail the extension check.
pub fn is_candidate_video(name: &str) -> bool {
    if !is_valid_file_name(name) {
        return false;
    }
    let lower = name.to_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    if !VIDEO_EXTENSIONS.contains(&ext) {
        return false;
    }
    // yt-dlp's intermediate files: "name.f137.mp4", "name.temp.mp4".
    if let Some((_, last)) = stem.rsplit_once('.') {
        let format_part = last.len() > 1
            && last.starts_with('f')
            && last[1..].chars().all(|c| c.is_ascii_digit());
        if last == "temp" || format_part {
            return false;
        }
    }
    true
}

/// Key for matching names: macOS may hand back the same accented name in
/// composed (NFC) or decomposed (NFD) form.
pub fn match_key(name: &str) -> String {
    name.nfc().collect()
}

/// Paths inside one library. `root` is canonical.
#[derive(Debug, Clone)]
pub struct LibraryPaths {
    pub root: PathBuf,
}

impl LibraryPaths {
    pub fn new(root: PathBuf) -> Self {
        LibraryPaths { root }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join(DATA_DIR)
    }

    pub fn manifest(&self) -> PathBuf {
        self.data_dir().join("library.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.data_dir().join("lock.json")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir().join("tmp")
    }

    pub fn videos_dir(&self) -> PathBuf {
        self.data_dir().join("videos")
    }

    pub fn video_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !is_valid_id(id) {
            return Err(format!("Invalid video id: {:?}", id));
        }
        Ok(self.videos_dir().join(id))
    }

    /// Path of a user's video. The name must be a single component.
    pub fn original(&self, file_name: &str) -> Result<PathBuf, String> {
        if !is_valid_file_name(file_name) {
            return Err(format!("Invalid file name: {:?}", file_name));
        }
        Ok(self.root.join(file_name))
    }

    /// Create (if needed) one of the app's own directories, refusing to
    /// work through a symlink planted in its place.
    pub fn ensure_dir(&self, dir: &Path) -> Result<(), String> {
        let mut current = self.root.clone();
        let relative = dir
            .strip_prefix(&self.root)
            .map_err(|_| format!("{} is outside the library", dir.display()))?;
        for component in relative.components() {
            let Component::Normal(part) = component else {
                return Err(format!("Unexpected path {}", dir.display()));
            };
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!("{} is a symlink; refusing to use it", current.display()))
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(format!("{} is not a folder", current.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&current)
                        .map_err(|e| format!("Failed to create {}: {}", current.display(), e))?;
                }
                Err(e) => return Err(format!("Failed to read {}: {}", current.display(), e)),
            }
        }
        Ok(())
    }

    pub fn ensure_video_dir(&self, id: &str) -> Result<PathBuf, String> {
        let dir = self.video_dir(id)?;
        self.ensure_dir(&dir)?;
        Ok(dir)
    }

    /// Last check before trashing a user's file: it must be a regular file
    /// (not a symlink) whose real parent is the library folder.
    pub fn verify_original(&self, path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| format!("Can't find {}: {}", path.display(), e))?;
        if !meta.file_type().is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        let parent = path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .ok_or_else(|| format!("Can't resolve {}", path.display()))?;
        let root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
        if parent != root {
            return Err(format!("{} is outside the library folder", path.display()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_must_be_canonical_uuids() {
        assert!(is_valid_id(&new_id()));
        for bad in [
            "",
            "..",
            "../..",
            "/etc",
            "abc",
            "{6f9619ff-8b86-d011-b42d-00cf4fc964ff}",
            "6F9619FF-8B86-D011-B42D-00CF4FC964FF",
            "6f9619ff8b86d011b42d00cf4fc964ff",
            "6f9619ff-8b86-d011-b42d-00cf4fc964ff/..",
        ] {
            assert!(!is_valid_id(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn file_names_must_be_one_component() {
        assert!(is_valid_file_name("Sunday Service.mp4"));
        assert!(is_valid_file_name("Café [abc].MOV"));
        for bad in [
            "",
            ".",
            "..",
            "../x.mp4",
            "../../Documents/x",
            "a/b.mp4",
            "/abs.mp4",
            ".hidden.mp4",
            "back\\slash.mp4",
            "nul\0.mp4",
        ] {
            assert!(!is_valid_file_name(bad), "{bad:?} should be rejected");
        }
        assert!(!is_valid_file_name(&"a".repeat(256)));
    }

    #[test]
    fn video_extensions_ignore_case() {
        for ok in ["a.mp4", "IMG_0001.MOV", "b.Mp4", "c.M4V", "d.mkv", "e.WEBM", "f.mts"] {
            assert!(is_candidate_video(ok), "{ok} should count");
        }
    }

    #[test]
    fn ignores_partial_hidden_and_intermediate_files() {
        for skip in [
            "a.mp4.part",
            "a.mp4.ytdl",
            "a.mp4.crdownload",
            "a.mp4.download",
            "a.tmp",
            "a.mp4.icloud",
            ".a.mp4.icloud",
            "._IMG_0001.MOV",
            ".DS_Store",
            "notes.txt",
            "noext",
            "abc.f137.mp4",
            "abc.f251.webm",
            "abc.temp.mp4",
        ] {
            assert!(!is_candidate_video(skip), "{skip} should be ignored");
        }
        // Only "f" + digits counts as a yt-dlp format suffix.
        assert!(is_candidate_video("Service.final.mp4"));
    }

    #[test]
    fn nfc_and_nfd_names_match() {
        let nfc = "Caf\u{e9}.mp4";
        let nfd = "Cafe\u{301}.mp4";
        assert_ne!(nfc, nfd);
        assert_eq!(match_key(nfc), match_key(nfd));
    }

    #[test]
    fn paths_reject_bad_input() {
        let paths = LibraryPaths::new(PathBuf::from("/lib"));
        assert!(paths.video_dir("../..").is_err());
        assert!(paths.original("../../Documents/x").is_err());
        assert_eq!(paths.original("a.mp4").unwrap(), PathBuf::from("/lib/a.mp4"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_refuses_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join(DATA_DIR)).unwrap();
        let paths = LibraryPaths::new(root);
        assert!(paths.ensure_dir(&paths.videos_dir()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn verify_original_rejects_symlinks_and_outside_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("precious.mp4");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, root.join("link.mp4")).unwrap();
        std::fs::write(root.join("real.mp4"), b"x").unwrap();

        let paths = LibraryPaths::new(root.clone());
        assert!(paths.verify_original(&root.join("real.mp4")).is_ok());
        assert!(paths.verify_original(&root.join("link.mp4")).is_err());
        assert!(paths.verify_original(&target).is_err());
    }
}
