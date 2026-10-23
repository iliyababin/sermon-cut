//! library.json and the per-video JSON files, with validation on load.

use super::fsutil::{self, ReadJsonError};
use super::{is_valid_file_name, is_valid_id, Entry, EntryStatus, LibraryPaths, FORMAT_VERSION, MIN_APP_VERSION, SCHEMA_VERSION};
use crate::state::TranscriptionResult;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// Ids waiting to be processed and the one being processed, in order,
    /// so the queue survives a restart. An id leaves when its job ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<String>,
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
            queue: Vec::new(),
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
    /// Unreadable; it was moved aside to the given name. The list comes from
    /// library.json.bak when `restored`, else it starts empty.
    Corrupt(Manifest, String, bool),
    /// library.json was missing but videos/ has content; restored from
    /// library.json.bak.
    Restored(Manifest),
    /// library.json was missing and there was no usable backup; the list
    /// starts empty (scans adopt the orphaned video folders).
    Lost(Manifest),
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
    let mut queued = HashSet::new();
    manifest.queue.retain(|id| is_valid_id(id) && queued.insert(id.clone()));
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
        Err(ReadJsonError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(recover_missing(paths)),
        Err(ReadJsonError::Io(e)) => return Err(format!("Can't read {}: {}", path.display(), e)),
        Err(e) => return Ok(with_backup(paths, set_aside(&path, &e.to_string()))),
    };
    match parse(&bytes) {
        Ok(loaded) => Ok(loaded),
        Err(e) => Ok(with_backup(paths, set_aside(&path, &e))),
    }
}

/// The backup, if it can be read and written back by this version.
fn read_backup(paths: &LibraryPaths) -> Option<Manifest> {
    let bytes = fsutil::read_limited(&paths.manifest_backup(), MAX_MANIFEST_BYTES).ok()?;
    match parse(&bytes) {
        Ok(Loaded::Ok(m)) => Some(m),
        _ => None,
    }
}

/// After an unreadable library.json was set aside: the backup's list.
fn with_backup(paths: &LibraryPaths, loaded: Loaded) -> Loaded {
    match loaded {
        Loaded::Corrupt(_, aside, _) => match read_backup(paths) {
            Some(m) => Loaded::Corrupt(m, aside, true),
            None => Loaded::Corrupt(Manifest::default(), aside, false),
        },
        other => other,
    }
}

/// No library.json. A new library has nothing in videos/; otherwise the
/// file was lost (deleted, or not synced) and the list is recovered.
fn recover_missing(paths: &LibraryPaths) -> Loaded {
    let has_videos = std::fs::read_dir(paths.videos_dir()).is_ok_and(|mut d| d.next().is_some());
    if !has_videos {
        return Loaded::Ok(Manifest::default());
    }
    match read_backup(paths) {
        Some(m) => Loaded::Restored(m),
        None => Loaded::Lost(Manifest::default()),
    }
}

fn parse(bytes: &[u8]) -> Result<Loaded, String> {
    if let Ok(v) = serde_json::from_slice::<VersionOnly>(bytes) {
        if v.version > FORMAT_VERSION || v.schema_version.is_some_and(|s| s > SCHEMA_VERSION) {
            // Show what we can understand, but never rewrite it.
            let mut manifest = serde_json::from_slice::<Manifest>(bytes).unwrap_or_default();
            sanitize(&mut manifest);
            let wanted = v.min_app_version.filter(|m| m.len() <= 32 && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'));
            return Ok(Loaded::Newer(manifest, wanted));
        }
    }

    let mut manifest = serde_json::from_slice::<Manifest>(bytes).map_err(|e| e.to_string())?;
    sanitize(&mut manifest);
    manifest.version = FORMAT_VERSION;
    manifest.schema_version = SCHEMA_VERSION;
    manifest.min_app_version = Some(MIN_APP_VERSION.to_string());
    Ok(Loaded::Ok(manifest))
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
            false,
        ),
        Err(e) => Loaded::Stuck(format!(
            "The library's tracking file (.sermon-cut/library.json) can't be read, and Sermon Cut couldn't keep a copy of it ({}). The library is read-only so nothing is lost; move that file somewhere safe, then reopen the library.",
            e
        )),
    }
}

