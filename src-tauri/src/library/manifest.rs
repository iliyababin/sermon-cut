//! library.json and the per-video JSON files, with validation on load.

use super::fsutil::{self, ReadJsonError};
use super::{is_valid_file_name, is_valid_id, Entry, EntryStatus, LibraryPaths, SCHEMA_VERSION};
use crate::state::TranscriptionResult;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

const MAX_MANIFEST_BYTES: u64 = 10 * 1024 * 1024;
const MAX_VIDEO_JSON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: u64 = 50 * 1024 * 1024;
const MAX_TEXT: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

impl Manifest {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }
}

pub enum Loaded {
    Ok(Manifest),
    /// Written by a newer app version: show it, but never write to it.
    Newer(Manifest, u32),
    /// Unreadable; it was moved aside to the given name and a fresh one started.
    Corrupt(Manifest, String),
}

#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

fn truncate(s: &mut Option<String>) {
    if let Some(v) = s {
        if v.len() > MAX_TEXT {
            let mut end = MAX_TEXT;
            while !v.is_char_boundary(end) {
                end -= 1;
            }
            v.truncate(end);
        }
    }
}

/// Drop anything that could be used to escape the library folder, and
/// duplicates. Logs what it dropped.
fn sanitize(manifest: &mut Manifest) {
    let mut seen_ids = HashSet::new();
    manifest.entries.retain(|e| {
        let ok = is_valid_id(&e.id) && is_valid_file_name(&e.file_name) && seen_ids.insert(e.id.clone());
        if !ok {
            eprintln!("[library] Ignoring invalid entry {:?} ({:?})", e.id, e.file_name);
        }
        ok
    });
    for e in &mut manifest.entries {
        truncate(&mut e.title);
        truncate(&mut e.source_url);
        truncate(&mut e.channel);
        truncate(&mut e.error);
        e.uploads.retain(|u| u.url.starts_with("https://"));
        if e.duration.is_some_and(|d| !d.is_finite() || d < 0.0) {
            e.duration = None;
        }
    }
}

pub fn load(paths: &LibraryPaths) -> Result<Loaded, String> {
    let path = paths.manifest();
    let bytes = match fsutil::read_limited(&path, MAX_MANIFEST_BYTES) {
        Ok(b) => b,
        Err(ReadJsonError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded::Ok(Manifest::default()))
        }
        Err(ReadJsonError::Io(e)) => return Err(format!("Can't read {}: {}", path.display(), e)),
        Err(e) => return Ok(set_aside(&path, &e.to_string())),
    };

    if let Ok(v) = serde_json::from_slice::<VersionOnly>(&bytes) {
        if v.version > SCHEMA_VERSION {
            // Show what we can understand, but never rewrite it.
            let mut manifest = serde_json::from_slice::<Manifest>(&bytes).unwrap_or_default();
            sanitize(&mut manifest);
            return Ok(Loaded::Newer(manifest, v.version));
        }
    }

    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(mut manifest) => {
            sanitize(&mut manifest);
            manifest.version = SCHEMA_VERSION;
            Ok(Loaded::Ok(manifest))
        }
        Err(e) => Ok(set_aside(&path, &e.to_string())),
    }
}

fn set_aside(path: &Path, reason: &str) -> Loaded {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let aside = path.with_file_name(format!("library.json.corrupt-{}", stamp));
    eprintln!("[library] library.json is unreadable ({}); moving it to {}", reason, aside.display());
    let _ = std::fs::rename(path, &aside);
    Loaded::Corrupt(
        Manifest::default(),
        aside.file_name().unwrap_or_default().to_string_lossy().to_string(),
    )
}

pub fn save(paths: &LibraryPaths, manifest: &Manifest) -> Result<(), String> {
    fsutil::write_json_atomic(&paths.manifest(), manifest)
}

/// When the app restarts, the job that was running is gone.
pub fn mark_interrupted(manifest: &mut Manifest) -> bool {
    let mut changed = false;
    for e in &mut manifest.entries {
        if e.status == EntryStatus::Processing {
            e.status = EntryStatus::Error;
            e.error = Some("Processing was interrupted. Click Retry to start again.".into());
            changed = true;
        }
    }
    changed
}

/// What the user typed on the review screen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub preacher: Option<String>,
}

/// videos/<id>/video.json: what processing made, plus review edits.
/// File names are relative to the video's folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoData {
    #[serde(default)]
    pub trimmed_file: Option<String>,
    #[serde(default)]
    pub trim_start: Option<f64>,
    #[serde(default)]
    pub trim_end: Option<f64>,
    #[serde(default)]
    pub thumbnail_options: Vec<String>,
    #[serde(default)]
    pub selected_thumbnail: Option<String>,
    #[serde(default)]
    pub review: Review,
}

pub fn load_video_data(dir: &Path) -> VideoData {
    let mut data: VideoData = match fsutil::read_json(&dir.join("video.json"), MAX_VIDEO_JSON_BYTES) {
        Ok(Some(d)) => d,
        Ok(None) => return VideoData::default(),
        Err(e) => {
            eprintln!("[library] Ignoring unreadable {}/video.json: {}", dir.display(), e);
            return VideoData::default();
        }
    };
    if data.trimmed_file.as_deref().is_some_and(|f| !is_valid_file_name(f)) {
        data.trimmed_file = None;
    }
    data.thumbnail_options.retain(|f| is_valid_file_name(f));
    if data.selected_thumbnail.as_deref().is_some_and(|f| !is_valid_file_name(f)) {
        data.selected_thumbnail = None;
    }
    truncate(&mut data.review.title);
    truncate(&mut data.review.preacher);
    if data.review.description.as_ref().is_some_and(|d| d.len() > 20_000) {
        data.review.description = None;
    }
    data
}

