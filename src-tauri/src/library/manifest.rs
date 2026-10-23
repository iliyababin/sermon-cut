//! library.json and the per-video JSON files, with validation on load.

use super::fsutil::{self, ReadJsonError};
use super::{is_valid_file_name, is_valid_id, Entry, EntryStatus, LibraryPaths, FORMAT_VERSION, MIN_APP_VERSION, SCHEMA_VERSION};
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
    /// Missing in files written by v0.4.0 (schema 1).
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app_version: Option<String>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

fn schema_v1() -> u32 {
    1
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            version: FORMAT_VERSION,
            schema_version: SCHEMA_VERSION,
            min_app_version: Some(MIN_APP_VERSION.to_string()),
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
    /// Carries the app version it asks for, if it says.
    Newer(Manifest, Option<String>),
    /// Unreadable; it was moved aside to the given name and a fresh one started.
    Corrupt(Manifest, String),
    /// Unreadable, and it couldn't be set aside either: open read-only so
    /// it is never overwritten.
    Stuck(String),
}

#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    min_app_version: Option<String>,
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
        for u in &mut e.uploads {
            if u.video_id.as_deref().is_some_and(|v| !crate::upload::is_valid_video_id(v)) {
                u.video_id = None;
            }
        }
        // The session URI gets the access token, so only YouTube's own.
        if e.upload_session.as_ref().is_some_and(|s| !s.is_trusted()) {
            e.upload_session = None;
        }
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
        if v.version > FORMAT_VERSION || v.schema_version.is_some_and(|s| s > SCHEMA_VERSION) {
            // Show what we can understand, but never rewrite it.
            let mut manifest = serde_json::from_slice::<Manifest>(&bytes).unwrap_or_default();
            sanitize(&mut manifest);
            let wanted = v.min_app_version.filter(|m| m.len() <= 32 && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'));
            return Ok(Loaded::Newer(manifest, wanted));
        }
    }

    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(mut manifest) => {
            sanitize(&mut manifest);
            manifest.version = FORMAT_VERSION;
            manifest.schema_version = SCHEMA_VERSION;
            manifest.min_app_version = Some(MIN_APP_VERSION.to_string());
            Ok(Loaded::Ok(manifest))
        }
        Err(e) => Ok(set_aside(&path, &e.to_string())),
    }
}

fn set_aside(path: &Path, reason: &str) -> Loaded {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let aside = path.with_file_name(format!("library.json.corrupt-{}", stamp));
    set_aside_to(path, &aside, reason, |a, b| std::fs::rename(a, b))
}