/// Write library.json, first keeping the current one as library.json.bak,
/// but only if it can be read: a damaged file never replaces a good backup.
pub fn save(paths: &LibraryPaths, manifest: &Manifest) -> Result<(), String> {
    if let Ok(current) = fsutil::read_limited(&paths.manifest(), MAX_MANIFEST_BYTES) {
        if serde_json::from_slice::<Manifest>(&current).is_ok() {
            if let Err(e) = fsutil::write_atomic(&paths.manifest_backup(), &current) {
                eprintln!("[library] Failed to update library.json.bak: {}", e);
            }
        }
    }
    fsutil::write_json_atomic(&paths.manifest(), manifest)
}

/// Whether a video folder holds finished processing results.
pub fn has_results(dir: &Path) -> bool {
    let data = load_video_data(dir);
    !data.thumbnail_options.is_empty() && data.trimmed_file.is_some_and(|t| dir.join(t).is_file())
}

/// A video folder no entry refers to (its library.json entry was lost),
/// waiting for its video to show up in a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct Orphan {
    pub id: String,
    pub has_results: bool,
}

/// Video folders without an entry, by the file hash in their
/// transcript.json. Folders without a transcript can't be matched.
pub fn find_orphans(paths: &LibraryPaths, manifest: &Manifest) -> HashMap<String, Orphan> {
    let known: HashSet<&str> = manifest.entries.iter().map(|e| e.id.as_str()).collect();
    let mut orphans = HashMap::new();
    let videos = paths.videos_dir();
    if !std::fs::symlink_metadata(&videos).is_ok_and(|m| m.is_dir()) {
        return orphans;
    }
    let Ok(dirs) = std::fs::read_dir(&videos) else { return orphans };
    for d in dirs.flatten() {
        let id = d.file_name().to_string_lossy().to_string();
        if !is_valid_id(&id) || known.contains(id.as_str()) || !d.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = d.path();
        if let Some(t) = load_transcript(&dir) {
            orphans.entry(t.file_hash).or_insert(Orphan { has_results: has_results(&dir), id });
        }
    }
    orphans
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
    /// The thumbnail title on one line instead of balanced over two.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub single_line: bool,
}

/// The key of a template render made from these inputs; the same as the
/// webview's `renderKey` (src/components/review/renderKey.ts): cyrb53 over
/// the inputs as a JSON array, as hex. A shared test vector keeps them in
/// step.
pub fn render_key(template_version: u32, title: &str, preacher: &str, single_line: bool, source: &str, version: &str) -> String {
    let json = serde_json::to_string(&(template_version, title, js_trim(preacher), single_line, source, version))
        .unwrap_or_default();
    cyrb53(&json)
}