pub fn save_video_data(dir: &Path, data: &VideoData) -> Result<(), String> {
    fsutil::write_json_atomic(&dir.join("video.json"), data)
}

/// videos/<id>/transcript.json. Tied to the file's hash so a replaced file
/// is never processed with someone else's transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptFile {
    pub file_hash: String,
    pub transcription: TranscriptionResult,
}

pub fn load_transcript(dir: &Path) -> Option<TranscriptFile> {
    match fsutil::read_json(&dir.join("transcript.json"), MAX_TRANSCRIPT_BYTES) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[library] Ignoring unreadable {}/transcript.json: {}", dir.display(), e);
            None
        }
    }
}

pub fn save_transcript(dir: &Path, transcript: &TranscriptFile) -> Result<(), String> {
    fsutil::write_json_atomic(&dir.join("transcript.json"), transcript)
}

/// Remove everything processing made for a video, keeping nothing.
pub fn remove_video_dir(paths: &LibraryPaths, id: &str) -> Result<(), String> {
    let dir = paths.video_dir(id)?;
    // Refuse if .sermon-cut or videos/ was swapped for a symlink.
    paths.ensure_dir(&paths.videos_dir())?;
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&dir),
        Ok(_) => std::fs::remove_dir_all(&dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
    .map_err(|e| format!("Failed to remove {}: {}", dir.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{new_id, Fingerprint};

    fn lib() -> (tempfile::TempDir, LibraryPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(tmp.path().canonicalize().unwrap());
        paths.ensure_dir(&paths.data_dir()).unwrap();
        (tmp, paths)
    }

    fn entry(name: &str) -> Entry {
        Entry::new(
            name.into(),
            Fingerprint { size: 1, mtime_ms: 1, hash: "h".into() },
            None,
        )
    }

    #[test]
    fn round_trips() {
        let (_t, paths) = lib();
        let mut m = Manifest::default();
        m.entries.push(entry("a.mp4"));
        save(&paths, &m).unwrap();
        let Loaded::Ok(back) = load(&paths).unwrap() else { panic!() };
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].file_name, "a.mp4");
    }

    #[test]
    fn drops_malicious_entries() {
        let (_t, paths) = lib();
        let good = new_id();
        let json = serde_json::json!({
            "version": 1,
            "entries": [
                {"id": "../..", "file_name": "a.mp4", "fingerprint": {"size":1,"mtime_ms":1,"hash":"h"}, "status": "new", "added_at": ""},
                {"id": good, "file_name": "../../Documents/x", "fingerprint": {"size":1,"mtime_ms":1,"hash":"h"}, "status": "new", "added_at": ""},
                {"id": good, "file_name": "ok.mp4", "fingerprint": {"size":1,"mtime_ms":1,"hash":"h"}, "status": "new", "added_at": ""},
                {"id": good, "file_name": "dupe.mp4", "fingerprint": {"size":1,"mtime_ms":1,"hash":"h"}, "status": "new", "added_at": ""}
            ]
        });
        std::fs::write(paths.manifest(), json.to_string()).unwrap();
        let Loaded::Ok(m) = load(&paths).unwrap() else { panic!() };
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].file_name, "ok.mp4");
    }

    #[test]
    fn corrupt_manifest_is_set_aside_not_overwritten() {
        let (_t, paths) = lib();
        std::fs::write(paths.manifest(), b"{ not json").unwrap();
        let Loaded::Corrupt(m, aside) = load(&paths).unwrap() else { panic!() };
        assert!(m.entries.is_empty());
        assert!(aside.starts_with("library.json.corrupt-"));
        assert_eq!(std::fs::read(paths.data_dir().join(&aside)).unwrap(), b"{ not json");
        assert!(!paths.manifest().exists());
    }

    #[test]
    fn newer_version_is_read_only() {
        let (_t, paths) = lib();
        std::fs::write(paths.manifest(), br#"{"version": 99, "entries": [], "future": true}"#).unwrap();
        let Loaded::Newer(_, v) = load(&paths).unwrap() else { panic!() };
        assert_eq!(v, 99);
        // Untouched.
        assert!(std::fs::read_to_string(paths.manifest()).unwrap().contains("future"));
    }

    #[test]
    fn interrupted_jobs_become_errors() {
        let mut m = Manifest::default();
        let mut e = entry("a.mp4");
        e.status = EntryStatus::Processing;
        m.entries.push(e);
        m.entries.push(entry("b.mp4"));
        assert!(mark_interrupted(&mut m));
        assert_eq!(m.entries[0].status, EntryStatus::Error);
        assert_eq!(m.entries[1].status, EntryStatus::New);
    }

    #[test]
    fn video_data_drops_bad_names() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("video.json"),
            br#"{"trimmed_file": "../../x", "thumbnail_options": ["a.jpg", "/etc/passwd", "../b.jpg"], "selected_thumbnail": "../c"}"#,
        )
        .unwrap();
        let d = load_video_data(tmp.path());
        assert_eq!(d.trimmed_file, None);
        assert_eq!(d.thumbnail_options, vec!["a.jpg".to_string()]);
        assert_eq!(d.selected_thumbnail, None);
    }
}