/// Keep an unreadable library.json for recovery: move it to `aside`, or
/// copy it there if it can't be moved. If neither works, it must not be
/// overwritten, so the library opens read-only.
fn set_aside_to(path: &Path, aside: &Path, reason: &str, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Loaded {
    eprintln!("[library] library.json is unreadable ({}); moving it to {}", reason, aside.display());
    let kept = rename(path, aside).or_else(|e| {
        eprintln!("[library] Couldn't move it ({}); copying instead", e);
        std::fs::copy(path, aside).map(|_| ())
    });
    match kept {
        Ok(()) => Loaded::Corrupt(
            Manifest::default(),
            aside.file_name().unwrap_or_default().to_string_lossy().to_string(),
        ),
        Err(e) => Loaded::Stuck(format!(
            "The library's tracking file (.sermon-cut/library.json) can't be read, and Sermon Cut couldn't keep a copy of it ({}). The library is read-only so nothing is lost; move that file somewhere safe, then reopen the library.",
            e
        )),
    }
}

pub fn save(paths: &LibraryPaths, manifest: &Manifest) -> Result<(), String> {
    fsutil::write_json_atomic(&paths.manifest(), manifest)
}

/// When the app restarts, the job that was running is gone. A video that
/// already had results (a re-process) keeps them, since processing only
/// replaces them when it finishes. `running` is a job of this session that
/// is still going (the library was reopened under it), left alone.
pub fn mark_interrupted(manifest: &mut Manifest, running: Option<&str>, has_results: impl Fn(&Entry) -> bool) -> bool {
    let mut changed = false;
    for e in &mut manifest.entries {
        if e.status != EntryStatus::Processing || running == Some(e.id.as_str()) {
            continue;
        }
        if has_results(e) {
            e.status = if e.uploads.is_empty() { EntryStatus::ReadyForReview } else { EntryStatus::Done };
            e.error = Some("Re-processing was interrupted. The earlier results are kept.".into());
        } else {
            e.status = EntryStatus::Error;
            e.error = Some("Processing was interrupted. Click Retry to start again.".into());
        }
        changed = true;
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

/// File name prefix of thumbnails rendered with the template (title,
/// speaker and logo drawn over a source image).
pub const RENDERED_PREFIX: &str = "thumbnail_rendered_";

/// A template render of one thumbnail source. `render_key` is computed by
/// the webview from everything that went into it (template version, title,
/// speaker, layout, source), so an unchanged render is reused, not redone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Render {
    pub source: String,
    pub file: String,
    pub render_key: String,
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
    /// Thumbnail sources (generated crops and custom ones). The file name is
    /// the source's id. Never holds renders.
    #[serde(default)]
    pub thumbnail_options: Vec<String>,
    /// The chosen thumbnail as a file, derived from `selected_source` and
    /// `renders` on every load and save. Kept for v0.4.0, which reads it.
    #[serde(default)]
    pub selected_thumbnail: Option<String>,
    #[serde(default)]
    pub selected_source: Option<String>,
    #[serde(default)]
    pub renders: Vec<Render>,
    #[serde(default)]
    pub review: Review,
}

impl VideoData {
    pub fn render_for(&self, source: &str) -> Option<&Render> {
        self.renders.iter().find(|r| r.source == source)
    }

    /// The selected source, or the first one.
    pub fn selected_or_first(&self) -> Option<&String> {
        self.selected_source.as_ref().or(self.thumbnail_options.first())
    }

    /// Make the data consistent: renders out of the options (v0.4.0 stacked
    /// them there on every reopen), one render per existing source, a
    /// selection that names a source, and `selected_thumbnail` derived.
    /// In memory only; `save_video_data` deletes the files dropped here.
    pub fn fix_up(&mut self, dir: &Path) {
        let mut seen = HashSet::new();
        self.thumbnail_options
            .retain(|f| is_valid_file_name(f) && !f.starts_with(RENDERED_PREFIX) && seen.insert(f.clone()));

        let options: HashSet<&String> = self.thumbnail_options.iter().collect();
        let mut renders: Vec<Render> = Vec::new();
        for r in self.renders.drain(..) {
            if options.contains(&r.source) && is_valid_file_name(&r.file) && r.file.starts_with(RENDERED_PREFIX) {
                renders.retain(|x| x.source != r.source);
                renders.push(r);
            }
        }
        self.renders = renders;

        if self.selected_source.as_ref().is_some_and(|s| !options.contains(s)) {
            self.selected_source = None;
        }
        if self.selected_source.is_none() {
            // Written by v0.4.0: its selection is a file name, which may be a
            // source (kept) or a render of an unknown source (dropped).
            self.selected_source = self
                .selected_thumbnail
                .as_ref()
                .filter(|s| options.contains(s))
                .or(self.thumbnail_options.first())
                .cloned();
        }
        self.selected_thumbnail = self.derived_selection(dir);
    }

    /// The file a v0.4.0 reader (and the library card) should show: the
    /// selected source's render if there is one, else the source.
    fn derived_selection(&self, dir: &Path) -> Option<String> {
        let source = self.selected_or_first()?;
        match self.render_for(source) {
            Some(r) if dir.join(&r.file).is_file() => Some(r.file.clone()),
            _ => Some(source.clone()),
        }
    }
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
    if data.selected_thumbnail.as_deref().is_some_and(|f| !is_valid_file_name(f)) {
        data.selected_thumbnail = None;
    }
    data.fix_up(dir);
    truncate(&mut data.review.title);
    truncate(&mut data.review.preacher);
    if data.review.description.as_ref().is_some_and(|d| d.len() > 20_000) {
        data.review.description = None;
    }
    data
}

/// Save video.json, then delete rendered thumbnails it no longer refers to
/// (replaced renders, and the ones v0.4.0 stacked up).
pub fn save_video_data(dir: &Path, data: &VideoData) -> Result<(), String> {
    let mut data = data.clone();
    data.fix_up(dir);
    fsutil::write_json_atomic(&dir.join("video.json"), &data)?;
    let keep: HashSet<&str> = data.renders.iter().map(|r| r.file.as_str()).collect();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(RENDERED_PREFIX) && !keep.contains(name.as_str()) && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
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
    fn unreadable_manifest_is_copied_when_it_cant_be_moved() {
        let (_t, paths) = lib();
        let path = paths.manifest();
        std::fs::write(&path, b"{ not json").unwrap();
        let aside = paths.data_dir().join("library.json.corrupt-x");
        let refuse = |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let Loaded::Corrupt(_, name) = set_aside_to(&path, &aside, "bad", refuse) else { panic!() };
        assert_eq!(name, "library.json.corrupt-x");
        assert_eq!(std::fs::read(&aside).unwrap(), b"{ not json");
    }

    #[test]
    fn unreadable_manifest_that_cant_be_kept_opens_read_only() {
        let (_t, paths) = lib();
        let path = paths.manifest();
        std::fs::write(&path, b"{ not json").unwrap();
        // A folder sits where the copy would go: neither a move nor a copy works.
        let aside = paths.data_dir().join("library.json.corrupt-x");
        std::fs::create_dir(&aside).unwrap();
        std::fs::write(aside.join("keep"), b"x").unwrap();
        let Loaded::Stuck(why) = set_aside_to(&path, &aside, "bad", |a, b| std::fs::rename(a, b)) else { panic!() };
        assert!(why.contains("read-only"), "{why}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json", "left in place");
    }

    #[test]
    fn newer_version_is_read_only() {
        let (_t, paths) = lib();
        std::fs::write(paths.manifest(), br#"{"version": 99, "entries": [], "future": true}"#).unwrap();
        let Loaded::Newer(_, wanted) = load(&paths).unwrap() else { panic!() };
        assert_eq!(wanted, None);
        // Untouched.
        assert!(std::fs::read_to_string(paths.manifest()).unwrap().contains("future"));
    }

    #[test]
    fn newer_schema_is_read_only_and_older_schema_upgrades() {
        let (_t, paths) = lib();
        std::fs::write(paths.manifest(), br#"{"version": 1, "schema_version": 3, "min_app_version": "0.9.0", "entries": []}"#).unwrap();
        let Loaded::Newer(_, wanted) = load(&paths).unwrap() else { panic!() };
        assert_eq!(wanted.as_deref(), Some("0.9.0"));

        // What v0.4.0 writes: no schema_version. Saved back as schema 2,
        // still format 1 so v0.4.0 can open it.
        std::fs::write(paths.manifest(), br#"{"version": 1, "entries": []}"#).unwrap();
        let Loaded::Ok(m) = load(&paths).unwrap() else { panic!() };
        save(&paths, &m).unwrap();
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(paths.manifest()).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        assert_eq!(raw["schema_version"], SCHEMA_VERSION);
        assert_eq!(raw["min_app_version"], MIN_APP_VERSION);
    }

    #[test]
    fn interrupted_jobs_become_errors() {
        let mut m = Manifest::default();
        let mut e = entry("a.mp4");
        e.status = EntryStatus::Processing;
        m.entries.push(e);
        m.entries.push(entry("b.mp4"));
        assert!(mark_interrupted(&mut m, None, |_| false));
        assert_eq!(m.entries[0].status, EntryStatus::Error);
        assert_eq!(m.entries[1].status, EntryStatus::New);

        // A re-process keeps the earlier results; a live job is left alone.
        m.entries[0].status = EntryStatus::Processing;
        m.entries[1].status = EntryStatus::Processing;
        let live = m.entries[1].id.clone();
        assert!(mark_interrupted(&mut m, Some(&live), |_| true));
        assert_eq!(m.entries[0].status, EntryStatus::ReadyForReview);
        assert!(m.entries[0].error.as_deref().unwrap().contains("earlier results"));
        assert_eq!(m.entries[1].status, EntryStatus::Processing);
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
        // The bad selection is dropped; the first source is used instead.
        assert_eq!(d.selected_source.as_deref(), Some("a.jpg"));
        assert_eq!(d.selected_thumbnail.as_deref(), Some("a.jpg"));
    }

    #[test]
    fn stacked_renders_heal() {
        // What v0.4.0 left after a few reopens: every render pass inserted
        // at the front of the options, and a render selected.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let stacked = [
            "thumbnail_rendered_1_200.jpg",
            "thumbnail_rendered_2_200.jpg",
            "thumbnail_rendered_1_100.jpg",
            "thumbnail_option_1.jpg",
            "thumbnail_option_2.jpg",
        ];
        for f in stacked {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let json = serde_json::json!({
            "thumbnail_options": stacked,
            "selected_thumbnail": "thumbnail_rendered_1_200.jpg",
            "review": {"title": "T"},
        });
        std::fs::write(dir.join("video.json"), json.to_string()).unwrap();

        let d = load_video_data(dir);
        assert_eq!(d.thumbnail_options, vec!["thumbnail_option_1.jpg", "thumbnail_option_2.jpg"]);
        assert!(d.renders.is_empty());
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        assert_eq!(d.selected_thumbnail.as_deref(), Some("thumbnail_option_1.jpg"));
        assert_eq!(d.review.title.as_deref(), Some("T"));

        // Saving deletes the stacked files, and the next load is stable.
        save_video_data(dir, &d).unwrap();
        for f in &stacked[..3] {
            assert!(!dir.join(f).exists(), "{f}");
        }
        assert!(dir.join("thumbnail_option_1.jpg").exists());
        let again = load_video_data(dir);
        assert_eq!(again.thumbnail_options.len(), 2);
    }

    #[test]
    fn v040_selection_of_a_source_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let json = serde_json::json!({
            "thumbnail_options": ["thumbnail_option_1.jpg", "thumbnail_option_2.jpg"],
            "selected_thumbnail": "thumbnail_option_2.jpg",
        });
        std::fs::write(tmp.path().join("video.json"), json.to_string()).unwrap();
        let d = load_video_data(tmp.path());
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
    }

    #[test]
    fn selection_derives_from_render() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("thumbnail_rendered_abc.jpg"), b"x").unwrap();
        let mut d = VideoData {
            thumbnail_options: vec!["a.jpg".into(), "b.jpg".into()],
            selected_source: Some("b.jpg".into()),
            renders: vec![
                Render { source: "b.jpg".into(), file: "thumbnail_rendered_abc.jpg".into(), render_key: "k".into() },
                // Its source is gone: dropped.
                Render { source: "gone.jpg".into(), file: "thumbnail_rendered_x.jpg".into(), render_key: "k".into() },
            ],
            ..Default::default()
        };
        d.fix_up(dir);
        assert_eq!(d.renders.len(), 1);
        assert_eq!(d.selected_thumbnail.as_deref(), Some("thumbnail_rendered_abc.jpg"));
        // A render whose file is missing falls back to the source.
        std::fs::remove_file(dir.join("thumbnail_rendered_abc.jpg")).unwrap();
        d.fix_up(dir);
        assert_eq!(d.selected_thumbnail.as_deref(), Some("b.jpg"));
    }
}