/// JavaScript's `String.prototype.trim`, which differs from Rust's in two
/// characters: it strips U+FEFF but not U+0085.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// cyrb53 over the string's UTF-16 code units, as JavaScript computes it.
fn cyrb53(s: &str) -> String {
    let mut h1: u32 = 0xdeadbeef;
    let mut h2: u32 = 0x41c6ce57;
    for ch in s.encode_utf16() {
        h1 = (h1 ^ ch as u32).wrapping_mul(2654435761);
        h2 = (h2 ^ ch as u32).wrapping_mul(1597334677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2246822507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2246822507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3266489909);
    let n = 4294967296u64 * (2097151 & h2) as u64 + h1 as u64;
    format!("{:x}", n)
}

/// File name prefix of thumbnails rendered with the template (title,
/// speaker and logo drawn over a source image).
pub const RENDERED_PREFIX: &str = "thumbnail_rendered_";

/// A new render's file name. It carries the source's name (without the
/// extension) and a fresh id, so the source can be told from the name
/// alone: v0.4.0 rewrites video.json keeping only `selected_thumbnail`.
pub fn render_file_name(source: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let named = format!("{}{}_{}.jpg", RENDERED_PREFIX, file_stem(source), id);
    if named.len() <= 255 {
        named
    } else {
        format!("{}{}.jpg", RENDERED_PREFIX, id)
    }
}

fn file_stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// The one source among `options` that a render's file name (see
/// `render_file_name`) says it was made from.
fn render_source<'a>(file: &str, options: &'a [String]) -> Option<&'a String> {
    let rest = file.strip_prefix(RENDERED_PREFIX)?.strip_suffix(".jpg")?;
    let (stem, id) = rest.rsplit_once('_')?;
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut matching = options.iter().filter(|o| file_stem(o) == stem);
    let source = matching.next()?;
    matching.next().is_none().then_some(source)
}

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
    /// In memory only; `prune_renders` deletes the files dropped here.
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
            // Written by v0.4.0, which keeps only `selected_thumbnail`: a
            // source, or a render whose name says its source (which then
            // stays that source's render). Else the first source.
            let chosen = self.selected_thumbnail.as_ref().and_then(|s| match options.contains(s) {
                true => Some(s.clone()),
                false => render_source(s, &self.thumbnail_options).cloned(),
            });
            if let (Some(source), Some(file)) = (&chosen, &self.selected_thumbnail) {
                if file != source && self.render_for(source).is_none() && dir.join(file).is_file() {
                    // No key: the review screen renders it again when it next
                    // shows it, and keeps this one until then.
                    self.renders.push(Render { source: source.clone(), file: file.clone(), render_key: String::new() });
                }
            }
            self.selected_source = chosen.or(self.thumbnail_options.first().cloned());
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

/// Save video.json. Only writes: a render being written, or one renamed
/// into place but not yet recorded, must survive a save that holds an older
/// list. Files the data no longer refers to are removed by `prune_renders`.
pub fn save_video_data(dir: &Path, data: &VideoData) -> Result<(), String> {
    let mut data = data.clone();
    data.fix_up(dir);
    fsutil::write_json_atomic(&dir.join("video.json"), &data)
}

/// Where a render is written before it's recorded: hidden, so no video.json
/// can name it and `prune_renders` never sees it; swept on the next open.
pub const RENDER_TMP_PREFIX: &str = ".render-tmp-";

/// A finished render's file name (see `render_file_name`; v0.4.0's
/// `thumbnail_rendered_<n>_<time>.jpg` too).
pub fn is_render_file(name: &str) -> bool {
    name.starts_with(RENDERED_PREFIX) && name.ends_with(".jpg") && is_valid_file_name(name) && !name.contains(fsutil::TMP_MARKER)
}

/// Delete rendered thumbnails `data` doesn't record (replaced renders,
/// renders of removed sources, the ones v0.4.0 stacked up). Run it under
/// the video's lock, with the data just loaded or saved there, so a render
/// recorded meanwhile can't be taken. Returns what it deleted.
pub fn prune_renders(dir: &Path, data: &VideoData) -> Vec<String> {
    let mut data = data.clone();
    data.fix_up(dir);
    let keep: HashSet<&str> = data.renders.iter().map(|r| r.file.as_str()).collect();
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return removed };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if is_render_file(&name) && !keep.contains(name.as_str()) && e.file_type().is_ok_and(|t| t.is_file()) {
            if std::fs::remove_file(e.path()).is_ok() {
                removed.push(name);
            }
        }
    }
    removed
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
        let Loaded::Corrupt(m, aside, false) = load(&paths).unwrap() else { panic!() };
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
        let Loaded::Corrupt(_, name, _) = set_aside_to(&path, &aside, "bad", refuse) else { panic!() };
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

    fn ids(m: &Manifest) -> Vec<String> {
        m.entries.iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn missing_manifest_is_restored_from_backup() {
        let (_t, paths) = lib();
        let mut m = Manifest::default();
        m.entries.push(entry("a.mp4"));
        save(&paths, &m).unwrap();
        m.entries.push(entry("b.mp4"));
        save(&paths, &m).unwrap();
        // The backup is the save before.
        assert_eq!(read_backup(&paths).map(|b| b.entries.len()), Some(1));

        std::fs::remove_file(paths.manifest()).unwrap();
        // A brand-new library (nothing in videos/) just starts empty.
        let Loaded::Ok(empty) = load(&paths).unwrap() else { panic!() };
        assert!(empty.entries.is_empty());

        paths.ensure_video_dir(&m.entries[0].id).unwrap();
        let Loaded::Restored(back) = load(&paths).unwrap() else { panic!() };
        assert_eq!(ids(&back), ids(&m)[..1]);

        // No usable backup either: rebuilt.
        std::fs::write(paths.manifest_backup(), b"junk").unwrap();
        assert!(matches!(load(&paths).unwrap(), Loaded::Lost(_)));
    }

    #[test]
    fn corrupt_manifest_never_replaces_a_good_backup() {
        let (_t, paths) = lib();
        let mut m = Manifest::default();
        m.entries.push(entry("a.mp4"));
        save(&paths, &m).unwrap();
        save(&paths, &m).unwrap();
        std::fs::write(paths.manifest(), b"{ half writ").unwrap();
        let mut newer = m.clone();
        newer.entries.push(entry("b.mp4"));
        save(&paths, &newer).unwrap();
        assert_eq!(read_backup(&paths).map(|b| ids(&b)), Some(ids(&m)));

        // And an unreadable file on open is set aside, with the backup's list.
        std::fs::write(paths.manifest(), b"{ half writ").unwrap();
        let Loaded::Corrupt(back, _, true) = load(&paths).unwrap() else { panic!() };
        assert_eq!(ids(&back), ids(&m));
    }

    #[test]
    fn orphaned_video_folders_are_found_by_transcript_hash() {
        let (_t, paths) = lib();
        let mut m = Manifest::default();
        m.entries.push(entry("known.mp4"));
        let known_dir = paths.ensure_video_dir(&m.entries[0].id).unwrap();
        let orphan = new_id();
        let dir = paths.ensure_video_dir(&orphan).unwrap();
        // A folder without a transcript can't be matched to anything.
        paths.ensure_video_dir(&new_id()).unwrap();
        let transcript = |hash: &str| TranscriptFile {
            file_hash: hash.into(),
            transcription: serde_json::from_value(serde_json::json!({"segments": [], "full_text": ""})).unwrap(),
        };
        save_transcript(&known_dir, &transcript("k")).unwrap();
        save_transcript(&dir, &transcript("h1")).unwrap();
        std::fs::write(dir.join("trimmed.mp4"), b"t").unwrap();
        std::fs::write(dir.join("thumbnail_option_1.jpg"), b"j").unwrap();
        save_video_data(&dir, &VideoData {
            trimmed_file: Some("trimmed.mp4".into()),
            thumbnail_options: vec!["thumbnail_option_1.jpg".into()],
            ..Default::default()
        })
        .unwrap();

        let found = find_orphans(&paths, &m);
        assert_eq!(found.len(), 1);
        assert_eq!(found["h1"], Orphan { id: orphan, has_results: true });
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

        // Saving keeps every file; pruning deletes the stacked ones, and the
        // next load is stable.
        save_video_data(dir, &d).unwrap();
        assert!(dir.join(stacked[0]).exists());
        prune_renders(dir, &d);
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
    fn v040_selection_of_a_render_keeps_its_source_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let options = ["thumbnail_option_1.jpg", "thumbnail_option_2.jpg", "custom_thumbnail_7.jpg"];
        for f in options {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let render = render_file_name("thumbnail_option_2.jpg");
        assert!(render.starts_with("thumbnail_rendered_thumbnail_option_2_"), "{render}");
        std::fs::write(dir.join(&render), b"r").unwrap();
        // How v0.4.0 writes video.json back: no renders, no selected_source.
        let json = serde_json::json!({"thumbnail_options": options, "selected_thumbnail": render});
        std::fs::write(dir.join("video.json"), json.to_string()).unwrap();

        let d = load_video_data(dir);
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        assert_eq!(d.render_for("thumbnail_option_2.jpg").map(|r| r.file.as_str()), Some(render.as_str()));
        assert_eq!(d.selected_thumbnail.as_deref(), Some(render.as_str()));
        save_video_data(dir, &d).unwrap();
        prune_renders(dir, &d);
        assert!(dir.join(&render).exists(), "the selected render is kept");
        let again = load_video_data(dir);
        assert_eq!(again.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));

        // A custom source's render too.
        let custom = render_file_name("custom_thumbnail_7.jpg");
        std::fs::write(dir.join(&custom), b"r").unwrap();
        let json = serde_json::json!({"thumbnail_options": options, "selected_thumbnail": custom});
        std::fs::write(dir.join("video.json"), json.to_string()).unwrap();
        assert_eq!(load_video_data(dir).selected_source.as_deref(), Some("custom_thumbnail_7.jpg"));

        // A render name that names no source: the first one.
        for name in [render_file_name("gone.jpg"), "thumbnail_rendered_1_200.jpg".to_string()] {
            let json = serde_json::json!({"thumbnail_options": options, "selected_thumbnail": name});
            std::fs::write(dir.join("video.json"), json.to_string()).unwrap();
            assert_eq!(load_video_data(dir).selected_source.as_deref(), Some("thumbnail_option_1.jpg"), "{name}");
        }
    }

    #[test]
    fn a_save_never_deletes_a_render_in_progress_or_not_yet_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        let older = VideoData { thumbnail_options: vec!["a.jpg".into()], ..Default::default() };
        save_video_data(dir, &older).unwrap();

        // A render being written, and one just renamed into place whose
        // record is still to come, while a save holding the older list runs.
        let tmp_name = format!("{}{}", RENDER_TMP_PREFIX, render_file_name("a.jpg"));
        std::fs::write(dir.join(&tmp_name), b"half").unwrap();
        let fresh = render_file_name("a.jpg");
        std::fs::write(dir.join(&fresh), b"r").unwrap();
        save_video_data(dir, &older).unwrap();
        assert!(dir.join(&tmp_name).exists());
        assert!(dir.join(&fresh).exists());

        // Recorded, then pruned with the current data: kept. The temp file
        // is never a render to prune.
        let mut now = load_video_data(dir);
        now.renders.push(Render { source: "a.jpg".into(), file: fresh.clone(), render_key: "k".into() });
        save_video_data(dir, &now).unwrap();
        assert!(prune_renders(dir, &load_video_data(dir)).is_empty());
        assert!(dir.join(&fresh).exists() && dir.join(&tmp_name).exists());

        // Replaced: the old one goes.
        let newer = render_file_name("a.jpg");
        std::fs::write(dir.join(&newer), b"r2").unwrap();
        now.renders = vec![Render { source: "a.jpg".into(), file: newer.clone(), render_key: "k2".into() }];
        save_video_data(dir, &now).unwrap();
        assert_eq!(prune_renders(dir, &load_video_data(dir)), vec![fresh.clone()]);
        assert!(dir.join(&newer).exists());
    }

    #[test]
    fn render_key_matches_the_webview() {
        // The same vector is in src/components/review/renderKey.test.ts.
        let key = render_key(4, "Héllo \"world\"\n\t😀 \\ </>", "\u{a0}Pastor Ü\u{feff} ", true, "custom_thumbnail_1.jpg", "1730000000000");
        assert_eq!(key, "1a4ef5393622d8");
        assert_eq!(js_trim("\u{85}x\u{feff}"), "\u{85}x");
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
