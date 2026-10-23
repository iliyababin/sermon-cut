//! The open library: state, scanning, the processing queue and actions.
//!
//! Locking: `inner` is a std mutex held only for short, non-async work.
//! Slow I/O (hashing, ffprobe, ffmpeg, uploads) happens outside it.

use super::fsutil::{self, TMP_MARKER};
use super::lock::{self, Acquire};
use super::manifest::{self, Loaded, Manifest, Orphan, Render, Review, TranscriptFile, VideoData};
use super::scan::{self, FileObs, ObsState, Reconciled, ScanCache};
use super::{Entry, EntryStatus, Fingerprint, LibraryPaths, UploadRecord, MAX_TRANSCRIBE_SECS};
use crate::state::{AppState, ProcessingStage, TranscriptionResult};
use crate::upload::{SessionStore, UploadOutcome, UploadSession, UploadWarning, WarningKind};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

pub type AppStateHandle = Arc<tokio::sync::Mutex<AppState>>;

const SWITCH_BUSY: &str = "Wait for processing, downloads and uploads to finish before changing the library folder.";

const UPDATING_LIBRARY: &str = "Updating library…";
/// While an app update installs.
const UPDATING: &str = "Sermon Cut is updating… It will restart in a moment.";

/// The entry went while its files were being worked on (the library was
/// reopened, e.g. after another computer let go of it).
const ENTRY_GONE: &str = "This video was removed from the library meanwhile.";

const FILE_CHANGED: &str =
    "The file changed since it was added. If it's still copying, wait for it to finish, then try again.";

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Writable,
    /// Shown, but nothing is written: newer format, the folder can't be
    /// written, or the migration is running. The message says which.
    ReadOnly(String),
    /// Another computer (named) has it open: shown read-only until it lets go.
    Locked(String),
    /// Folder missing or unreadable (drive unplugged). Last-known list shown.
    Unavailable(String),
}

impl Mode {
    /// This session holds lock.json: writable, or read-only only while the
    /// pre-0.4 migration runs (which writes).
    fn holds_lock(&self) -> bool {
        match self {
            Mode::Writable => true,
            Mode::ReadOnly(why) => why == UPDATING_LIBRARY,
            Mode::Locked(_) | Mode::Unavailable(_) => false,
        }
    }

    /// What to tell the user about a library that can't be written now.
    fn reason(&self) -> Option<String> {
        match self {
            Mode::Writable => None,
            Mode::Locked(host) => Some(format!(
                "This library is open on {}. It's read-only here so the two computers don't overwrite each other's changes.",
                host
            )),
            Mode::ReadOnly(why) | Mode::Unavailable(why) => Some(why.clone()),
        }
    }
}

struct Running {
    id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
    cancel: CancellationToken,
    previous: EntryStatus,
    /// Its outputs are being swapped in. A commit always runs to the end:
    /// a cancel now only sets `late_cancel`.
    committing: bool,
    late_cancel: bool,
}

/// What a cancel that came during the commit says.
pub const LATE_CANCEL: &str = "Cancel came too late — processing finished; your earlier review edits were kept.";

/// A job's work folder, unique to one run (so nothing left running by a
/// cancelled run can write into the next one's), deleted when the job ends
/// however it ends.
struct StagingDir(PathBuf);

impl StagingDir {
    fn create(paths: &LibraryPaths, id: &str) -> Result<Self, String> {
        paths.ensure_dir(&paths.tmp_dir())?;
        let dir = paths.tmp_dir().join(format!("job-{}-{}", id, &super::new_id()[..8]));
        std::fs::create_dir(&dir).map_err(|e| format!("Failed to create work folder: {}", e))?;
        Ok(StagingDir(dir))
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadItem {
    pub job_id: String,
    pub youtube_id: String,
    pub title: String,
    #[serde(skip)]
    cancel: Option<tokio::sync::watch::Sender<bool>>,
}

struct ActiveUpload {
    percent: u32,
    cancel: CancellationToken,
}

struct Inner {
    configured: String,
    paths: Option<LibraryPaths>,
    manifest: Manifest,
    mode: Mode,
    cache: ScanCache,
    last: Reconciled,
    scanned: bool,
    queue: VecDeque<String>,
    running: Option<Running>,
    /// Entries with an upload, trash or thumbnail operation in flight.
    busy: HashSet<String>,
    /// Template renders being written (see `save_rendered_thumbnail`); the
    /// open-time sweep would delete their temp files.
    rendering: usize,
    /// YouTube uploads in flight.
    uploads: std::collections::HashMap<String, ActiveUpload>,
    downloads: Vec<DownloadItem>,
    session: String,
    notice: Option<String>,
    /// Video folders whose entries were lost, by file hash; a scan that
    /// finds their video gives the new entry the folder back.
    orphans: std::collections::HashMap<String, Orphan>,
    /// An app update is being installed: no new work may start, since the
    /// app relaunches when it's done.
    updating: bool,
    /// The next scan takes in a folder's existing videos (a switched or
    /// rebuilt library), which auto-process must leave alone: the entries
    /// it adds count as already there (`first_seen` 0).
    baseline_scan: bool,
}

pub struct Library {
    inner: Mutex<Inner>,
    scan_lock: tokio::sync::Mutex<()>,
    /// Held while lock.json is written or the folder it's in changes:
    /// heartbeat, open (switch, reopen, take over, clear data) and release.
    /// Taken after `scan_lock`, before anything else (docs/INVARIANTS.md).
    /// The heartbeat takes only this one, so a long scan or migration
    /// can't starve it.
    lock_file_mutex: tokio::sync::Mutex<()>,
    /// One lock per video folder, held around a read-modify-write of its
    /// video.json and any pruning of its files. Taken before `inner`, never
    /// across an `.await` (see docs/INVARIANTS.md). Use `video_lock`.
    video_locks: Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>,
    wake: tokio::sync::Notify,
    /// The pre-0.4 migration ran this launch.
    migration_tried: std::sync::atomic::AtomicBool,
}

// ---------------------------------------------------------------------------
// What the frontend sees

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ViewStatus {
    Copying,
    Unreadable,
    New,
    TooLong,
    Queued,
    Processing,
    Uploading,
    ReadyForReview,
    Done,
    Error,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryVideo {
    pub id: String,
    pub file_name: String,
    pub file_path: String,
    pub title: String,
    pub status: ViewStatus,
    pub error: Option<String>,
    pub duration: Option<f64>,
    pub size: u64,
    pub channel: Option<String>,
    pub source_url: Option<String>,
    pub hidden: bool,
    pub uploads: Vec<UploadRecord>,
    pub added_at: String,
    /// Selected (or first) thumbnail, for the card.
    pub thumbnail: Option<String>,
    pub processing_stage: Option<ProcessingStage>,
    pub processing_progress: Option<f32>,
    pub processing_message: Option<String>,
    /// Percent of a YouTube upload in progress.
    pub upload_progress: Option<u32>,
    /// Whether Hide/Trash/Reset are allowed right now.
    pub busy: bool,
    /// For a queued video, its place in line ("2 of 3"), counting the
    /// video being processed now as the first.
    pub queue_position: Option<usize>,
    pub queue_length: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryView {
    pub folder: String,
    pub available: bool,
    pub writable: bool,
    /// Why the library is read-only or unavailable, or other one-off news.
    pub message: Option<String>,
    pub locked_by: Option<String>,
    pub tool_problem: Option<String>,
    pub videos: Vec<LibraryVideo>,
    pub downloads: Vec<DownloadItem>,
    pub notices: Vec<String>,
    pub max_transcribe_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoDetail {
    #[serde(flatten)]
    pub video: LibraryVideo,
    pub transcription: Option<TranscriptionResult>,
    pub trimmed_path: Option<String>,
    pub trim_start: Option<f64>,
    pub trim_end: Option<f64>,
    pub thumbnail_options: Vec<ThumbnailOption>,
    /// Stored template renders whose files exist, one per source at most.
    pub renders: Vec<RenderView>,
    pub selected_source: Option<String>,
    /// The selected render, or its source (absolute path).
    pub selected_thumbnail: Option<String>,
    pub review: Review,
    /// The YouTube URL of another library entry with the same file (same
    /// content hash) that was already uploaded, for the confirm dialog.
    pub duplicate_upload: Option<String>,
}

/// A thumbnail source on the review screen.
#[derive(Debug, Clone, Serialize)]
pub struct ThumbnailOption {
    /// The file name; stable across reopens, so renders and the selection
    /// are keyed by it.
    pub id: String,
    pub path: String,
    /// What the template draws over: the plain crop of a generated option
    /// (the option itself is graded and shrunk), else the option.
    pub render_from: String,
    /// Changes whenever the image is regenerated (its mtime), so it is part
    /// of the render key.
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderView {
    pub source: String,
    pub path: String,
    pub render_key: String,
}

// ---------------------------------------------------------------------------

fn path_string(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
    crate::notify::refresh_badge(app);
}

#[derive(Debug, Clone, Serialize)]
struct ProcessingProgressEvent {
    video_id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
}

fn hold(lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Result<T, String>> {
    let handle = tokio::task::spawn_blocking(f);
    async move { handle.await.map_err(|e| format!("Background task failed: {}", e)) }
}


/// Outcome of a lock heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub enum HeartbeatOutcome {
    Unchanged,
    /// Another computer took the library over; it's read-only here now.
    LostTo(String),
    /// The computer that had it let go; reopen it for writing.
    Regained,
}

impl Inner {
    fn writable(&self) -> Result<&LibraryPaths, String> {
        match (&self.mode, &self.paths) {
            (Mode::Writable, Some(paths)) => Ok(paths),
            (Mode::Writable, None) => Err("The library isn't open".into()),
            (mode, _) => Err(mode.reason().unwrap_or_default()),
        }
    }

    fn save(&self) -> Result<(), String> {
        let paths = self.writable()?;
        manifest::save(paths, &self.manifest)
    }

    /// The second half of a scan: match what `observe` saw to the entries.
    /// Callers hold the scan lock from before the observe until after this.
    /// Returns the replaced entries whose old outputs must be deleted (see
    /// `Library::finish_scan`); they're marked busy until then.
    fn apply_scan(&mut self, obs: &[FileObs]) -> Vec<String> {
        let busy = self.busy_set();
        let last_present = self.scanned.then(|| std::mem::take(&mut self.last.present));
        let known: Option<HashSet<String>> =
            self.baseline_scan.then(|| self.manifest.entries.iter().map(|e| e.id.clone()).collect());
        let reconciled =
            scan::reconcile_adopting(&mut self.manifest.entries, obs, &busy, last_present.as_ref(), &mut self.orphans);
        if let Some(known) = known {
            for e in self.manifest.entries.iter_mut().filter(|e| !known.contains(&e.id)) {
                e.first_seen = 0;
            }
        }
        let mut stale = Vec::new();
        if self.mode == Mode::Writable {
            // Only a saved scan settles the backlog: a read-only one is
            // forgotten when the library is reopened for writing.
            self.baseline_scan = false;
            stale = reconciled.replaced.clone();
            self.busy.extend(stale.iter().cloned());
            if reconciled.changed {
                self.save_logged();
            }
        }
        self.last = reconciled;
        self.scanned = true;
        stale
    }

    fn save_logged(&self) {
        if matches!(self.mode, Mode::Writable) {
            if let Err(e) = self.save() {
                eprintln!("[library] Failed to save library.json: {}", e);
            }
        }
    }

    /// Videos auto-process should queue: new, first seen at or after
    /// `since`, never auto-queued before, and ready to process.
    fn auto_process_candidates(&self, since: i64) -> Vec<String> {
        self.manifest
            .entries
            .iter()
            .filter(|e| {
                e.status == EntryStatus::New
                    && !e.hidden
                    && !e.auto_queued
                    && e.first_seen >= since
                    && e.duration.is_none_or(|d| d <= MAX_TRANSCRIBE_SECS)
                    && self.present(&e.id)
                    && !self.last.copying_ids.contains(&e.id)
                    && !self.last.unreadable_ids.contains(&e.id)
                    && !self.is_busy(&e.id)
            })
            .map(|e| e.id.clone())
            .collect()
    }

    /// Nothing of this session is running, queued or writing: no job,
    /// download, busy video (upload, trash, thumbnail work) or render.
    fn is_idle(&self) -> bool {
        self.running.is_none()
            && self.queue.is_empty()
            && self.downloads.is_empty()
            && self.busy.is_empty()
            && self.rendering == 0
    }

    fn require_not_updating(&self) -> Result<(), String> {
        if self.updating {
            return Err(UPDATING.into());
        }
        Ok(())
    }

    /// What would be lost by quitting now, for the quit warning.
    fn work_in_progress(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.uploads.is_empty() {
            parts.push(match self.uploads.len() {
                1 => "a YouTube upload".to_string(),
                n => format!("{} YouTube uploads", n),
            });
        }
        if self.running.is_some() || !self.queue.is_empty() {
            parts.push("video processing".to_string());
        }
        if !self.downloads.is_empty() {
            parts.push("a download".to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" and "))
    }

    /// The newest upload of another entry for the same file content.
    fn duplicate_upload(&self, id: &str, hash: &str) -> Option<String> {
        if hash.is_empty() {
            return None;
        }
        self.manifest
            .entries
            .iter()
            .filter(|o| o.id != id && o.fingerprint.hash == hash)
            .flat_map(|o| o.uploads.iter())
            .max_by(|a, b| a.uploaded_at.cmp(&b.uploaded_at))
            .map(|u| u.url.clone())
    }

    fn is_busy(&self, id: &str) -> bool {
        self.busy.contains(id)
            || self.queue.iter().any(|q| q == id)
            || self.running.as_ref().is_some_and(|r| r.id == id)
    }

    fn busy_set(&self) -> HashSet<String> {
        let mut set = self.busy.clone();
        set.extend(self.queue.iter().cloned());
        if let Some(r) = &self.running {
            set.insert(r.id.clone());
        }
        set
    }

    fn entry(&self, id: &str) -> Result<&Entry, String> {
        self.manifest.get(id).ok_or_else(|| "That video is no longer in the library".to_string())
    }

    fn entry_mut(&mut self, id: &str) -> Result<&mut Entry, String> {
        self.manifest
            .get_mut(id)
            .ok_or_else(|| "That video is no longer in the library".to_string())
    }

    fn present(&self, id: &str) -> bool {
        self.last.present.contains(id)
    }

    /// Template renders replace nothing processing makes, so they may run
    /// during an upload, but not while the video is (about to be) processed
    /// or its thumbnails are being regenerated.
    fn require_render_allowed(&self, id: &str) -> Result<(), String> {
        if self.queue.iter().any(|q| q == id) || self.running.as_ref().is_some_and(|r| r.id == id) {
            return Err("This video is being processed".into());
        }
        if self.busy.contains(id) && !self.uploads.contains_key(id) {
            return Err("This video is busy. Wait for it to finish, or cancel it first.".into());
        }
        Ok(())
    }

    fn require_idle(&self, id: &str) -> Result<(), String> {
        if self.is_busy(id) {
            return Err("This video is busy. Wait for it to finish, or cancel it first.".into());
        }
        Ok(())
    }

    fn status_of(&self, e: &Entry) -> ViewStatus {
        if let Some(r) = &self.running {
            if r.id == e.id {
                return ViewStatus::Processing;
            }
        }
        if self.queue.iter().any(|q| q == &e.id) {
            return ViewStatus::Queued;
        }
        if self.uploads.contains_key(&e.id) {
            return ViewStatus::Uploading;
        }
        if self.scanned && !self.present(&e.id) {
            return ViewStatus::Missing;
        }
        if self.last.copying_ids.contains(&e.id) {
            return ViewStatus::Copying;
        }
        if self.last.unreadable_ids.contains(&e.id) {
            return ViewStatus::Unreadable;
        }
        match e.status {
            EntryStatus::New if e.duration.is_some_and(|d| d > MAX_TRANSCRIBE_SECS) => ViewStatus::TooLong,
            EntryStatus::New => ViewStatus::New,
            // A persisted Processing without a running job is shown as an error
            // until the startup fix-up runs.
            EntryStatus::Processing | EntryStatus::Error => ViewStatus::Error,
            EntryStatus::ReadyForReview => ViewStatus::ReadyForReview,
            EntryStatus::Done => ViewStatus::Done,
        }
    }

    fn video_view(&self, e: &Entry) -> LibraryVideo {
        let paths = self.paths.as_ref();
        let status = self.status_of(e);
        let thumbnail = match (status, paths.and_then(|p| p.video_dir(&e.id).ok())) {
            (ViewStatus::ReadyForReview | ViewStatus::Uploading | ViewStatus::Done | ViewStatus::Error | ViewStatus::Missing, Some(dir)) => {
                let data = manifest::load_video_data(&dir);
                data.selected_thumbnail
                    .or_else(|| data.thumbnail_options.first().cloned())
                    .map(|name| path_string(&dir.join(name)))
                    .filter(|p| Path::new(p).exists())
            }
            _ => None,
        };
        let running = self.running.as_ref().filter(|r| r.id == e.id);
        let ahead = usize::from(self.running.is_some());
        let queue_position = self.queue.iter().position(|q| *q == e.id).map(|i| i + 1 + ahead);
        LibraryVideo {
            id: e.id.clone(),
            file_name: e.file_name.clone(),
            file_path: paths.map(|p| path_string(&p.root.join(&e.file_name))).unwrap_or_default(),
            title: e.display_title(),
            status,
            error: match status {
                ViewStatus::Error => e.error.clone().or_else(|| Some("Something went wrong".into())),
                ViewStatus::Unreadable => Some("The file changed and Sermon Cut can't read it any more. It may be incomplete or damaged.".into()),
                ViewStatus::Queued | ViewStatus::Processing => None,
                // E.g. a failed re-process of a video whose earlier results are kept.
                _ => e.error.clone(),
            },
            duration: e.duration,
            size: e.fingerprint.size,
            channel: e.channel.clone(),
            source_url: e.source_url.clone(),
            hidden: e.hidden,
            uploads: e.uploads.clone(),
            added_at: e.added_at.clone(),
            thumbnail,
            processing_stage: running.map(|r| r.stage.clone()),
            processing_progress: running.map(|r| r.progress),
            processing_message: running.map(|r| r.message.clone()),
            upload_progress: self.uploads.get(&e.id).map(|u| u.percent),
            busy: self.is_busy(&e.id),
            queue_position,
            queue_length: queue_position.map(|_| self.queue.len() + ahead),
        }
    }

    fn pending_view(&self, o: &FileObs) -> LibraryVideo {
        LibraryVideo {
            id: format!("pending:{}", o.name),
            file_name: o.name.clone(),
            file_path: self.paths.as_ref().map(|p| path_string(&p.root.join(&o.name))).unwrap_or_default(),
            title: Path::new(&o.name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| o.name.clone()),
            status: if o.state == ObsState::Unreadable { ViewStatus::Unreadable } else { ViewStatus::Copying },
            error: (o.state == ObsState::Unreadable)
                .then(|| "Sermon Cut can't read this file. It may be incomplete or not a video.".to_string()),
            duration: None,
            size: o.size,
            channel: None,
            source_url: None,
            hidden: false,
            uploads: vec![],
            // Sorts with the newest, i.e. at the top.
            added_at: chrono::Utc::now().to_rfc3339(),
            thumbnail: None,
            processing_stage: None,
            processing_progress: None,
            processing_message: None,
            upload_progress: None,
            busy: true,
            queue_position: None,
            queue_length: None,
        }
    }

    fn view(&self, notices: Vec<String>) -> LibraryView {
        let mut videos: Vec<LibraryVideo> = self.manifest.entries.iter().map(|e| self.video_view(e)).collect();
        videos.extend(self.last.copying.iter().chain(self.last.unreadable.iter()).map(|o| self.pending_view(o)));
        // Newest first.
        videos.sort_by(|a, b| b.added_at.cmp(&a.added_at));

        let (available, writable, message, locked_by) = match &self.mode {
            Mode::Writable => (true, true, self.notice.clone(), None),
            Mode::Locked(host) => (true, false, self.mode.reason(), Some(host.clone())),
            Mode::ReadOnly(why) => (true, false, Some(why.clone()), None),
            Mode::Unavailable(why) => (false, false, Some(why.clone()), None),
        };

        LibraryView {
            folder: self
                .paths
                .as_ref()
                .map(|p| path_string(&p.root))
                .unwrap_or_else(|| self.configured.clone()),
            available,
            writable,
            message,
            locked_by,
            tool_problem: crate::tools::missing_tool_message(),
            videos,
            downloads: self.downloads.clone(),
            notices,
            max_transcribe_secs: MAX_TRANSCRIBE_SECS,
        }
    }
}

/// Delete stale temp files from a crashed run: everything in tmp/,
/// half-written JSON (`*.tmp-*`) next to the real files and unfinished
/// renders (`.render-tmp-*`); and, under each video's lock, renders its
/// video.json doesn't record.
fn sweep(paths: &LibraryPaths, video_lock: &dyn Fn(&str) -> Arc<Mutex<()>>) {
    let _ = std::fs::remove_dir_all(paths.tmp_dir()); // removes a symlink itself, never its target
    let mut dirs = vec![paths.data_dir()];
    let videos_dir = paths.videos_dir();
    if std::fs::symlink_metadata(&videos_dir).is_ok_and(|m| m.is_dir()) {
        if let Ok(videos) = std::fs::read_dir(&videos_dir) {
            dirs.extend(
                videos
                    .flatten()
                    .map(|d| d.path())
                    .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())),
            );
        }
    }
    for dir in dirs {
        recover_commit(&dir);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.contains(TMP_MARKER) || name.starts_with(manifest::RENDER_TMP_PREFIX) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let id = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
        if super::is_valid_id(&id) && dir.parent() == Some(videos_dir.as_path()) {
            let lock = video_lock(&id);
            let _v = hold(&lock);
            manifest::prune_renders(&dir, &manifest::load_video_data(&dir));
        }
    }
}

/// The outside tools a processing job runs, so tests can stand in for
/// ffmpeg and the thumbnail model.
pub(crate) trait JobTools: Sync {
    async fn extract_audio(&self, video: &Path, audio: &Path) -> Result<(), String>;
    async fn duration(&self, video: &Path) -> Result<f64, String>;
    /// Cut `video` from `start` to `end` into `out`; returns where the cut
    /// really starts (see `ffmpeg::trim_accurate`).
    async fn trim(&self, video: &Path, start: f64, end: f64, out: &Path, on_step: &(dyn Fn(f32, &str) + Send + Sync)) -> Result<f64, String>;
    /// Thumbnail options (and their plain crops) for the cut, into `out`;
    /// returns their paths.
    async fn thumbnails(&self, video: &Path, start: f64, end: f64, title: &str, out: &Path) -> Result<Vec<String>, String>;
}

/// ffmpeg and the thumbnail model.
struct RealTools;

impl JobTools for RealTools {
    async fn extract_audio(&self, video: &Path, audio: &Path) -> Result<(), String> {
        crate::ffmpeg::extract_audio(video, audio).await
    }
    async fn duration(&self, video: &Path) -> Result<f64, String> {
        crate::ffmpeg::get_duration(&path_string(video)).await
    }
    async fn trim(&self, video: &Path, start: f64, end: f64, out: &Path, on_step: &(dyn Fn(f32, &str) + Send + Sync)) -> Result<f64, String> {
        Ok(crate::ffmpeg::trim_accurate(video, start, end, out, on_step).await?.start)
    }
    async fn thumbnails(&self, video: &Path, start: f64, end: f64, title: &str, out: &Path) -> Result<Vec<String>, String> {
        crate::thumbnail::generate_thumbnail_options(video, start, end, title, out, 10).await
    }
}

/// What a processing job made, for `commit_job`.
struct JobOutputs {
    trimmed_name: String,
    start: f64,
    end: f64,
    thumb_names: Vec<String>,
    duration: f64,
}

/// Where a commit keeps a video's previous outputs (and video.json) until
/// the new ones are in. Hidden, so no video.json can name it. Its journal,
/// written before anything moves, lists what moves and how far it got.
const COMMIT_BACKUP: &str = ".commit-backup";
const COMMIT_JOURNAL: &str = ".journal.json";
/// What the backup's copy of video.json is called.
const COMMIT_DATA: &str = "video.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommitPhase {
    /// Moving files; a crash now means the old outputs go back.
    Swapping,
    /// The new outputs and video.json are in; only the clean-up is left.
    Done,
}

#[derive(Debug, Serialize, serde::Deserialize)]
struct CommitJournal {
    phase: CommitPhase,
    /// Old outputs, moved from the folder into the backup.
    old: Vec<String>,
    /// New outputs, moved from staging into the folder.
    new: Vec<String>,
}

/// Generated files in a video folder: everything but the transcript and
/// the review data.
fn is_output(name: &str) -> bool {
    name != "transcript.json"
        && name != "video.json"
        && !name.contains(TMP_MARKER)
        && !name.starts_with(manifest::RENDER_TMP_PREFIX)
}

/// Plain files directly in `dir` (sorted, so a commit is repeatable).
fn files_in(dir: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name())
        .collect();
    names.sort();
    Ok(names)
}

/// Names of the plain files in `dir` (sorted), leaving out any that
/// aren't UTF-8, which no job makes.
fn names_in(dir: &Path) -> std::io::Result<Vec<String>> {
    Ok(files_in(dir)?.into_iter().filter_map(|n| n.into_string().ok()).collect())
}

fn write_journal(backup: &Path, journal: &CommitJournal) -> Result<(), String> {
    fsutil::write_json_atomic(&backup.join(COMMIT_JOURNAL), journal)
}

/// A commit's backup folder is still there after `recover_commit` tried
/// to put it back (the frontend offers to show it: `isStuckCommit`).
pub const STUCK_COMMIT: &str = "An earlier save of this video didn't finish and couldn't be undone, so nothing was changed. Click Show leftovers, move the \".commit-backup\" folder out of the video's folder, then try again.";

/// Start a commit: the backup folder, its journal, and a copy of the
/// current video.json, before anything moves. The old outputs are those
/// `replaces` picks. A backup left by an earlier commit is put right
/// first; if it can't be, nothing is done.
fn begin_commit(dir: &Path, staging: &Path, replaces: &dyn Fn(&str) -> bool) -> Result<CommitJournal, String> {
    recover_commit(dir);
    let backup = dir.join(COMMIT_BACKUP);
    std::fs::create_dir(&backup).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => STUCK_COMMIT.to_string(),
        _ => format!("Failed to save results: {}", e),
    })?;
    let begun = (|| -> Result<CommitJournal, String> {
        let old: Vec<String> = names_in(dir)
            .map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?
            .into_iter()
            .filter(|n| is_output(n) && replaces(n))
            .collect();
        // Everything staged, folders too.
        let mut new: Vec<String> = std::fs::read_dir(staging)
            .map_err(|e| format!("Failed to read work folder: {}", e))?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        new.sort();
        let journal = CommitJournal { phase: CommitPhase::Swapping, old, new };
        write_journal(&backup, &journal)?;
        match std::fs::read(dir.join("video.json")) {
            Ok(bytes) => fsutil::write_atomic(&backup.join(COMMIT_DATA), &bytes)
                .map_err(|e| format!("Failed to save results: {}", e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Failed to read video.json: {}", e)),
        }
        Ok(journal)
    })();
    if begun.is_err() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    begun
}

/// Swap a job's staged outputs into the video folder, then write its new
/// video.json with `write_data`. The old outputs are moved aside first and
/// put back if anything fails, so the folder is never left half old, half
/// new; on success they're deleted. A crash midway is put right by
/// `recover_commit`.
fn swap_in_outputs(dir: &Path, staging: &Path, write_data: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    swap_in_outputs_where(dir, staging, &|_| true, write_data)
}

/// `swap_in_outputs`, replacing only the old outputs `replaces` picks; the
/// others stay as they are.
fn swap_in_outputs_where(
    dir: &Path,
    staging: &Path,
    replaces: &dyn Fn(&str) -> bool,
    write_data: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let backup = dir.join(COMMIT_BACKUP);
    let mut journal = begin_commit(dir, staging, replaces)?;

    let mut aside = Vec::new();
    let mut moved_in = Vec::new();
    let result = (|| -> Result<(), String> {
        for name in &journal.old {
            std::fs::rename(dir.join(name), backup.join(name)).map_err(|e| format!("Failed to save results: {}", e))?;
            aside.push(name.clone());
        }
        for name in &journal.new {
            std::fs::rename(staging.join(name), dir.join(name)).map_err(|e| format!("Failed to save results: {}", e))?;
            moved_in.push(name.clone());
        }
        write_data()
    })();

    match result {
        Ok(()) => {
            // Committed. Mark it so an interrupted clean-up isn't undone.
            journal.phase = CommitPhase::Done;
            let _ = write_journal(&backup, &journal);
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(e) => {
            for name in &moved_in {
                if std::fs::rename(dir.join(name), staging.join(name)).is_err() {
                    let _ = std::fs::remove_file(dir.join(name));
                }
            }
            let mut restored = true;
            for name in &aside {
                restored &= std::fs::rename(backup.join(name), dir.join(name)).is_ok();
            }
            if restored {
                let _ = std::fs::remove_dir_all(&backup);
            } // else left for recover_commit on the next open
            Err(e)
        }
    }
}

/// Finish a commit a crash interrupted. One whose journal reached Done is
/// kept. Otherwise the folder goes back to how it was: everything in the
/// backup (old outputs, old video.json) is moved back, replacing the new
/// file of the same name, and new outputs with no old namesake are
/// deleted. Files only ever in the folder (old ones not yet moved aside,
/// or already put back) stay. The backup is removed once all is back.
fn recover_commit(dir: &Path) {
    let backup = dir.join(COMMIT_BACKUP);
    if !std::fs::symlink_metadata(&backup).is_ok_and(|m| m.is_dir()) {
        return;
    }
    // No journal: the crash came before it was written, when nothing had
    // moved yet.
    let journal: Option<CommitJournal> = fsutil::read_json(&backup.join(COMMIT_JOURNAL), 1 << 20).ok().flatten();
    // `DONE`: how an earlier build marked a finished commit.
    if journal.as_ref().is_some_and(|j| j.phase == CommitPhase::Done) || backup.join("DONE").exists() {
        let _ = std::fs::remove_dir_all(&backup);
        return;
    }
    let mut restored = true;
    for name in names_in(&backup).unwrap_or_default() {
        if name == COMMIT_JOURNAL || name.contains(TMP_MARKER) {
            continue;
        }
        restored &= std::fs::rename(backup.join(&name), dir.join(&name)).is_ok();
    }
    if let Some(j) = &journal {
        let old: HashSet<&String> = j.old.iter().collect();
        for name in j.new.iter().filter(|n| !old.contains(n) && is_output(n)) {
            match std::fs::remove_file(dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => restored = false,
                _ => {}
            }
        }
    }
    if restored {
        let _ = std::fs::remove_dir_all(&backup);
    }
}

/// On open, before `mark_interrupted`: a queued entry still marked
/// Processing (and not this session's running job) was cut short by the
/// app stopping; count it. True if any was.
fn count_restarts(manifest: &mut Manifest, running: Option<&str>) -> bool {
    let queued: HashSet<String> = manifest.queue.iter().cloned().collect();
    let mut any = false;
    for e in &mut manifest.entries {
        if e.status == EntryStatus::Processing && running != Some(e.id.as_str()) && queued.contains(&e.id) {
            e.process_attempts += 1;
            any = true;
        }
    }
    any
}

/// After `mark_interrupted`: say why an entry stopped too often to be
/// restarted (it then leaves the queue).
fn give_up_restarting(manifest: &mut Manifest) {
    let queued: HashSet<String> = manifest.queue.iter().cloned().collect();
    for e in &mut manifest.entries {
        if e.process_attempts >= super::MAX_RESTARTS && queued.contains(&e.id) {
            e.error = Some(if e.status == EntryStatus::Error {
                "Processing stopped unexpectedly twice — click Retry.".into()
            } else {
                "Re-processing stopped unexpectedly twice — click Reprocess to try again. The earlier results are kept.".into()
            });
        }
    }
}

fn newer_library_message(wanted: Option<&str>) -> String {
    match wanted {
        Some(v) => format!(
            "This library was updated by a newer Sermon Cut — please update (to version {} or later) to make changes here.",
            v
        ),
        None => "This library was updated by a newer Sermon Cut — please update to make changes here.".into(),
    }
}

fn fingerprint_matches(now: &Fingerprint, expected: &Fingerprint) -> bool {
    now.size == expected.size && now.hash == expected.hash
}

/// What `unmounted_volume` needs to know about a path.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeStat {
    dev: u64,
    symlink: bool,
}

#[cfg(unix)]
fn volume_stat(path: &Path) -> Option<VolumeStat> {
    use std::os::unix::fs::MetadataExt;
    let symlink = std::fs::symlink_metadata(path).ok()?.file_type().is_symlink();
    Some(VolumeStat { dev: std::fs::metadata(path).ok()?.dev(), symlink })
}

#[cfg(not(unix))]
fn volume_stat(_: &Path) -> Option<VolumeStat> {
    None
}

/// For a path on /Volumes/<X>: X's name if no drive is mounted there.
/// When a drive is unplugged without being ejected, macOS can leave
/// /Volumes/<X> behind as a plain folder on the startup disk (or a program
/// can create one); writing there would quietly start a second, empty
/// library. A mounted drive has its own device id, different from that of
/// /Volumes itself. "/Volumes/Macintosh HD" is a symlink to /, not stale.
fn unmounted_volume(path: &Path, stat: impl Fn(&Path) -> Option<VolumeStat>) -> Option<String> {
    use std::path::Component;
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) || parts.next() != Some(Component::Normal("Volumes".as_ref())) {
        return None;
    }
    let Some(Component::Normal(name)) = parts.next() else { return None };
    let volumes = Path::new("/Volumes");
    let volume = stat(&volumes.join(name))?;
    let parent = stat(volumes)?;
    (!volume.symlink && volume.dev == parent.dev).then(|| name.to_string_lossy().to_string())
}

fn open_paths(configured: &str) -> Result<LibraryPaths, Mode> {
    let raw = PathBuf::from(configured);
    if cfg!(target_os = "macos") {
        if let Some(drive) = unmounted_volume(&raw, volume_stat) {
            // Nothing is created: the folder would land on the startup disk.
            return Err(Mode::Unavailable(format!(
                "The drive \"{}\" is not connected. Plug it in to open the library {}.",
                drive, configured
            )));
        }
    }
    if !raw.exists() {
        if configured == crate::state::default_library_dir() {
            std::fs::create_dir_all(&raw)
                .map_err(|e| Mode::Unavailable(format!("Can't create the library folder {}: {}", configured, e)))?;
        } else {
            return Err(Mode::Unavailable(format!(
                "Can't find the library folder {}. If it's on an external drive, plug it in. Or choose another folder in Settings.",
                configured
            )));
        }
    }
    let root = raw
        .canonicalize()
        .map_err(|e| Mode::Unavailable(format!("Can't open the library folder {}: {}", configured, e)))?;
    if !root.is_dir() {
        return Err(Mode::Unavailable(format!("{} is not a folder", configured)));
    }
    Ok(LibraryPaths::new(root))
}

impl Library {
    pub fn new(configured: String) -> Arc<Self> {
        let lib = Arc::new(Library {
            inner: Mutex::new(Inner {
                configured: String::new(),
                paths: None,
                manifest: Manifest::default(),
                mode: Mode::Unavailable("The library hasn't been opened yet".into()),
                cache: ScanCache::default(),
                last: Reconciled::default(),
                scanned: false,
                queue: VecDeque::new(),
                running: None,
                busy: HashSet::new(),
                rendering: 0,
                uploads: std::collections::HashMap::new(),
                downloads: Vec::new(),
                session: super::new_id(),
                notice: None,
                orphans: std::collections::HashMap::new(),
                updating: false,
                baseline_scan: false,
            }),
            scan_lock: tokio::sync::Mutex::new(()),
            lock_file_mutex: tokio::sync::Mutex::new(()),
            video_locks: Mutex::new(std::collections::HashMap::new()),
            wake: tokio::sync::Notify::new(),
            migration_tried: std::sync::atomic::AtomicBool::new(false),
        });
        lib.inner().configured = configured;
        lib
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The lock for one video's folder. Lock it with `hold`, before `inner`.
    fn video_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.video_locks.lock().unwrap_or_else(|p| p.into_inner());
        // Forget the ones nobody holds, so the map stays small.
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        locks.entry(id.to_string()).or_default().clone()
    }

    /// (Re)open the configured folder. Blocking: call from a blocking thread.
    fn open_blocking(&self, force_lock: bool) {
        let configured = self.inner().configured.clone();
        let paths = match open_paths(&configured) {
            Ok(p) => p,
            Err(mode) => {
                let mut g = self.inner();
                g.mode = mode;
                return;
            }
        };

        let mut notice = None;
        let mut recovered = false;
        // The list starts over from what's in the folder.
        let mut rebuilt = false;
        let (manifest, mut mode) = match paths.ensure_dir(&paths.data_dir()) {
            Err(e) => (
                Manifest::default(),
                Mode::ReadOnly(format!("Sermon Cut can't save its data in this folder ({}). Choose a folder you can write to.", e)),
            ),
            Ok(()) => match manifest::load(&paths) {
                Ok(Loaded::Ok(m)) => (m, Mode::Writable),
                Ok(Loaded::Corrupt(m, aside, restored)) => {
                    notice = Some(if restored {
                        format!(
                            "The library's tracking file couldn't be read, so it was set aside as .sermon-cut/{} and the list was restored from its backup.",
                            aside
                        )
                    } else {
                        format!(
                            "The library's tracking file couldn't be read, so it was set aside as .sermon-cut/{} and the list was rebuilt. Processed videos keep their results; upload history isn't shown.",
                            aside
                        )
                    });
                    recovered = restored;
                    rebuilt = !restored;
                    (m, Mode::Writable)
                }
                Ok(Loaded::Restored(m)) => {
                    notice = Some("The library's tracking file was missing, so the list was restored from its backup.".into());
                    recovered = true;
                    (m, Mode::Writable)
                }
                Ok(Loaded::Lost(m)) => {
                    rebuilt = true;
                    notice = Some(
                        "The library's tracking file was missing, so the list was rebuilt. Processed videos keep their results; upload history isn't shown.".into(),
                    );
                    (m, Mode::Writable)
                }
                Ok(Loaded::Stuck(why)) => (Manifest::default(), Mode::ReadOnly(why)),
                Ok(Loaded::Newer(m, wanted)) => (
                    m,
                    Mode::ReadOnly(newer_library_message(wanted.as_deref())),
                ),
                Err(e) => (Manifest::default(), Mode::Unavailable(e)),
            },
        };

        // A reopen (lock regained, take over) can happen while this session
        // still has work going; that work's files and status are left alone.
        let (session, running, idle, queue_idle) = {
            let g = self.inner();
            let running = g.running.as_ref().map(|r| r.id.clone());
            let idle = g.is_idle();
            (g.session.clone(), running, idle, g.running.is_none() && g.queue.is_empty())
        };
        if mode == Mode::Writable {
            match lock::acquire(&paths, &session, force_lock) {
                Ok(Acquire::Acquired) => {}
                Ok(Acquire::HeldBy(host)) => mode = Mode::Locked(host),
                Err(e) => mode = Mode::ReadOnly(format!("Sermon Cut can't write to this folder: {}", e)),
            }
        }

        let mut manifest = manifest;
        let mut orphans = std::collections::HashMap::new();
        if mode == Mode::Writable {
            if idle {
                sweep(&paths, &|id| self.video_lock(id));
            }
            let has_results = |e: &Entry| paths.video_dir(&e.id).is_ok_and(|dir| manifest::has_results(&dir));
            let restarts = count_restarts(&mut manifest, running.as_deref());
            if manifest::mark_interrupted(&mut manifest, running.as_deref(), has_results) || restarts || recovered {
                give_up_restarting(&mut manifest);
                if let Err(e) = manifest::save(&paths, &manifest) {
                    eprintln!("[library] Failed to save library.json: {}", e);
                }
            }
            // Normally none: a folder outlives its entry only when
            // library.json was lost or restored from an older backup.
            orphans = manifest::find_orphans(&paths, &manifest);
        }
        // The queue of the last session (re-process requests of finished
        // videos too), minus what can't run or was interrupted too often.
        let restored: Vec<String> = if mode == Mode::Writable && queue_idle {
            manifest
                .queue
                .iter()
                .filter(|id| {
                    manifest.get(id).is_some_and(|e| {
                        e.process_attempts < super::MAX_RESTARTS && paths.original(&e.file_name).is_ok_and(|p| p.is_file())
                    })
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let mut g = self.inner();
        // Queue, running job, uploads and downloads are this session's and
        // carry on; mark_interrupted skipped the running job's entry.
        g.paths = Some(paths);
        g.manifest = manifest;
        g.mode = mode;
        g.notice = notice;
        g.orphans = orphans;
        g.cache = ScanCache::default();
        g.last = Reconciled::default();
        g.scanned = false;
        if rebuilt {
            g.baseline_scan = true;
        }
        let saved_queue = g.manifest.queue.clone();
        if g.mode == Mode::Writable && g.queue.is_empty() && g.running.is_none() {
            g.queue = restored.iter().cloned().collect();
            g.manifest.queue = restored;
        } else {
            // This session's queue carries on (see above).
            let mut current: Vec<String> = g.running.iter().map(|r| r.id.clone()).collect();
            current.extend(g.queue.iter().cloned());
            g.manifest.queue = current;
        }
        if g.manifest.queue != saved_queue {
            g.save_logged();
        }
        let wake = !g.queue.is_empty();
        drop(g);
        if wake {
            self.wake.notify_one();
        }
    }

    /// Whether the library could move to `configured` right now.
    pub fn check_switch(&self, configured: &str) -> Result<(), String> {
        let g = self.inner();
        if g.configured == configured {
            return Ok(());
        }
        if !g.is_idle() {
            return Err(SWITCH_BUSY.into());
        }
        Ok(())
    }

    /// Point the library at a different folder (Settings).
    pub async fn switch(self: &Arc<Self>, configured: String) -> Result<(), String> {
        // No scan, trash or download commit runs meanwhile; while the folder
        // changes, nothing can be written (the old paths are gone).
        let _scan = self.scan_lock.lock().await;
        let _lock_file = self.lock_file_mutex.lock().await;
        let (paths, session) = {
            let mut g = self.inner();
            if g.configured == configured {
                return Ok(());
            }
            if !g.is_idle() {
                return Err(SWITCH_BUSY.into());
            }
            g.configured = configured;
            g.mode = Mode::Unavailable("Switching folders…".into());
            // The new folder's videos are a backlog, not new arrivals.
            g.baseline_scan = true;
            (g.paths.take(), g.session.clone())
        };
        let lib = self.clone();
        blocking(move || {
            if let Some(old) = paths {
                lock::release(&old, &session);
            }
            lib.open_blocking(false);
        })
        .await
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.inner().paths.as_ref().map(|p| p.root.clone())
    }

    /// Let go of lock.json (the app is quitting). Sync, so it can run from
    /// the exit handler: it waits briefly for a heartbeat in progress.
    pub fn release_lock(&self) {
        let mut waited = 0;
        let _lock_file = loop {
            match self.lock_file_mutex.try_lock() {
                Ok(guard) => break Some(guard),
                // A heartbeat takes milliseconds; don't hang the quit on it.
                Err(_) if waited >= 2000 => break None,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    waited += 20;
                }
            }
        };
        let (paths, session) = {
            let g = self.inner();
            match (&g.paths, &g.mode) {
                (Some(paths), mode) if mode.holds_lock() => (paths.clone(), g.session.clone()),
                _ => return,
            }
        };
        lock::release(&paths, &session);
    }

    pub async fn take_over(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let _lock_file = self.lock_file_mutex.lock().await;
        // What arrived while another computer had it is a backlog.
        self.inner().baseline_scan = true;
        let lib = self.clone();
        blocking(move || lib.open_blocking(true)).await
    }

    /// Refresh lock.json; go read-only if another computer took over.
    /// `Regained` means another computer let go of a library we were locked
    /// out of, so it should be reopened for writing. Holds the lock-file
    /// mutex (not the scan lock, which a migration may hold for minutes) so
    /// it can't write the lock file of a folder being switched away from.
    pub async fn heartbeat(self: &Arc<Self>) -> HeartbeatOutcome {
        let _lock_file = self.lock_file_mutex.lock().await;
        let (paths, session, mode) = {
            let g = self.inner();
            let Some(paths) = g.paths.clone() else { return HeartbeatOutcome::Unchanged };
            (paths, g.session.clone(), g.mode.clone())
        };
        let lib = self.clone();
        let outcome = blocking(move || match mode {
            _ if mode.holds_lock() => match lock::heartbeat(&paths, &session) {
                Ok(Some(host)) => {
                    let mut g = lib.inner();
                    // Unchanged meanwhile (the lock-file mutex keeps opens
                    // and switches out, but not every mode change).
                    if g.mode.holds_lock() {
                        g.mode = Mode::Locked(host.clone());
                        return HeartbeatOutcome::LostTo(host);
                    }
                    HeartbeatOutcome::Unchanged
                }
                Ok(None) => HeartbeatOutcome::Unchanged,
                Err(e) => {
                    eprintln!("[library] Lock heartbeat failed: {}", e);
                    HeartbeatOutcome::Unchanged
                }
            },
            Mode::Locked(_) => {
                match lock::acquire(&paths, &session, false) {
                    Ok(Acquire::Acquired) => HeartbeatOutcome::Regained,
                    _ => HeartbeatOutcome::Unchanged,
                }
            }
            _ => HeartbeatOutcome::Unchanged,
        })
        .await;
        outcome.unwrap_or(HeartbeatOutcome::Unchanged)
    }

    /// Reopen after the lock was regained. What arrived while another
    /// computer had the library is a backlog, not new arrivals.
    pub async fn reopen(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let _lock_file = self.lock_file_mutex.lock().await;
        self.inner().baseline_scan = true;
        let lib = self.clone();
        blocking(move || lib.open_blocking(false)).await
    }

    /// Migrate pre-0.4 videos if there are any and the library is writable.
    /// Called under the scan lock. Runs once per launch: a video that fails
    /// is tried again on the next launch, up to migrate::MAX_ATTEMPTS.
    async fn migrate_if_needed(self: &Arc<Self>, app_state: &AppStateHandle) {
        if self.migration_tried.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let legacy = {
            let state = app_state.lock().await;
            if state.legacy_videos.is_empty() {
                return;
            }
            state.legacy_videos.clone()
        };
        // Nothing else may change the list while files move, so the library
        // is read-only ("Updating library…") meanwhile. The scan lock keeps
        // scans, trash, downloads and switches out; idle means no job,
        // upload or download can write either. Busy: a later scan retries.
        let (paths, manifest) = {
            let mut g = self.inner();
            let Ok(paths) = g.writable().cloned() else {
                // Not writable yet (e.g. drive unplugged): a later scan retries.
                return;
            };
            if !g.is_idle() {
                return;
            }
            g.mode = Mode::ReadOnly(UPDATING_LIBRARY.into());
            (paths, g.manifest.clone())
        };
        self.migration_tried.store(true, std::sync::atomic::Ordering::Relaxed);

        let moved = blocking({
            let paths = paths.clone();
            move || {
                let mut manifest = manifest;
                let outcome = super::migrate::migrate(&paths, &mut manifest, &legacy);
                // Saved even after failures: entries made so far point at
                // files already moved, and the next run picks up from there.
                let saved = manifest::save(&paths, &manifest);
                (manifest, outcome, saved)
            }
        })
        .await;
        {
            let mut g = self.inner();
            if g.mode == Mode::ReadOnly(UPDATING_LIBRARY.into()) {
                g.mode = Mode::Writable;
            }
            if let Ok((manifest, _, _)) = &moved {
                g.manifest = manifest.clone();
            }
        }
        let (outcome, saved) = match moved {
            Ok((_, outcome, saved)) => (outcome, saved),
            Err(e) => return eprintln!("[migrate] {}", e),
        };
        if let Err(e) = saved {
            // state.json keeps every old video; the next launch tries again.
            return eprintln!("[migrate] Failed to save library.json: {}", e);
        }

        let mut state = app_state.lock().await;
        if let Err(e) = state.backup_pre_library_state() {
            eprintln!("[migrate] {}", e);
            return;
        }
        let notices = super::migrate::settle(&mut state.legacy_videos, &outcome);
        state.notices.extend(notices);
        match state.save() {
            Ok(()) => super::migrate::cleanup(&paths, &outcome),
            Err(e) => eprintln!("[migrate] Failed to save state: {}", e),
        }
        println!(
            "[migrate] Moved {} old videos into the library; {} failed",
            outcome.done.len(),
            outcome.failed.len()
        );
    }

    /// Scan the folder. One at a time; I/O happens without the lock.
    pub async fn rescan(self: &Arc<Self>, app_state: &AppStateHandle) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;

        let needs_open = !matches!(self.inner().mode, Mode::Writable | Mode::ReadOnly(_) | Mode::Locked(_));
        if needs_open {
            let _lock_file = self.lock_file_mutex.lock().await;
            let lib = self.clone();
            blocking(move || lib.open_blocking(false)).await?;
        }
        self.migrate_if_needed(app_state).await;

        let (root, known, mut cache) = {
            let mut g = self.inner();
            let Some(paths) = g.paths.clone() else { return Ok(()) };
            if matches!(g.mode, Mode::Unavailable(_)) {
                return Ok(());
            }
            (paths.root, scan::known_files(&g.manifest.entries), std::mem::take(&mut g.cache))
        };

        let observed = blocking(move || {
            let result = scan::observe(&root, &known, &mut cache, &crate::ffmpeg::probe_duration_blocking, scan::STABLE_FOR);
            (result, cache)
        })
        .await?;

        let obs = {
            let mut g = self.inner();
            let (result, cache) = observed;
            g.cache = cache;
            match result {
                Ok(obs) => obs,
                Err(e) => {
                    let folder = g.paths.as_ref().map(|p| path_string(&p.root)).unwrap_or_default();
                    g.mode = Mode::Unavailable(format!(
                        "Can't read the library folder {} ({}). If it's on an external drive, plug it back in.",
                        folder, e
                    ));
                    return Ok(());
                }
            }
        };
        self.finish_scan(&obs).await;

        let since = {
            let s = app_state.lock().await;
            s.settings.auto_process.then_some(s.settings.auto_process_since).flatten()
        };
        if let Some(since) = since {
            self.auto_process(since).await;
        }
        Ok(())
    }

    /// Queue the videos auto-process should take (see
    /// `auto_process_candidates`). Returns the ones queued.
    async fn auto_process(self: &Arc<Self>, since: i64) -> Vec<String> {
        let candidates = {
            let g = self.inner();
            if g.mode != Mode::Writable || g.updating {
                return Vec::new();
            }
            g.auto_process_candidates(since)
        };
        let mut queued = Vec::new();
        for id in candidates {
            match self.enqueue(&id).await {
                Ok(()) => {
                    let mut g = self.inner();
                    if let Ok(e) = g.entry_mut(&id) {
                        e.auto_queued = true;
                    }
                    g.save_logged();
                    queued.push(id);
                }
                Err(e) => eprintln!("[library] Auto-process couldn't queue {}: {}", id, e),
            }
        }
        queued
    }

    /// Apply a scan, then delete replaced videos' old outputs outside the
    /// lock. Those videos stay busy (nothing can be queued) until done.
    async fn finish_scan(self: &Arc<Self>, obs: &[FileObs]) {
        let (stale, paths) = {
            let mut g = self.inner();
            (g.apply_scan(obs), g.paths.clone())
        };
        if stale.is_empty() {
            return;
        }
        if let Some(paths) = paths {
            let ids = stale.clone();
            let _ = blocking(move || {
                for id in &ids {
                    if let Err(e) = manifest::remove_video_dir(&paths, id) {
                        eprintln!("[library] {}", e);
                    }
                }
            })
            .await;
        }
        let mut g = self.inner();
        for id in &stale {
            g.busy.remove(id);
        }
    }

    pub fn view(&self, notices: Vec<String>) -> LibraryView {
        self.inner().view(notices)
    }

    /// Videos waiting for review, for the Dock badge.
    pub fn ready_for_review_count(&self) -> usize {
        let g = self.inner();
        g.manifest
            .entries
            .iter()
            .filter(|e| !e.hidden && g.status_of(e) == ViewStatus::ReadyForReview)
            .count()
    }

    /// The title a video is shown with, for notifications.
    pub fn title_of(&self, id: &str) -> String {
        self.inner().manifest.get(id).map(|e| e.display_title()).unwrap_or_default()
    }

    pub fn detail(&self, id: &str) -> Result<VideoDetail, String> {
        let g = self.inner();
        let e = g.entry(id)?;
        let video = g.video_view(e);
        let paths = g.paths.clone().ok_or("The library isn't open")?;
        let hash = e.fingerprint.hash.clone();
        let duplicate_upload = g.duplicate_upload(id, &hash);
        drop(g);

        let dir = paths.video_dir(id)?;
        let data = manifest::load_video_data(&dir);
        let transcription = manifest::load_transcript(&dir)
            .filter(|t| t.file_hash == hash)
            .map(|t| t.transcription);
        let abs = |name: &String| path_string(&dir.join(name));
        let thumbnail_options = data
            .thumbnail_options
            .iter()
            .map(|id| {
                let (render_from, version) = render_from(&dir, id);
                ThumbnailOption {
                    id: id.clone(),
                    path: abs(id),
                    render_from: path_string(&render_from),
                    version,
                }
            })
            .collect();
        let renders = data
            .renders
            .iter()
            .filter(|r| dir.join(&r.file).is_file())
            .map(|r| RenderView {
                source: r.source.clone(),
                path: abs(&r.file),
                render_key: r.render_key.clone(),
            })
            .collect();
        Ok(VideoDetail {
            video,
            transcription,
            trimmed_path: data.trimmed_file.as_ref().map(abs).filter(|p| Path::new(p).exists()),
            trim_start: data.trim_start,
            trim_end: data.trim_end,
            thumbnail_options,
            renders,
            selected_source: data.selected_or_first().cloned(),
            selected_thumbnail: data.selected_thumbnail.as_ref().map(abs),
            review: data.review,
            duplicate_upload,
        })
    }

    // -----------------------------------------------------------------------
    // Queue

    pub async fn enqueue(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (root, path, expected) = {
            let g = self.inner();
            g.require_not_updating()?;
            let paths = g.writable()?;
            let e = g.entry(id)?;
            if g.is_busy(id) {
                return Err("This video is already queued or processing".into());
            }
            if !g.present(id) {
                return Err(format!("{} isn't in the library folder any more", e.file_name));
            }
            if g.last.copying_ids.contains(id) || g.last.unreadable_ids.contains(id) {
                return Err(FILE_CHANGED.into());
            }
            if let Some(d) = e.duration.filter(|d| *d > MAX_TRANSCRIBE_SECS) {
                return Err(format!(
                    "This video is {} min long. Videos over {} min can't be transcribed yet; trim it to the sermon first.",
                    (d / 60.0).round(),
                    (MAX_TRANSCRIBE_SECS / 60.0) as u32
                ));
            }
            (paths.root.clone(), paths.original(&e.file_name)?, e.fingerprint.clone())
        };

        // Quick re-check now (size + mtime); the job re-hashes before and after.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("Can't read {}: {}", path.display(), e))?;
        if meta.len() != expected.size || fsutil::mtime_ms(&meta) != expected.mtime_ms {
            return Err(FILE_CHANGED.into());
        }
        self.add_to_queue(id, &root, &expected)
    }

    /// The second half of `enqueue`, after the file was checked without the
    /// lock: everything may have changed meanwhile (the library switched,
    /// went read-only, the entry went or its file changed), so look again.
    fn add_to_queue(&self, id: &str, root: &Path, expected: &Fingerprint) -> Result<(), String> {
        let mut g = self.inner();
        g.require_not_updating()?;
        if g.writable()?.root != root {
            return Err("The library folder changed. Try again.".into());
        }
        if !fingerprint_matches(&g.entry(id)?.fingerprint, expected) {
            return Err(FILE_CHANGED.into());
        }
        if g.is_busy(id) {
            return Err("This video is already queued or processing".into());
        }
        // Asked for again: it may be restarted after a crash again.
        g.entry_mut(id)?.process_attempts = 0;
        g.queue.push_back(id.to_string());
        if !g.manifest.queue.iter().any(|q| q == id) {
            g.manifest.queue.push(id.to_string());
        }
        g.save_logged();
        drop(g);
        self.wake.notify_one();
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let mut g = self.inner();
        g.queue.retain(|q| q != id);
        if let Some(r) = g.running.as_mut().filter(|r| r.id == id) {
            // Leaves the saved queue when the job has stopped (finish_job).
            if r.committing {
                r.late_cancel = true;
            } else {
                r.cancel.cancel();
            }
        } else if g.manifest.queue.iter().any(|q| q == id) {
            g.manifest.queue.retain(|q| q != id);
            g.save_logged();
        }
        Ok(())
    }

    fn set_progress(&self, app: &AppHandle, id: &str, stage: ProcessingStage, progress: f32, message: &str) {
        {
            let mut g = self.inner();
            if let Some(r) = g.running.as_mut().filter(|r| r.id == id) {
                r.stage = stage.clone();
                r.progress = progress;
                r.message = message.to_string();
            }
        }
        let _ = app.emit(
            "processing-progress",
            ProcessingProgressEvent {
                video_id: id.to_string(),
                stage,
                progress,
                message: message.to_string(),
            },
        );
    }

    /// Take the next queued video and mark it processing.
    fn start_next(&self) -> Option<(String, CancellationToken)> {
        let mut g = self.inner();
        while let Some(id) = g.queue.pop_front() {
            let Ok(e) = g.entry_mut(&id) else {
                g.manifest.queue.retain(|q| *q != id);
                continue;
            };
            let previous = e.status;
            e.status = EntryStatus::Processing;
            let token = CancellationToken::new();
            g.running = Some(Running {
                id: id.clone(),
                stage: ProcessingStage::ExtractingAudio,
                progress: 0.0,
                message: "Starting...".into(),
                cancel: token.clone(),
                previous,
                committing: false,
                late_cancel: false,
            });
            g.save_logged();
            return Some((id, token));
        }
        None
    }

    /// Record how the running job ended (None: cancelled). A failed
    /// re-process of a video that already had results keeps them (run_job
    /// only replaces them when it succeeds) and its status, with the error
    /// shown on its card. Returns a notice for a cancel that came during
    /// the commit, too late to stop it. The job's work folder is already
    /// gone (`StagingDir`).
    fn finish_job(&self, id: &str, result: &Option<Result<(), String>>) -> Option<&'static str> {
        {
            let mut g = self.inner();
            let running = g.running.take();
            let late = running.as_ref().is_some_and(|r| r.late_cancel) && matches!(result, Some(Ok(())));
            let previous = running.map(|r| r.previous).unwrap_or(EntryStatus::New);
            g.manifest.queue.retain(|q| q != id);
            if let Ok(e) = g.entry_mut(id) {
                match result {
                    Some(Ok(())) => e.process_attempts = 0,
                    Some(Err(err)) if matches!(previous, EntryStatus::Done | EntryStatus::ReadyForReview) => {
                        e.status = previous;
                        e.error = Some(format!("Re-processing failed: {}", err));
                    }
                    Some(Err(err)) => {
                        e.status = EntryStatus::Error;
                        e.error = Some(err.clone());
                    }
                    None => {
                        e.status = if previous == EntryStatus::Processing { EntryStatus::New } else { previous };
                        // A cancelled retry still shows why it failed before.
                        if e.status != EntryStatus::Error {
                            e.error = None;
                        }
                    }
                }
            }
            g.save_logged();
            late.then_some(LATE_CANCEL)
        }
    }

    /// Runs queued jobs one at a time, forever.
    pub async fn worker(self: Arc<Self>, app: AppHandle, app_state: AppStateHandle) {
        loop {
            let Some((id, token)) = self.start_next() else {
                self.wake.notified().await;
                continue;
            };
            emit_changed(&app);

            let result = tokio::select! {
                r = self.run_job(&app, &app_state, &id) => Some(r),
                _ = token.cancelled() => None,
            };
            // A job that saw the cancel itself (just before its commit).
            let result = if token.is_cancelled() && !matches!(result, Some(Ok(()))) { None } else { result };
            if let Some(notice) = self.finish_job(&id, &result) {
                app_state.lock().await.notices.push(notice.to_string());
            }

            if let Some(Ok(())) = &result {
                let title = self.title_of(&id);
                crate::notify::notify(&app, crate::notify::Event::Processed { title: &title });
            }
            if let Some(Err(err)) = &result {
                eprintln!("[process] {} failed: {}", id, err);
                let _ = app.emit(
                    "processing-progress",
                    ProcessingProgressEvent {
                        video_id: id.clone(),
                        stage: ProcessingStage::Error,
                        progress: 0.0,
                        message: err.clone(),
                    },
                );
            }
            emit_changed(&app);
        }
    }

    /// Extract audio → transcribe → trim → thumbnails, into a staging folder
    /// that's committed only if the file is unchanged at the end.
    async fn run_job(self: &Arc<Self>, app: &AppHandle, app_state: &AppStateHandle, id: &str) -> Result<(), String> {
        let progress = |stage: ProcessingStage, done: f32, message: &str| self.set_progress(app, id, stage, done, message);
        self.run_job_with(app_state, id, &progress, &RealTools).await
    }

    /// `run_job` with the outside tools (ffmpeg, the thumbnail model) and
    /// the progress reports given; Modal is reached at the settings' URL.
    async fn run_job_with<T: JobTools>(
        self: &Arc<Self>,
        app_state: &AppStateHandle,
        id: &str,
        progress: &(dyn Fn(ProcessingStage, f32, &str) + Send + Sync),
        tools: &T,
    ) -> Result<(), String> {
        let (paths, file_name, expected, duration, display_title) = {
            let g = self.inner();
            let paths = g.writable()?.clone();
            let e = g.entry(id)?;
            (paths, e.file_name.clone(), e.fingerprint.clone(), e.duration, e.display_title())
        };
        let (api_url, api_key) = {
            let s = app_state.lock().await;
            (s.settings.modal_api_url.clone(), s.settings.modal_api_key.clone())
        };
        let video_path = paths.original(&file_name)?;

        progress(ProcessingStage::ExtractingAudio, 0.0, "Checking the video file...");
        let check_path = video_path.clone();
        let now = blocking(move || fsutil::fingerprint(&check_path)).await??;
        if !fingerprint_matches(&now, &expected) {
            return Err(FILE_CHANGED.into());
        }

        let staging_dir = StagingDir::create(&paths, id)?;
        let staging = staging_dir.0.clone();
        let video_dir = paths.ensure_video_dir(id)?;

        let cached = manifest::load_transcript(&video_dir).filter(|t| t.file_hash == expected.hash);
        let transcription = match cached {
            Some(t) => {
                println!("[process] Using saved transcript for {}", file_name);
                t.transcription
            }
            None => {
                progress(ProcessingStage::ExtractingAudio, 5.0, "Extracting audio from video...");
                let audio = staging.join("audio.ogg");
                tools.extract_audio(&video_path, &audio).await?;

                // The server answers only when it's done, so show how long
                // it has been going.
                let started = std::time::Instant::now();
                let audio_path = path_string(&audio);
                let transcribe = crate::modal::transcribe_audio(&audio_path, &api_url, api_key.as_deref());
                tokio::pin!(transcribe);
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
                let result = loop {
                    tokio::select! {
                        r = &mut transcribe => break r,
                        _ = tick.tick() => progress(
                            ProcessingStage::Transcribing,
                            25.0,
                            &crate::modal::transcribing_message(started.elapsed()),
                        ),
                    }
                };
                let _ = std::fs::remove_file(&audio);
                let transcription = result?;
                // The one output saved straight away rather than staged: it
                // costs a paid transcription, and it's keyed to the file's
                // hash, so it only replaces a transcript of other content.
                manifest::save_transcript(
                    &video_dir,
                    &TranscriptFile {
                        file_hash: expected.hash.clone(),
                        transcription: transcription.clone(),
                    },
                )?;
                transcription
            }
        };

        progress(ProcessingStage::Trimming, 50.0, "Trimming video to sermon boundaries...");
        let duration = match duration {
            Some(d) => d,
            None => tools.duration(&video_path).await?,
        };
        let mut start = transcription.sermon_start.unwrap_or(0.0).clamp(0.0, duration);
        let mut end = transcription.sermon_end.unwrap_or(duration).clamp(0.0, duration);
        if end <= start {
            start = 0.0;
            end = duration;
        }
        let trimmed_name = trimmed_name(&file_name);
        // The cut may start at a keyframe a moment early; chapters and
        // captions are timed from where it really starts.
        let on_step = |f: f32, _: &str| {
            progress(ProcessingStage::Trimming, 50.0 + 25.0 * f, "Trimming video to sermon boundaries...");
        };
        let start = tools.trim(&video_path, start, end, &staging.join(&trimmed_name), &on_step).await?;

        progress(ProcessingStage::GeneratingThumbnails, 75.0, "Generating thumbnail options...");
        let title = transcription.suggested_title.clone().unwrap_or(display_title);
        let thumbs = tools.thumbnails(&video_path, start, end, &title, &staging).await?;
        let thumb_names: Vec<String> = thumbs
            .iter()
            .filter_map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        let check_path = video_path.clone();
        let after = blocking(move || fsutil::fingerprint(&check_path)).await??;
        if !fingerprint_matches(&after, &expected) {
            return Err("The file changed while it was being processed, so the results were thrown away. Try again once it has finished copying.".into());
        }

        let outputs = JobOutputs { trimmed_name, start, end, thumb_names, duration };
        self.commit_job(id, &video_dir, &staging, &expected.hash, outputs, || {}).await?;
        drop(staging_dir);
        progress(ProcessingStage::Complete, 100.0, "Processing complete!");
        Ok(())
    }

    /// Swap a job's outputs in and mark the video ready. Nothing else
    /// writes this folder while the video is processing (reviews, renders
    /// and thumbnail edits are refused), so the swap runs without the
    /// library lock. Once it has begun it runs to the end: a cancel then
    /// only says it came too late (see `cancel`). The review typed so far
    /// is kept. `before_swap` runs once the commit can no longer be
    /// cancelled (tests cancel there).
    async fn commit_job(
        &self,
        id: &str,
        video_dir: &Path,
        staging: &Path,
        hash: &str,
        outputs: JobOutputs,
        before_swap: impl FnOnce(),
    ) -> Result<(), String> {
        {
            let mut g = self.inner();
            g.writable()?;
            if g.entry(id)?.fingerprint.hash != hash {
                return Err(FILE_CHANGED.into());
            }
            let running = g.running.as_mut().filter(|r| r.id == id).ok_or("Cancelled")?;
            if running.cancel.is_cancelled() {
                return Err("Cancelled".into());
            }
            running.committing = true;
        }
        before_swap();
        let JobOutputs { trimmed_name, start, end, thumb_names, duration } = outputs;
        let (dir, staging) = (video_dir.to_path_buf(), staging.to_path_buf());
        // Under the video's lock, like every other writer of video.json
        // (a render being saved, a review), so none interleaves with the swap.
        let lock = self.video_lock(id);
        blocking(move || {
            let _v = hold(&lock);
            let old = manifest::load_video_data(&dir);
            let data = VideoData {
                trimmed_file: Some(trimmed_name),
                trim_start: Some(start),
                trim_end: Some(end),
                selected_source: thumb_names.first().cloned(),
                thumbnail_options: thumb_names,
                review: old.review,
                ..Default::default()
            };
            swap_in_outputs(&dir, &staging, || manifest::save_video_data(&dir, &data))
        })
        .await??;
        let mut g = self.inner();
        let e = g.entry_mut(id)?;
        e.status = EntryStatus::ReadyForReview;
        e.error = None;
        e.duration = Some(duration);
        g.save()
    }

    // -----------------------------------------------------------------------
    // Trim

    /// Cut the trimmed video again at new points (the review screen's trim
    /// panel). Refused while the video is queued, processing, uploading or
    /// otherwise busy, and busy itself until done. Returns where the new
    /// cut really starts (see `ffmpeg::trim_accurate`) and ends.
    pub async fn set_trim(self: &Arc<Self>, app: &AppHandle, id: &str, start: f64, end: f64) -> Result<(f64, f64), String> {
        let on_step = |f: f32, message: &str| {
            let _ = app.emit(
                "trim-progress",
                serde_json::json!({ "video_id": id, "progress": (f * 100.0).round(), "message": message }),
            );
        };
        let result = self.set_trim_with(id, start, end, &on_step).await;
        emit_changed(app);
        result
    }

    async fn set_trim_with(
        self: &Arc<Self>,
        id: &str,
        start: f64,
        end: f64,
        on_step: &(dyn Fn(f32, &str) + Send + Sync),
    ) -> Result<(f64, f64), String> {
        let (_busy, paths, entry, _data) = self.begin_op(id)?;
        if !matches!(entry.status, EntryStatus::ReadyForReview | EntryStatus::Done) {
            return Err("Process the video before changing where it starts and ends".into());
        }
        let end = match entry.duration {
            Some(d) => end.min(d),
            None => end,
        };
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end - start < 1.0 {
            return Err("The end must be at least a second after the start".into());
        }
        let dir = paths.ensure_video_dir(id)?;
        let video_path = paths.original(&entry.file_name)?;
        paths.ensure_dir(&paths.tmp_dir())?;
        let staging = paths.tmp_dir().join(format!("trim-{}", id));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir(&staging).map_err(|e| format!("Failed to create work folder: {}", e))?;

        let result = async {
            let name = trimmed_name(&entry.file_name);
            let out = staging.join(&name);
            let actual = crate::ffmpeg::trim_accurate(&video_path, start, end, &out, on_step).await?.start;

            let check = video_path.clone();
            let now = blocking(move || fsutil::fingerprint(&check)).await??;
            if !fingerprint_matches(&now, &entry.fingerprint) {
                return Err(FILE_CHANGED.to_string());
            }
            let (lib, id, dir, work) = (self.clone(), id.to_string(), dir.clone(), staging.clone());
            blocking(move || lib.record_trim(&id, &dir, &work, &name, actual, end)).await??;
            Ok((actual, end))
        }
        .await;
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    /// Swap the new cut staged in `staging` in and record its times in
    /// video.json, together, through the commit journal: a crash or error
    /// midway leaves the old cut with its old times. Under the video's lock
    /// like every other video.json edit (the review screen saves while a
    /// trim runs). The cut it replaces goes.
    fn record_trim(&self, id: &str, dir: &Path, staging: &Path, name: &str, start: f64, end: f64) -> Result<(), String> {
        let lock = self.video_lock(id);
        let _v = hold(&lock);
        let old = manifest::load_video_data(dir).trimmed_file;
        let replaces = |n: &str| n == name || old.as_deref() == Some(n);
        swap_in_outputs_where(dir, staging, &replaces, || {
            let mut data = manifest::load_video_data(dir);
            data.trimmed_file = Some(name.to_string());
            data.trim_start = Some(start);
            data.trim_end = Some(end);
            manifest::save_video_data(dir, &data)
        })
        // An upload session of the old cut is kept: the next upload asks
        // YouTube about it first (it may have finished; INVARIANTS #8, #9).
    }

    /// A still of the original video at `time`, for the trim panel when the
    /// video itself can't play in the app. Written to the system temp
    /// folder; older stills there are cleared out.
    pub async fn frame_still(&self, id: &str, time: f64) -> Result<String, String> {
        if !time.is_finite() || time < 0.0 {
            return Err("Invalid time".into());
        }
        let video_path = {
            let g = self.inner();
            let paths = g.paths.clone().ok_or("The library isn't open")?;
            paths.original(&g.entry(id)?.file_name)?
        };
        let dir = std::env::temp_dir().join("sermon-cut-stills");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
        if let Ok(old) = std::fs::read_dir(&dir) {
            let hour = std::time::Duration::from_secs(3600);
            for f in old.flatten() {
                let stale = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > hour);
                if stale {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        let out = dir.join(format!("{}-{}.jpg", id, (time * 1000.0).round() as u64));
        crate::ffmpeg::extract_frame(&path_string(&video_path), time, &path_string(&out)).await?;
        Ok(path_string(&out))
    }

    // -----------------------------------------------------------------------
    // Actions

    fn with_entry<T>(&self, id: &str, f: impl FnOnce(&mut Inner, &LibraryPaths) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.inner();
        let paths = g.writable()?.clone();
        g.entry(id)?;
        f(&mut g, &paths)
    }

    /// Decide under `inner` (writable, the entry exists, idle, plus
    /// `check`), then mark the video busy until the guard drops, so its
    /// files can be worked on without `inner` (docs/INVARIANTS.md #4).
    fn mark_busy(&self, id: &str, check: impl FnOnce(&Inner) -> Result<(), String>) -> Result<(BusyGuard<'_>, LibraryPaths), String> {
        let paths = self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            check(g)?;
            g.busy.insert(id.to_string());
            Ok(paths.clone())
        })?;
        Ok((BusyGuard { lib: self, id: id.to_string() }, paths))
    }

    /// Delete a video's folder under its lock. Blocking.
    fn remove_video_files(&self, paths: &LibraryPaths, id: &str) -> Result<(), String> {
        let lock = self.video_lock(id);
        let _v = hold(&lock);
        manifest::remove_video_dir(paths, id)
    }

    /// Process a video again from a new transcription: its saved transcript
    /// goes (under the video's lock, never `inner`), then it is queued. The
    /// results so far stay until the new ones are committed, and the review
    /// typed so far (title, description, speaker) is kept (`commit_job`).
    pub async fn reprocess(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let dir = self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            paths.video_dir(id)
        })?;
        let lock = self.video_lock(id);
        blocking(move || {
            let _v = hold(&lock);
            match std::fs::remove_file(dir.join("transcript.json")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove the old transcript: {}", e)),
                _ => Ok(()),
            }
        })
        .await??;
        self.enqueue(id).await
    }

    /// Back to New: delete what processing made. Keeps upload history.
    /// Blocking (deletes files): call it from a blocking thread.
    pub fn reset(&self, id: &str) -> Result<(), String> {
        let (_busy, paths) = self.mark_busy(id, |_| Ok(()))?;
        self.remove_video_files(&paths, id)?;
        let mut g = self.inner();
        let e = g.entry_mut(id).map_err(|_| ENTRY_GONE.to_string())?;
        e.status = EntryStatus::New;
        e.error = None;
        g.save()
    }

    pub fn set_hidden(&self, id: &str, hidden: bool) -> Result<(), String> {
        self.with_entry(id, |g, _| {
            g.require_idle(id)?;
            g.entry_mut(id)?.hidden = hidden;
            g.save()
        })
    }

    /// Drop a video whose file is gone. Blocking (deletes files).
    pub fn forget(&self, id: &str) -> Result<(), String> {
        let (_busy, paths) = self.mark_busy(id, |g| {
            if g.present(id) {
                return Err("The file is still in the library folder. Use Move to Trash or Hide instead.".into());
            }
            Ok(())
        })?;
        self.remove_video_files(&paths, id)?;
        let mut g = self.inner();
        // Gone already (e.g. the library was reopened meanwhile): done too.
        g.manifest.entries.retain(|e| e.id != id);
        g.save()
    }

    /// Move the user's file to the Trash, then delete the app's data for it.
    pub async fn trash(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.trash_with(id, move_to_trash).await
    }

    async fn trash_with(self: &Arc<Self>, id: &str, mover: fn(&Path) -> Result<(), String>) -> Result<(), String> {
        // A scan that listed the file just before it went would add it back
        // as a new video once the entry is gone.
        let _scan = self.scan_lock.lock().await;
        let (_busy, paths) = self.mark_busy(id, |_| Ok(()))?;
        let path = paths.original(&self.inner().entry(id)?.file_name)?;
        let (lib, check, video) = (self.clone(), path.clone(), id.to_string());
        let result = blocking(move || -> Result<(), String> {
            paths.verify_original(&check)?;
            mover(&check)?;
            if let Err(e) = lib.remove_video_files(&paths, &video) {
                eprintln!("[library] {}", e);
            }
            Ok(())
        })
        .await
        .and_then(|r| r);

        result.map_err(|e| {
            format!(
                "Couldn't move \"{}\" to the Trash: {}. Some network and external drives have no Trash; delete the file in Finder if you want it gone.",
                path.file_name().unwrap_or_default().to_string_lossy(),
                e
            )
        })?;
        let mut g = self.inner();
        // Gone already (the library was reopened meanwhile): nothing to drop.
        g.manifest.entries.retain(|e| e.id != id);
        g.last.present.remove(id);
        g.save()
    }

    /// Save the review screen's edits. `selected_source` is a thumbnail
    /// source id; one that's no longer an option (e.g. regenerated meanwhile)
    /// is ignored rather than losing the text edits.
    /// Blocking (reads and writes video.json).
    pub fn save_review(&self, id: &str, review: Review, selected_source: Option<String>) -> Result<(), String> {
        // The video's lock, held throughout, keeps a reset, forget or trash
        // (which delete the folder under it) from interleaving.
        let lock = self.video_lock(id);
        let _v = hold(&lock);
        let paths = self.with_entry(id, |g, paths| {
            // Processing replaces video.json, so edits made now would be lost.
            if g.queue.iter().any(|q| q == id) || g.running.as_ref().is_some_and(|r| r.id == id) {
                return Err("This video is being processed".into());
            }
            Ok(paths.clone())
        })?;
        let dir = paths.ensure_video_dir(id)?;
        let mut data = manifest::load_video_data(&dir);
        data.review = review;
        if let Some(source) = selected_source.as_deref().and_then(source_id) {
            if data.thumbnail_options.contains(&source) {
                data.selected_source = Some(source);
            }
        }
        manifest::save_video_data(&dir, &data)
    }

    /// Mark an id busy for an operation that runs outside the lock, until
    /// the returned guard is dropped.
    fn begin_op(&self, id: &str) -> Result<(BusyGuard<'_>, LibraryPaths, Entry, VideoData), String> {
        let (paths, e) = self.with_entry(id, |g, paths| {
            g.require_not_updating()?;
            g.require_idle(id)?;
            let e = g.entry(id)?.clone();
            g.busy.insert(id.to_string());
            Ok((paths.clone(), e))
        })?;
        let busy = BusyGuard { lib: self, id: id.to_string() };
        let data = manifest::load_video_data(&paths.video_dir(id)?);
        Ok((busy, paths, e, data))
    }

    pub async fn regenerate_thumbnails(self: &Arc<Self>, id: &str) -> Result<Vec<String>, String> {
        let (_busy, paths, entry, data) = self.begin_op(id)?;
        async {
            let dir = paths.ensure_video_dir(id)?;
            let transcript = manifest::load_transcript(&dir).map(|t| t.transcription);
            let start = data.trim_start.or(transcript.as_ref().and_then(|t| t.sermon_start)).unwrap_or(0.0);
            let end = data
                .trim_end
                .or(transcript.as_ref().and_then(|t| t.sermon_end))
                .or(entry.duration)
                .unwrap_or(0.0);
            let title = data
                .review
                .title
                .clone()
                .or(transcript.and_then(|t| t.suggested_title))
                .unwrap_or_else(|| entry.display_title());

            paths.ensure_dir(&paths.tmp_dir())?;
            let staging = paths.tmp_dir().join(format!("thumbs-{}", id));
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir(&staging).map_err(|e| format!("Failed to create work folder: {}", e))?;
            let video_path = paths.original(&entry.file_name)?;
            let generated =
                crate::thumbnail::generate_thumbnail_options(&video_path, start, end, &title, &staging, 10).await;
            let names = generated.map(|thumbs| {
                thumbs
                    .iter()
                    .filter_map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()))
                    .collect::<Vec<_>>()
            });
            let names = match names {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&staging);
                    return Err(e);
                }
            };

            // Under the video's lock, on a blocking thread, never `inner`.
            let lock = self.video_lock(id);
            let (commit_dir, commit_staging, commit_names) = (dir.clone(), staging.clone(), names.clone());
            let committed = blocking(move || {
                let _v = hold(&lock);
                commit_regenerated(&commit_dir, &commit_staging, &commit_names)
            })
            .await
            .and_then(|r| r);
            let _ = std::fs::remove_dir_all(&staging);
            committed?;
            Ok(names.iter().map(|n| path_string(&dir.join(n))).collect())
        }
        .await
    }

    pub async fn custom_thumbnail(
        self: &Arc<Self>,
        id: &str,
        source: PathBuf,
        crop: crate::thumbnail::CropRect,
        grading: bool,
    ) -> Result<String, String> {
        let (_busy, paths, _entry, _data) = self.begin_op(id)?;
        async {
            let dir = paths.ensure_video_dir(id)?;
            let path = crate::thumbnail::process_custom_thumbnail(&source, &dir, &crop, grading).await?;
            let name = file_in_dir(&dir, &path)?;
            // Under the video's lock, on a blocking thread, never `inner`
            // (the video is busy, so its entry stays).
            let lock = self.video_lock(id);
            let (commit_dir, added) = (dir.clone(), name.clone());
            blocking(move || {
                let _v = hold(&lock);
                let mut data = manifest::load_video_data(&commit_dir);
                if !data.thumbnail_options.contains(&added) {
                    data.thumbnail_options.insert(0, added);
                }
                // The selection is by id, so adding a source never moves it; the
                // review screen selects the new one itself.
                let pruned = prune_custom_thumbnails(&mut data.thumbnail_options, data.selected_source.as_deref());
                manifest::save_video_data(&commit_dir, &data)?;
                for old in pruned {
                    let _ = std::fs::remove_file(commit_dir.join(old));
                }
                // Renders of the dropped sources.
                manifest::prune_renders(&commit_dir, &data);
                Ok::<_, String>(())
            })
            .await??;
            Ok(path_string(&dir.join(name)))
        }
        .await
    }

    /// Persist a template render of one thumbnail source, replacing that
    /// source's previous render. Never changes the selection. Allowed while
    /// the video uploads (the review screen stays usable), refused while it's
    /// queued or processing, which replaces every thumbnail.
    pub async fn save_rendered_thumbnail(
        self: &Arc<Self>,
        id: &str,
        source: &str,
        render_key: &str,
        template_version: u32,
        logo: &str,
        image_bytes: Vec<u8>,
    ) -> Result<String, String> {
        if !valid_render_key(render_key) {
            return Err("Invalid render key".into());
        }
        let source = source_id(source).ok_or_else(|| format!("Invalid thumbnail: {}", source))?;
        // The files are read and written under the video's lock, on a
        // blocking thread, never under `inner` (docs/INVARIANTS.md #4);
        // `inner` only decides.
        let paths = self.with_entry(id, |g, paths| {
            g.require_render_allowed(id)?;
            Ok(paths.clone())
        })?;
        let job = RenderSave {
            lib: self.clone(),
            id: id.to_string(),
            source,
            render_key: render_key.to_string(),
            template_version,
            logo: logo.to_string(),
        };
        let dir = {
            let job = job.clone();
            blocking(move || {
                let lock = job.lib.video_lock(&job.id);
                let _v = hold(&lock);
                let dir = paths.ensure_video_dir(&job.id)?;
                job.check(&dir, &manifest::load_video_data(&dir))?;
                Ok::<_, String>(dir)
            })
            .await??
        };

        // Named here, never by the webview. Written under a hidden temp name
        // (swept on the next open if we crash) that no save or prune touches.
        let name = manifest::render_file_name(&job.source);
        let tmp = dir.join(format!("{}{}", manifest::RENDER_TMP_PREFIX, name));
        let out = tmp.clone();
        let rendering = RenderingGuard::new(self);
        let written = blocking(move || crate::thumbnail::save_for_youtube(&image_bytes, &out)).await.and_then(|r| r);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // Moved into place and recorded together, under the video's lock, so
        // no prune or commit can see the file unrecorded; then the replaced
        // render goes.
        let committed = {
            let (dir, name, tmp) = (dir.clone(), name.clone(), tmp.clone());
            blocking(move || {
                let lock = job.lib.video_lock(&job.id);
                let _v = hold(&lock);
                let mut data = manifest::load_video_data(&dir);
                job.check(&dir, &data)?;
                std::fs::rename(&tmp, dir.join(&name)).map_err(|e| format!("Failed to save thumbnail: {}", e))?;
                data.renders.retain(|r| r.source != job.source);
                data.renders.push(Render { source: job.source.clone(), file: name.clone(), render_key: job.render_key.clone() });
                if let Err(e) = manifest::save_video_data(&dir, &data) {
                    let _ = std::fs::remove_file(dir.join(&name));
                    return Err(e);
                }
                manifest::prune_renders(&dir, &data);
                Ok(())
            })
            .await
            .and_then(|r| r)
        };
        drop(rendering);
        if let Err(e) = committed {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(path_string(&dir.join(name)))
    }

    /// Everything an upload needs, with the video marked busy. The thumbnail
    /// is `thumbnail_source`'s render when its key is `render_key` (what the
    /// review screen showed), else the source itself; with no source given,
    /// the selected one's render. Its bytes are read now: renders are allowed
    /// during an upload and replace (delete) the file it came from.
    pub fn begin_upload(
        &self,
        id: &str,
        thumbnail_source: Option<String>,
        render_key: Option<String>,
    ) -> Result<UploadJob<'_>, String> {
        let (busy, paths, entry, data) = self.begin_op(id)?;
        type Prepared = (PathBuf, Option<(PathBuf, Vec<u8>)>, Option<Vec<crate::state::TranscriptSegment>>);
        let prepare = || -> Result<Prepared, String> {
            let dir = paths.video_dir(id)?;
            let video_path = match data.trimmed_file.as_ref().map(|n| dir.join(n)).filter(|p| p.exists()) {
                Some(p) => p,
                None => paths.original(&entry.file_name)?,
            };
            let thumbnail = match resolve_thumbnail(&dir, &data, thumbnail_source.as_deref(), render_key.as_deref())? {
                Some(p) => {
                    let bytes = std::fs::read(&p).map_err(|e| format!("Couldn't read the thumbnail: {}", e))?;
                    Some((p, bytes))
                }
                None => None,
            };
            let segments = manifest::load_transcript(&dir)
                .filter(|t| t.file_hash == entry.fingerprint.hash)
                .map(|t| t.transcription.segments);
            Ok((video_path, thumbnail, segments))
        };
        // On an error, dropping the guard frees the video again.
        let (video_path, thumbnail, segments) = prepare()?;
        let cancel = CancellationToken::new();
        let active = ActiveUpload { percent: 0, cancel: cancel.clone() };
        self.inner().uploads.insert(id.to_string(), active);
        let (thumbnail_path, thumbnail_data) = thumbnail.unzip();
        Ok(UploadJob {
            video_path,
            thumbnail_path,
            thumbnail_data,
            segments,
            file_hash: entry.fingerprint.hash.clone(),
            cancel,
            _busy: busy,
        })
    }

    pub fn set_upload_progress(&self, id: &str, percent: u32) {
        if let Some(u) = self.inner().uploads.get_mut(id) {
            u.percent = percent;
        }
    }

    /// Stop an upload. Its session is kept, so the next try continues it.
    pub fn cancel_upload(&self, id: &str) -> Result<(), String> {
        match self.inner().uploads.get(id) {
            Some(u) => {
                u.cancel.cancel();
                Ok(())
            }
            None => Err("This video isn't uploading.".into()),
        }
    }

    /// The entry's unfinished upload session.
    pub fn upload_session(&self, id: &str) -> Option<UploadSession> {
        self.inner().manifest.get(id).and_then(|e| e.upload_session.clone())
    }

    /// Keep (or clear) the entry's upload session in library.json.
    pub fn set_upload_session(&self, id: &str, session: Option<&UploadSession>) {
        let mut g = self.inner();
        let Ok(e) = g.entry_mut(id) else { return };
        if e.upload_session.as_ref() == session {
            return;
        }
        e.upload_session = session.cloned();
        g.save_logged();
    }

    /// Forget the stored upload session at the user's word, when it can't be
    /// checked with YouTube (e.g. it belongs to another account). The one
    /// way a session goes without a definite answer (docs/INVARIANTS.md #8):
    /// the next upload starts a new one, which may duplicate the earlier
    /// upload if that had finished. Refused while the video is busy.
    pub fn forget_upload_session(&self, id: &str) -> Result<(), String> {
        let mut g = self.inner();
        g.writable()?;
        if g.is_busy(id) {
            return Err("Wait until this video's upload or processing has stopped.".into());
        }
        let e = g.entry_mut(id)?;
        if e.upload_session.take().is_none() {
            return Ok(());
        }
        g.save()
    }

    /// The video is on YouTube: record it and drop the session, in one
    /// save, before the post-upload steps. Recording the same video twice
    /// keeps one record.
    pub fn record_upload(&self, id: &str, file_hash: &str, options: &UploadOptions, video_id: &str) {
        let mut g = self.inner();
        let Ok(e) = g.entry_mut(id) else { return };
        if !e.uploads.iter().any(|u| u.video_id.as_deref() == Some(video_id)) {
            e.uploads.push(UploadRecord {
                url: crate::upload::watch_url(video_id),
                uploaded_at: chrono::Utc::now().to_rfc3339(),
                file_hash: file_hash.to_string(),
                privacy: Some(options.privacy.clone()),
                playlist_id: options.playlist_id.clone(),
                video_id: Some(video_id.to_string()),
                warnings: Vec::new(),
            });
        }
        e.upload_session = None;
        e.status = EntryStatus::Done;
        e.error = None;
        g.save_logged();
    }

    /// End an upload; with an outcome it succeeded. Its record (made by
    /// `record_upload` when the video went up) gets the warnings.
    pub fn finish_upload(&self, id: &str, job: UploadJob<'_>, outcome: Option<&UploadOutcome>, options: &UploadOptions) {
        if let Some(outcome) = outcome {
            self.record_upload(id, &job.file_hash, options, &outcome.video_id);
        }
        let mut g = self.inner();
        g.uploads.remove(id);
        let Some(outcome) = outcome else { return };
        if let Ok(e) = g.entry_mut(id) {
            if let Some(rec) = e.uploads.iter_mut().rev().find(|u| u.video_id.as_deref() == Some(&outcome.video_id)) {
                rec.warnings = outcome.warnings.clone();
            }
        }
        g.save_logged();
        // Recorded first, then free (the job's guard): a second upload can't
        // start without seeing this one.
        drop(g);
        drop(job);
    }

    /// Whether `youtube_id` is one of the entry's recorded uploads.
    pub fn has_upload(&self, id: &str, youtube_id: &str) -> bool {
        let g = self.inner();
        g.manifest.get(id).is_some_and(|e| e.uploads.iter().any(|u| u.video_id.as_deref() == Some(youtube_id)))
    }

    /// For "Set thumbnail again": the last upload's YouTube id and the
    /// thumbnail to send (the selected one), with the video marked busy.
    /// Finish with `finish_thumbnail_retry`.
    pub fn begin_thumbnail_retry(&self, id: &str) -> Result<(String, PathBuf, BusyGuard<'_>), String> {
        let (busy, paths, entry, data) = self.begin_op(id)?;
        let prepare = || -> Result<(String, PathBuf), String> {
            let video_id = entry
                .uploads
                .last()
                .ok_or("This video hasn't been uploaded yet.")?
                .video_id
                .clone()
                .ok_or("This upload was made by an older version of Sermon Cut. Set the thumbnail in YouTube Studio instead.")?;
            let dir = paths.video_dir(id)?;
            let thumbnail = resolve_thumbnail(&dir, &data, None, None)?.ok_or("This video has no thumbnail to send.")?;
            Ok((video_id, thumbnail))
        };
        let (video_id, thumbnail) = prepare()?;
        Ok((video_id, thumbnail, busy))
    }

    /// After "Set thumbnail again": on success the thumbnail warning goes.
    /// Returns the upload's remaining warnings.
    pub fn finish_thumbnail_retry(&self, busy: BusyGuard<'_>, id: &str, video_id: &str, ok: bool) -> Vec<UploadWarning> {
        drop(busy);
        let mut g = self.inner();
        let mut remaining = Vec::new();
        if let Ok(e) = g.entry_mut(id) {
            if let Some(rec) = e.uploads.iter_mut().rev().find(|u| u.video_id.as_deref() == Some(video_id)) {
                if ok {
                    rec.warnings.retain(|w| w.kind != WarningKind::Thumbnail);
                }
                remaining = rec.warnings.clone();
            }
        }
        if ok {
            g.save_logged();
        }
        remaining
    }

    /// Smoke-test hook: pretend an upload is running (nothing is sent).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn hook_fake_upload(&self, id: &str) {
        let active = ActiveUpload { percent: 0, cancel: CancellationToken::new() };
        self.inner().uploads.insert(id.to_string(), active);
    }

    /// What would be lost by quitting now, for the quit warning.
    pub fn work_in_progress(&self) -> Option<String> {
        self.inner().work_in_progress()
    }

    /// Before installing an app update: refused while anything is running,
    /// otherwise no new processing, upload or download starts until
    /// `end_update` (or the relaunch).
    pub fn begin_update(&self) -> Result<(), String> {
        let mut g = self.inner();
        if let Some(work) = g.work_in_progress() {
            return Err(format!(
                "Sermon Cut is still working on {}. Update when it has finished (or cancel it first).",
                work
            ));
        }
        g.updating = true;
        Ok(())
    }

    /// The update failed: work may start again.
    pub fn end_update(&self) {
        self.inner().updating = false;
    }

    pub fn reveal_path(&self, id: &str) -> Result<PathBuf, String> {
        let g = self.inner();
        let paths = g.paths.clone().ok_or("The library isn't open")?;
        let e = g.entry(id)?;
        let original = paths.original(&e.file_name)?;
        let dir = paths.video_dir(id)?;
        let trimmed = manifest::load_video_data(&dir).trimmed_file.map(|n| dir.join(n));
        Ok(trimmed.filter(|p| p.exists()).unwrap_or(original))
    }

    /// What "Show leftovers" shows: a commit's backup folder that couldn't
    /// be put back (`STUCK_COMMIT`), else the video's folder.
    pub fn commit_backup_path(&self, id: &str) -> Result<PathBuf, String> {
        let paths = self.inner().paths.clone().ok_or("The library isn't open")?;
        self.inner().entry(id)?;
        let dir = paths.video_dir(id)?;
        let backup = dir.join(COMMIT_BACKUP);
        Ok(if backup.exists() { backup } else { dir })
    }

    /// Delete .sermon-cut entirely (never the videos) and start fresh.
    pub async fn clear_data(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let paths = {
            let g = self.inner();
            let paths = g.writable()?.clone();
            if !g.is_idle() {
                return Err("Wait for processing, downloads and uploads to finish first.".into());
            }
            paths
        };
        let _lock_file = self.lock_file_mutex.lock().await;
        let lib = self.clone();
        blocking(move || {
            let data = paths.data_dir();
            if std::fs::symlink_metadata(&data).is_ok_and(|m| m.is_dir()) {
                std::fs::remove_dir_all(&data).map_err(|e| format!("Failed to delete {}: {}", data.display(), e))?;
            }
            lib.open_blocking(false);
            lib.inner().baseline_scan = true;
            Ok(())
        })
        .await?
    }

    // -----------------------------------------------------------------------
    // Downloads

    /// Download into .sermon-cut/tmp, then move the finished file into the
    /// library under a readable name. Returns the new entry's id.
    pub async fn download(
        self: &Arc<Self>,
        app: &AppHandle,
        url: &str,
        quality: &str,
    ) -> Result<String, String> {
        // Listed (and so blocking a folder switch) from the start, while the
        // details are fetched too.
        let job_id = super::new_id();
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let paths = {
            let mut g = self.inner();
            g.require_not_updating()?;
            let paths = g.writable()?.clone();
            g.downloads.push(DownloadItem {
                job_id: job_id.clone(),
                youtube_id: String::new(),
                title: "Getting video details…".into(),
                cancel: Some(cancel_tx),
            });
            paths
        };
        emit_changed(app);

        let work = paths.tmp_dir().join(format!("dl-{}", job_id));
        let result = async {
            let info = tokio::select! {
                info = crate::youtube_dl::get_video_info(url) => info?,
                _ = cancel_rx.wait_for(|cancelled| *cancelled) => return Err("Download cancelled".to_string()),
            };
            {
                let mut g = self.inner();
                if let Some(d) = g.downloads.iter_mut().find(|d| d.job_id == job_id) {
                    d.youtube_id = info.id.clone();
                    d.title = info.title.clone();
                }
            }
            emit_changed(app);

            paths.ensure_dir(&paths.tmp_dir())?;
            std::fs::create_dir_all(&work).map_err(|e| format!("Failed to create download folder: {}", e))?;
            let file = crate::youtube_dl::download_video(url, quality, &info.id, &work, cancel_rx, app.clone()).await?;
            let check = file.clone();
            let fingerprint = blocking(move || fsutil::fingerprint(&check)).await??;
            let duration = match info.duration {
                Some(d) => Some(d),
                None => crate::ffmpeg::get_duration(&path_string(&file)).await.ok(),
            };
            let ext = file
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| "mp4".into());

            // Under the scan lock from before the move until the entry is
            // saved: a scan that listed the folder before the move can't mark
            // the new entry missing, and no scan can see the file before it
            // has its YouTube details. The move itself runs outside `inner`.
            let _scan = self.scan_lock.lock().await;
            self.inner().writable()?;
            let (src, root) = (file.clone(), paths.root.clone());
            let wanted = download_file_name(&info.title, &info.id, &ext);
            let name = blocking(move || move_into_library(&src, &root, &wanted)).await??;
            let mut g = self.inner();
            let mut entry = Entry::new(name, fingerprint, duration);
            entry.title = Some(info.title.clone());
            entry.source_url = Some(info.url.clone());
            entry.youtube_id = Some(info.id.clone());
            entry.channel = info.channel.clone();
            let id = entry.id.clone();
            g.last.present.insert(id.clone());
            g.manifest.entries.push(entry);
            g.save()?;
            Ok(id)
        }
        .await;

        let _ = std::fs::remove_dir_all(&work);
        self.inner().downloads.retain(|d| d.job_id != job_id);
        emit_changed(app);
        result
    }

    pub fn cancel_download(&self, youtube_id: &str) {
        let g = self.inner();
        if youtube_id.is_empty() {
            return;
        }
        for d in g.downloads.iter().filter(|d| d.youtube_id == youtube_id || d.job_id == youtube_id) {
            if let Some(tx) = &d.cancel {
                let _ = tx.send(true);
            }
        }
    }
}

/// "trimmed.<ext>", keeping the original's container.
fn trimmed_name(file_name: &str) -> String {
    let ext = Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| super::VIDEO_EXTENSIONS.contains(&e.as_str()))
        .unwrap_or_else(|| "mp4".into());
    format!("trimmed.{}", ext)
}

/// Where and how a video goes on YouTube.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadOptions {
    pub privacy: String,
    pub playlist_id: Option<String>,
}

impl UploadOptions {
    /// The review screen's choices, with the settings filling in what it
    /// didn't send. An empty playlist means "none" (not the default one).
    pub fn resolve(
        settings: &crate::state::Settings,
        privacy: Option<String>,
        playlist_id: Option<String>,
    ) -> Result<Self, String> {
        let privacy = privacy.unwrap_or_else(|| settings.upload_privacy.clone());
        if !crate::state::UPLOAD_PRIVACIES.contains(&privacy.as_str()) {
            return Err(format!("Unknown privacy setting: {}", privacy));
        }
        let playlist_id = match playlist_id {
            Some(explicit) => Some(explicit),
            None => settings.youtube_playlist_id.clone(),
        }
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
        Ok(UploadOptions { privacy, playlist_id })
    }
}

pub struct UploadJob<'a> {
    pub video_path: PathBuf,
    /// Where the thumbnail came from; the file may be gone by the time it's
    /// sent.
    pub thumbnail_path: Option<PathBuf>,
    /// The thumbnail as it was when the upload started.
    pub thumbnail_data: Option<Vec<u8>>,
    pub segments: Option<Vec<crate::state::TranscriptSegment>>,
    pub file_hash: String,
    pub cancel: CancellationToken,
    /// The video stays busy until the job is finished or dropped.
    _busy: BusyGuard<'a>,
}

/// Keeps a video marked busy (no processing, reset, trash or second upload)
/// until dropped, including on an early return or a panic.
pub struct BusyGuard<'a> {
    lib: &'a Library,
    id: String,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.lib.inner();
        g.busy.remove(&self.id);
        g.uploads.remove(&self.id);
    }
}

/// A render being saved (see `save_rendered_thumbnail`).
#[derive(Clone)]
struct RenderSave {
    lib: Arc<Library>,
    id: String,
    source: String,
    render_key: String,
    template_version: u32,
    logo: String,
}

impl RenderSave {
    /// Whether the render may still be saved: the video isn't being
    /// processed (asked of `inner`, briefly), its source is still an option,
    /// and its key is the saved review's. Called holding the video's lock.
    fn check(&self, dir: &Path, data: &VideoData) -> Result<(), String> {
        self.lib.with_entry(&self.id, |g, _| g.require_render_allowed(&self.id))?;
        if !data.thumbnail_options.contains(&self.source) {
            return Err("That thumbnail is no longer one of this video's options".into());
        }
        check_render_key(dir, data, &self.source, self.template_version, &self.logo, &self.render_key)
    }
}

/// Counts a render being written, so a reopen doesn't sweep its temp file.
struct RenderingGuard<'a> {
    lib: &'a Library,
}

impl<'a> RenderingGuard<'a> {
    fn new(lib: &'a Library) -> Self {
        lib.inner().rendering += 1;
        RenderingGuard { lib }
    }
}

impl Drop for RenderingGuard<'_> {
    fn drop(&mut self) {
        self.lib.inner().rendering -= 1;
    }
}

/// The upload session kept on a library entry.
pub struct EntrySession<'a> {
    pub lib: &'a Library,
    pub id: &'a str,
    /// What the finished upload is recorded with.
    pub options: &'a UploadOptions,
}

impl SessionStore for EntrySession<'_> {
    fn load(&self) -> Option<UploadSession> {
        self.lib.upload_session(self.id)
    }
    fn save(&self, session: Option<&UploadSession>) {
        self.lib.set_upload_session(self.id, session);
    }
    fn complete(&self, video_id: &str, file_hash: &str) {
        self.lib.record_upload(self.id, file_hash, self.options, video_id);
    }
}

/// A thumbnail source id from what the webview sent: a file name, or a path
/// whose file name is used.
fn source_id(s: &str) -> Option<String> {
    Path::new(s)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| super::is_valid_file_name(n))
}

fn valid_render_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The plain crop written next to a generated option
/// ("thumbnail_option_3.jpg" → "thumbnail_option_3_raw.jpg").
fn raw_variant(id: &str) -> Option<String> {
    let stem = id.strip_suffix(".jpg")?;
    let n = stem.strip_prefix("thumbnail_option_")?;
    n.bytes().all(|b| b.is_ascii_digit()).then(|| format!("{}_raw.jpg", stem))
}

/// Thumbnail sources the user added; regenerating keeps them.
fn is_custom_thumbnail(name: &str) -> bool {
    name.starts_with("custom_thumbnail_")
}

/// Swap regenerated thumbnail options (staged as thumbnail_option_N.jpg and
/// their plain crops) in for the generated ones, through the commit journal,
/// so a crash leaves the old set or the new one. Custom thumbnails and their
/// renders stay. A selected custom one stays selected; a selected generated
/// one becomes the new option at the same place (else the first). The
/// replaced options' renders go in the same commit. Call it under the
/// video's lock.
fn commit_regenerated(dir: &Path, staging: &Path, names: &[String]) -> Result<(), String> {
    let plan = RegeneratePlan::new(&manifest::load_video_data(dir), names);
    swap_in_outputs_where(dir, staging, &|n| plan.replaces(n), || manifest::save_video_data(dir, &plan.data))?;
    manifest::prune_renders(dir, &plan.data);
    Ok(())
}

/// What regenerating changes (see `commit_regenerated`).
struct RegeneratePlan {
    /// The new video.json.
    data: VideoData,
    /// The generated options being replaced, and their renders' files.
    replaced: HashSet<String>,
    replaced_renders: HashSet<String>,
    trimmed: Option<String>,
}

impl RegeneratePlan {
    fn new(old: &VideoData, names: &[String]) -> Self {
        let replaced: HashSet<String> = old.thumbnail_options.iter().filter(|o| !is_custom_thumbnail(o)).cloned().collect();
        let replaced_renders = old.renders.iter().filter(|r| replaced.contains(&r.source)).map(|r| r.file.clone()).collect();
        let generated: Vec<&String> = old.thumbnail_options.iter().filter(|o| replaced.contains(*o)).collect();
        let selected = match old.selected_or_first() {
            Some(s) if !replaced.contains(s) => Some(s.clone()),
            Some(s) => generated.iter().position(|g| *g == s).and_then(|i| names.get(i)).or(names.first()).cloned(),
            None => names.first().cloned(),
        };
        let mut data = old.clone();
        data.thumbnail_options = old
            .thumbnail_options
            .iter()
            .filter(|o| !replaced.contains(*o))
            .chain(names.iter())
            .cloned()
            .collect();
        data.renders.retain(|r| !replaced.contains(&r.source));
        data.selected_source = selected;
        RegeneratePlan { data, replaced, replaced_renders, trimmed: old.trimmed_file.clone() }
    }

    /// Whether the file `name` in the video's folder is replaced (moved aside,
    /// then deleted): the generated options with their plain crops, and
    /// their renders. Never the trimmed video.
    fn replaces(&self, name: &str) -> bool {
        self.trimmed.as_deref() != Some(name)
            && (name.starts_with("thumbnail_option_") || self.replaced.contains(name) || self.replaced_renders.contains(name))
    }
}

/// What the template draws over for source `id` (its plain crop, if it's a
/// generated option that has one), and that file's version (its mtime), part
/// of the render key.
fn render_from(dir: &Path, id: &str) -> (PathBuf, String) {
    let raw = raw_variant(id).map(|r| dir.join(r)).filter(|p| p.is_file());
    let from = raw.unwrap_or_else(|| dir.join(id));
    let version = std::fs::metadata(&from)
        .map(|m| fsutil::mtime_ms(&m).to_string())
        .unwrap_or_default();
    (from, version)
}

/// Refuse a render whose key isn't the one the saved review gives for this
/// source now: it was drawn with a title, speaker or image that has changed
/// since. Keys, not counters: inputs that go A→B→A make A's render current.
fn check_render_key(dir: &Path, data: &VideoData, source: &str, template_version: u32, logo: &str, key: &str) -> Result<(), String> {
    let (_, version) = render_from(dir, source);
    let r = &data.review;
    let expected = manifest::render_key(
        template_version,
        r.title.as_deref().unwrap_or(""),
        r.preacher.as_deref().unwrap_or(""),
        r.single_line,
        source,
        &version,
        logo,
    );
    if expected != key {
        return Err(STALE_RENDER.into());
    }
    Ok(())
}

const STALE_RENDER: &str = "The title, speaker, logo or image changed while the thumbnail was being drawn. It will be drawn again.";

/// Which file to send to YouTube as the thumbnail (see `begin_upload`).
fn resolve_thumbnail(
    dir: &Path,
    data: &VideoData,
    source: Option<&str>,
    render_key: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let source = match source {
        Some(s) => {
            let id = source_id(s).filter(|id| data.thumbnail_options.contains(id));
            Some(id.ok_or_else(|| format!("{} isn't one of this video's thumbnails", s))?)
        }
        None => data.selected_or_first().cloned(),
    };
    let Some(source) = source else { return Ok(None) };
    let render = data
        .render_for(&source)
        .filter(|r| render_key.is_none_or(|k| k == r.render_key))
        .map(|r| dir.join(&r.file))
        .filter(|p| p.is_file());
    let path = render.unwrap_or_else(|| dir.join(&source));
    Ok(path.is_file().then_some(path))
}

/// Resolve a path the frontend sent back to a file directly inside `dir`.
/// Custom thumbnails kept per video, besides the selected one.
const KEEP_CUSTOM_THUMBNAILS: usize = 3;

/// Drop all but the newest custom thumbnails (they're added at the front)
/// and the selected one from `options`; returns the dropped names.
fn prune_custom_thumbnails(options: &mut Vec<String>, selected: Option<&str>) -> Vec<String> {
    let mut customs = 0;
    let mut pruned = Vec::new();
    options.retain(|name| {
        if !name.starts_with("custom_thumbnail_") {
            return true;
        }
        customs += 1;
        if customs <= KEEP_CUSTOM_THUMBNAILS || Some(name.as_str()) == selected {
            return true;
        }
        pruned.push(name.clone());
        false
    });
    pruned
}

fn file_in_dir(dir: &Path, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| super::is_valid_file_name(n))
        .ok_or_else(|| format!("Invalid file: {}", path))?;
    let parent_ok = fsutil::is_directly_in(p, dir);
    if !parent_ok || !dir.join(&name).is_file() {
        return Err(format!("{} isn't one of this video's files", path));
    }
    Ok(name)
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    // NSFileManager needs no Automation permission (the Finder method asks
    // again after every update of an ad-hoc signed app).
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

/// "<title> [<id>].<ext>", safe on every filesystem and at most 240 bytes.
pub fn download_file_name(title: &str, youtube_id: &str, ext: &str) -> String {
    let suffix = format!(" [{}].{}", youtube_id, ext);
    let mut clean: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = clean.trim_start_matches('.').trim().to_string();
    let budget = 240usize.saturating_sub(suffix.len());
    let mut end = clean.len().min(budget);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let base = clean[..end].trim_end();
    let base = if base.is_empty() { "Video" } else { base };
    format!("{}{}", base, suffix)
}

/// Move a finished download into the library without overwriting anything:
/// "Name.mp4", then "Name (2).mp4", … Returns the final name.
fn move_into_library(src: &Path, root: &Path, name: &str) -> Result<String, String> {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    for n in 1..1000 {
        let candidate = if n == 1 { name.to_string() } else { format!("{} ({}).{}", stem, n, ext) };
        let dst = root.join(&candidate);
        // hard_link fails if the name exists, so this never clobbers a file.
        match std::fs::hard_link(src, &dst) {
            Ok(()) => {
                let _ = std::fs::remove_file(src);
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // Filesystems without hard links (exFAT, some SMB): check, then rename.
                if dst.exists() {
                    continue;
                }
                return std::fs::rename(src, &dst)
                    .map(|_| candidate)
                    .map_err(|e| format!("Failed to move the download into the library: {}", e));
            }
        }
    }
    Err("Too many files with the same name".into())
}

#[cfg(test)]
mod tests {
    use super::manifest::RENDERED_PREFIX;
    use super::*;

    #[test]
    fn download_names_are_safe_and_short() {
        assert_eq!(download_file_name("Sunday: Hope / Faith", "abc", "mp4"), "Sunday Hope Faith [abc].mp4");
        assert_eq!(download_file_name("...hidden", "abc", "mp4"), "hidden [abc].mp4");
        assert_eq!(download_file_name("", "abc", "webm"), "Video [abc].webm");
        let long = "é".repeat(300);
        let name = download_file_name(&long, "abcdefghijk", "mp4");
        assert!(name.len() <= 240, "{}", name.len());
        assert!(name.ends_with(" [abcdefghijk].mp4"));
        assert!(crate::library::is_candidate_video(&name));
    }

    #[test]
    fn move_into_library_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("A [x].mp4"), b"existing").unwrap();
        let src = root.join("src.mp4");
        std::fs::write(&src, b"new").unwrap();
        let name = move_into_library(&src, root, "A [x].mp4").unwrap();
        assert_eq!(name, "A [x] (2).mp4");
        assert_eq!(std::fs::read(root.join("A [x].mp4")).unwrap(), b"existing");
        assert_eq!(std::fs::read(root.join(&name)).unwrap(), b"new");
        assert!(!src.exists());
    }

    #[test]
    fn upload_is_tracked_until_it_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.mp4"), b"video").unwrap();
        let lib = Library::new(root.to_string_lossy().to_string());
        lib.open_blocking(false);
        let id = {
            let mut g = lib.inner();
            let fp = fsutil::fingerprint(&root.join("a.mp4")).unwrap();
            let mut e = Entry::new("a.mp4".into(), fp, Some(60.0));
            e.status = EntryStatus::ReadyForReview;
            let id = e.id.clone();
            g.manifest.entries.push(e);
            g.last.present.insert(id.clone());
            g.scanned = true;
            id
        };
        assert!(lib.work_in_progress().is_none());

        let job = lib.begin_upload(&id, None, None).unwrap();
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Uploading);
        assert_eq!(v.upload_progress, Some(0));
        assert!(v.busy);
        assert_eq!(lib.work_in_progress().as_deref(), Some("a YouTube upload"));
        // A second upload, a reset or a trash can't start meanwhile.
        assert!(lib.begin_upload(&id, None, None).is_err());
        assert!(lib.reset(&id).is_err());

        lib.set_upload_progress(&id, 42);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.upload_progress, Some(42));

        lib.finish_upload(&id, job, Some(&outcome("x", vec![])), &public());
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Done);
        assert_eq!(v.upload_progress, None);
        assert_eq!(v.uploads.len(), 1);
        assert!(lib.work_in_progress().is_none());

        // A failed upload goes back to how it was, with nothing recorded.
        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.finish_upload(&id, job, None, &public());
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Done);
        assert_eq!(v.uploads.len(), 1);
        assert!(!v.busy);
    }

    #[tokio::test]
    async fn update_is_refused_while_busy_and_blocks_new_work() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        let err = lib.begin_update().unwrap_err();
        assert!(err.contains("a YouTube upload"), "{err}");
        lib.finish_upload(&id, job, None, &public());

        lib.begin_update().unwrap();
        assert_eq!(lib.begin_upload(&id, None, None).err().as_deref(), Some(UPDATING));
        assert_eq!(lib.enqueue(&id).await.unwrap_err(), UPDATING);
        assert!(lib.inner().queue.is_empty());
        assert!(lib.work_in_progress().is_none());

        // A failed install lets work start again.
        lib.end_update();
        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.finish_upload(&id, job, None, &public());
    }

    fn outcome(video_id: &str, warnings: Vec<UploadWarning>) -> UploadOutcome {
        UploadOutcome { url: crate::upload::watch_url(video_id), video_id: video_id.into(), warnings, earlier: None, before_cancel: false }
    }

    #[test]
    fn upload_session_persists_and_is_cleared_on_success() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        let options = public();
        let store = EntrySession { lib: &lib, id: &id, options: &options };
        let session = UploadSession::new("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=1".into(), "h", 5, "m");
        store.save(Some(&session));
        lib.finish_upload(&id, job, None, &public());

        // Survives a restart (library.json read again).
        let root = lib.root().unwrap();
        let reopened = Library::new(root.to_string_lossy().to_string());
        reopened.open_blocking(true);
        assert_eq!(reopened.upload_session(&id), Some(session.clone()));

        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.finish_upload(&id, job, Some(&outcome("abc", vec![])), &public());
        assert_eq!(lib.upload_session(&id), None);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads[0].video_id.as_deref(), Some("abc"));
    }

    /// Regression (55e4d3b): a custom thumbnail is cut only from the video's
    /// own images or a picked one.
    #[tokio::test]
    async fn a_custom_thumbnail_reads_only_allowed_images() {
        let (_t, lib, id, dir) = processed_library();
        // Not under the temp folder, which the app may use.
        let elsewhere = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        let outside = elsewhere.path().join("Pictures/pastor.png");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, png()).unwrap();
        std::fs::write(dir.join("thumbnail_option_1_raw.jpg"), png()).unwrap();
        let rect = crate::thumbnail::CropRect { x: 0, y: 0, width: 4, height: 4 };
        let root = lib.root();
        let check = |picked: bool| {
            let root = root.clone();
            move |p: &str| crate::commands::check_path_in(root, None, picked, p, crate::commands::Access::Data)
        };
        let custom = super::super::commands::custom_thumbnail_checked;
        let options = || manifest::load_video_data(&dir).thumbnail_options;
        let before = options();

        let err = custom(&lib, check(false), &id, &outside.to_string_lossy(), rect.clone(), false).await.unwrap_err();
        assert!(err.contains("only use files in its own folders"), "{err}");
        assert_eq!(options(), before, "nothing added");
        assert!(lib.inner().busy.is_empty());

        // The video's own option, cropped again: allowed.
        let own = dir.join("thumbnail_option_1_raw.jpg");
        let made = custom(&lib, check(false), &id, &own.to_string_lossy(), rect.clone(), false).await.unwrap();
        assert_eq!(options()[0], Path::new(&made).file_name().unwrap().to_string_lossy());
        // A picked image: allowed.
        custom(&lib, check(true), &id, &outside.to_string_lossy(), rect, false).await.unwrap();
    }

    struct OneToken;
    impl crate::upload::TokenSource for OneToken {
        fn token(&self) -> futures::future::BoxFuture<'_, Result<String, crate::upload::UploadError>> {
            Box::pin(async { Ok("tok1".to_string()) })
        }
        fn refresh(&self) -> futures::future::BoxFuture<'_, Result<String, crate::upload::UploadError>> {
            Box::pin(async { Ok("tok1".to_string()) })
        }
    }

    /// Regression (adaa7fc): the app quits while the captions go up, after
    /// the video itself is on YouTube. library.json must already have the
    /// upload and no session, so the next Upload can't send it again. Runs
    /// the real uploader against the mock YouTube.
    #[tokio::test]
    async fn quitting_during_the_post_upload_steps_keeps_the_upload_recorded() {
        let (_t, lib, id, _dir) = processed_library();
        let server = crate::upload::test_server::MockServer::start().await;
        let job = lib.begin_upload(&id, None, None).unwrap();
        let options = public();
        let store = EntrySession { lib: &lib, id: &id, options: &options };
        let cfg = crate::upload::UploadConfig { endpoints: crate::upload::Endpoints::at(&server.base), ..Default::default() };
        let video = crate::upload::client::VideoUpload {
            path: &job.video_path,
            file_hash: &job.file_hash,
            metadata: serde_json::json!({"snippet": {"title": "Sermon"}, "status": {"privacyStatus": "public"}}),
        };
        let after = crate::upload::AfterUpload {
            captions: Some("1\n00:00:00,000 --> 00:00:01,000\nhi\n".into()),
            ..Default::default()
        };
        let http = crate::upload::http_client();
        let run = crate::upload::run(&http, &cfg, &OneToken, &store, &job.cancel, &video, &after, &|_, _| {});
        tokio::select! {
            r = run => panic!("the upload should be stopped during its captions: {:?}", r.map(|o| o.video_id)),
            // The quit: the upload's future is dropped, nothing else runs.
            _ = server.wait_until(|s| s.captions >= 1) => {}
        }
        drop(job);

        let root = lib.root().unwrap();
        let reopened = Library::new(root.to_string_lossy().to_string());
        reopened.open_blocking(true);
        assert_eq!(reopened.upload_session(&id), None);
        let v = reopened.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads.iter().filter_map(|u| u.video_id.as_deref()).collect::<Vec<_>>(), ["vid1"]);
        assert_eq!(server.with(|s| s.creates), 1);
    }

    #[test]
    fn an_upload_session_can_be_forgotten_when_not_uploading() {
        let (_t, lib, id, _dir) = processed_library();
        let session = UploadSession::new("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=1".into(), "h", 5, "m");
        lib.set_upload_session(&id, Some(&session));
        let job = lib.begin_upload(&id, None, None).unwrap();
        assert!(lib.forget_upload_session(&id).is_err(), "not during an upload");
        assert_eq!(lib.upload_session(&id), Some(session));
        lib.finish_upload(&id, job, None, &public());
        lib.forget_upload_session(&id).unwrap();
        assert_eq!(lib.upload_session(&id), None);
        // Saved: a reopen doesn't bring it back.
        let reopened = Library::new(lib.root().unwrap().to_string_lossy().to_string());
        reopened.open_blocking(true);
        assert_eq!(reopened.upload_session(&id), None);
        lib.forget_upload_session(&id).unwrap();
    }

    #[test]
    fn a_finished_upload_is_recorded_before_the_post_upload_steps() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        let options = UploadOptions { privacy: "unlisted".into(), playlist_id: Some("PL1".into()) };
        let store = EntrySession { lib: &lib, id: &id, options: &options };
        let session = UploadSession::new("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=1".into(), "h", 5, "m");
        store.save(Some(&session));
        store.complete("vid7", &job.file_hash);

        // A crash now: library.json already has the video and no session,
        // so the next Upload can't send it again.
        let root = lib.root().unwrap();
        let reopened = Library::new(root.to_string_lossy().to_string());
        reopened.open_blocking(true);
        assert_eq!(reopened.upload_session(&id), None);
        let v = reopened.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads.len(), 1);
        assert_eq!(v.uploads[0].video_id.as_deref(), Some("vid7"));
        assert_eq!(v.uploads[0].privacy.as_deref(), Some("unlisted"));
        assert_eq!(v.uploads[0].playlist_id.as_deref(), Some("PL1"));

        // Without a crash, the warnings join that record; no second one.
        let captions = UploadWarning { kind: WarningKind::Captions, message: "c".into() };
        lib.finish_upload(&id, job, Some(&outcome("vid7", vec![captions.clone()])), &options);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads.len(), 1);
        assert_eq!(v.uploads[0].warnings, vec![captions]);
    }

    #[test]
    fn cancel_reaches_the_running_upload() {
        let (_t, lib, id, _dir) = processed_library();
        assert!(lib.cancel_upload(&id).is_err());
        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.cancel_upload(&id).unwrap();
        assert!(job.cancel.is_cancelled());
        lib.finish_upload(&id, job, None, &public());
        assert!(lib.cancel_upload(&id).is_err());
    }

    #[test]
    fn thumbnail_retry_uses_the_stored_video_id_and_clears_its_warning() {
        let (_t, lib, id, dir) = processed_library();
        let thumb = UploadWarning { kind: WarningKind::Thumbnail, message: "t".into() };
        let captions = UploadWarning { kind: WarningKind::Captions, message: "c".into() };
        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.finish_upload(&id, job, Some(&outcome("vid9", vec![thumb.clone(), captions.clone()])), &public());

        let (video_id, path, busy) = lib.begin_thumbnail_retry(&id).unwrap();
        assert_eq!(video_id, "vid9");
        assert_eq!(path, dir.join("thumbnail_option_2.jpg"));
        // Busy meanwhile.
        assert!(lib.begin_upload(&id, None, None).is_err());
        // A failure keeps the warning.
        assert_eq!(lib.finish_thumbnail_retry(busy, &id, &video_id, false), vec![thumb, captions.clone()]);
        let (video_id, _, busy) = lib.begin_thumbnail_retry(&id).unwrap();
        assert_eq!(lib.finish_thumbnail_retry(busy, &id, &video_id, true), vec![captions.clone()]);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads[0].warnings, vec![captions]);
        assert!(!v.busy);
    }

    #[test]
    fn untrusted_session_uris_are_dropped_on_load() {
        let (_t, lib, id, _dir) = processed_library();
        lib.set_upload_session(&id, Some(&UploadSession::new("https://evil.example/x".into(), "h", 5, "m")));
        let reopened = Library::new(lib.root().unwrap().to_string_lossy().to_string());
        reopened.open_blocking(true);
        assert_eq!(reopened.upload_session(&id), None);
    }

    fn public() -> UploadOptions {
        UploadOptions { privacy: "public".into(), playlist_id: None }
    }

    #[test]
    fn upload_options_fill_in_from_settings() {
        let mut settings = crate::state::Settings::default();
        settings.upload_privacy = "unlisted".into();
        settings.youtube_playlist_id = Some("PLdefault".into());
        let o = UploadOptions::resolve(&settings, None, None).unwrap();
        assert_eq!(o, UploadOptions { privacy: "unlisted".into(), playlist_id: Some("PLdefault".into()) });
        let o = UploadOptions::resolve(&settings, Some("private".into()), Some("PLother".into())).unwrap();
        assert_eq!(o, UploadOptions { privacy: "private".into(), playlist_id: Some("PLother".into()) });
        // "No playlist" chosen on the review screen beats the default.
        assert_eq!(UploadOptions::resolve(&settings, None, Some("".into())).unwrap().playlist_id, None);
        assert!(UploadOptions::resolve(&settings, Some("secret".into()), None).is_err());
        // First time: public.
        assert_eq!(UploadOptions::resolve(&crate::state::Settings::default(), None, None).unwrap().privacy, "public");
    }

    #[test]
    fn upload_record_keeps_privacy_and_playlist() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        let options = UploadOptions { privacy: "unlisted".into(), playlist_id: Some("PL1".into()) };
        lib.finish_upload(&id, job, Some(&outcome("y", vec![])), &options);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.uploads[0].privacy.as_deref(), Some("unlisted"));
        assert_eq!(v.uploads[0].playlist_id.as_deref(), Some("PL1"));
    }

    /// Stands in for ffmpeg and the thumbnail model, writing small files
    /// where the real tools would and noting each call.
    #[derive(Default)]
    struct FakeTools {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl JobTools for FakeTools {
        async fn extract_audio(&self, _video: &Path, audio: &Path) -> Result<(), String> {
            self.calls.lock().unwrap().push("extract_audio".into());
            std::fs::write(audio, b"fake audio").map_err(|e| e.to_string())
        }
        async fn duration(&self, _video: &Path) -> Result<f64, String> {
            self.calls.lock().unwrap().push("duration".into());
            Ok(60.0)
        }
        async fn trim(&self, _video: &Path, start: f64, end: f64, out: &Path, on_step: &(dyn Fn(f32, &str) + Send + Sync)) -> Result<f64, String> {
            self.calls.lock().unwrap().push(format!("trim {start}-{end}"));
            on_step(0.5, "half");
            std::fs::write(out, b"the cut").map_err(|e| e.to_string())?;
            // The cut starts at a keyframe half a second early.
            Ok(start - 0.5)
        }
        async fn thumbnails(&self, _video: &Path, start: f64, _end: f64, title: &str, out: &Path) -> Result<Vec<String>, String> {
            self.calls.lock().unwrap().push(format!("thumbnails from {start} for {title}"));
            let mut paths = Vec::new();
            for n in 1..=2 {
                std::fs::write(out.join(format!("thumbnail_option_{n}_raw.jpg")), b"raw").unwrap();
                let p = out.join(format!("thumbnail_option_{n}.jpg"));
                std::fs::write(&p, b"jpg").unwrap();
                paths.push(path_string(&p));
            }
            Ok(paths)
        }
    }

    /// A Modal stand-in on localhost: answers each POST with `reply` and
    /// keeps the bodies it was sent.
    async fn modal_mock(reply: &'static str) -> (String, Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/transcribe", listener.local_addr().unwrap());
        let bodies: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::default();
        let seen = bodies.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let head_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "the request ended early");
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                seen.lock().unwrap().push(buf[head_end..].to_vec());
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                );
                sock.write_all(answer.as_bytes()).await.unwrap();
            }
        });
        (url, bodies)
    }

    const MODAL_REPLY: &str = r#"{"segments":[{"start":12,"end":13,"text":"Hope"}],"text":"Hope","sermon_analysis":{"sermon_start":12.0,"sermon_end":50.0,"title":"The Hope We Have","description":"About hope","chapters":[{"time":12.0,"title":"Hope"}]}}"#;

    #[tokio::test]
    async fn a_job_runs_end_to_end_and_reuses_its_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sunday.mp4"), b"video").unwrap();
        let lib = Library::new(root.to_string_lossy().to_string());
        lib.open_blocking(false);
        lib.inner().apply_scan(&[seen(&root, "sunday.mp4")]);
        let id = lib.inner().manifest.entries[0].id.clone();
        let (url, bodies) = modal_mock(MODAL_REPLY).await;
        let mut state = AppState::for_test();
        (state.settings.modal_api_url, state.settings.modal_api_key) = (url, Some("k".into()));
        let app_state: AppStateHandle = Arc::new(tokio::sync::Mutex::new(state));

        let run = |tools: &'static FakeTools| {
            let (lib, app_state, id) = (lib.clone(), app_state.clone(), id.clone());
            async move {
                lib.enqueue(&id).await.unwrap();
                let (started, _token) = lib.start_next().unwrap();
                assert_eq!(started, id);
                let stages = std::sync::Mutex::new(Vec::new());
                let progress = |stage: ProcessingStage, done: f32, _: &str| stages.lock().unwrap().push((stage, done));
                let result = lib.run_job_with(&app_state, &id, &progress, tools).await;
                lib.finish_job(&id, &Some(result.clone()));
                result.unwrap();
                stages.into_inner().unwrap()
            }
        };

        let tools: &'static FakeTools = Box::leak(Box::default());
        let stages = run(tools).await;
        assert_eq!(
            *tools.calls.lock().unwrap(),
            ["extract_audio", "trim 12-50", "thumbnails from 11.5 for The Hope We Have"]
        );
        // The audio went to Modal once, as base64.
        {
            let bodies = bodies.lock().unwrap();
            assert_eq!(bodies.len(), 1);
            let sent: serde_json::Value = serde_json::from_slice(&bodies[0]).unwrap();
            use base64::Engine;
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(sent["audio_base64"].as_str().unwrap()).unwrap(), b"fake audio");
        }
        assert!(stages.iter().any(|(s, done)| *s == ProcessingStage::Trimming && (*done - 62.5).abs() < 0.01));
        assert_eq!(stages.last().unwrap().0, ProcessingStage::Complete);

        let dir = lib.inner().paths.as_ref().unwrap().video_dir(&id).unwrap();
        let data = manifest::load_video_data(&dir);
        let trimmed = data.trimmed_file.clone().unwrap();
        assert_eq!(std::fs::read(dir.join(&trimmed)).unwrap(), b"the cut");
        assert_eq!((data.trim_start, data.trim_end), (Some(11.5), Some(50.0)), "times from where the cut really starts");
        assert_eq!(data.thumbnail_options, ["thumbnail_option_1.jpg", "thumbnail_option_2.jpg"]);
        assert_eq!(data.selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        assert!(dir.join("thumbnail_option_2_raw.jpg").exists());
        let transcript = manifest::load_transcript(&dir).unwrap();
        assert_eq!(transcript.transcription.suggested_title.as_deref(), Some("The Hope We Have"));
        {
            let g = lib.inner();
            let e = g.entry(&id).unwrap();
            assert_eq!(e.status, EntryStatus::ReadyForReview);
            assert!(g.running.is_none() && g.queue.is_empty());
        }
        let tmp_dir = lib.inner().paths.as_ref().unwrap().tmp_dir();
        assert!(std::fs::read_dir(&tmp_dir).map(|d| d.count()).unwrap_or(0) == 0, "the work folder is gone");

        // Again (e.g. Retry): the saved transcript is used; Modal isn't
        // asked again and no audio is extracted.
        let again: &'static FakeTools = Box::leak(Box::default());
        run(again).await;
        assert_eq!(*again.calls.lock().unwrap(), ["trim 12-50", "thumbnails from 11.5 for The Hope We Have"]);
        assert_eq!(bodies.lock().unwrap().len(), 1);

        // Reprocess: transcribed again, and the review typed meanwhile kept.
        let typed = Review { title: Some("My Title".into()), description: Some("Mine".into()), preacher: Some("P".into()), ..Default::default() };
        lib.save_review(&id, typed, None).unwrap();
        lib.reprocess(&id).await.unwrap();
        assert!(manifest::load_transcript(&dir).is_none());
        assert!(lib.reprocess(&id).await.is_err(), "already queued");
        let (started, _token) = lib.start_next().unwrap();
        let fresh: &'static FakeTools = Box::leak(Box::default());
        let result = lib.run_job_with(&app_state, &started, &|_, _, _| {}, fresh).await;
        lib.finish_job(&id, &Some(result.clone()));
        result.unwrap();
        assert_eq!(fresh.calls.lock().unwrap()[0], "extract_audio");
        assert_eq!(bodies.lock().unwrap().len(), 2);
        let review = manifest::load_video_data(&dir).review;
        assert_eq!((review.title.as_deref(), review.description.as_deref()), (Some("My Title"), Some("Mine")));
    }

    /// A writable library with one processed video that has two generated
    /// thumbnail options.
    fn processed_library() -> (tempfile::TempDir, Arc<Library>, String, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.mp4"), b"video").unwrap();
        let lib = Library::new(root.to_string_lossy().to_string());
        lib.open_blocking(false);
        let (id, dir) = {
            let mut g = lib.inner();
            let fp = fsutil::fingerprint(&root.join("a.mp4")).unwrap();
            let mut e = Entry::new("a.mp4".into(), fp, Some(60.0));
            e.status = EntryStatus::ReadyForReview;
            let id = e.id.clone();
            g.manifest.entries.push(e);
            g.last.present.insert(id.clone());
            g.scanned = true;
            let dir = g.paths.as_ref().unwrap().ensure_video_dir(&id).unwrap();
            (id, dir)
        };
        for f in ["thumbnail_option_1.jpg", "thumbnail_option_1_raw.jpg", "thumbnail_option_2.jpg"] {
            std::fs::write(dir.join(f), b"jpg").unwrap();
        }
        let data = VideoData {
            thumbnail_options: vec!["thumbnail_option_1.jpg".into(), "thumbnail_option_2.jpg".into()],
            selected_source: Some("thumbnail_option_2.jpg".into()),
            ..Default::default()
        };
        manifest::save_video_data(&dir, &data).unwrap();
        (tmp, lib, id, dir)
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(4, 4).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    const TV: u32 = 4;

    /// The key a render of `source` must carry once the review's title is
    /// `title` (and the speaker "P").
    fn test_key(dir: &Path, source: &str, title: &str) -> String {
        let (_, version) = render_from(dir, source);
        manifest::render_key(TV, title, "P", false, source, &version, "")
    }

    /// Save the review with `title`, then a render of `source` made from it.
    async fn rendered(lib: &Arc<Library>, id: &str, source: &str, title: &str, bytes: Vec<u8>) -> Result<String, String> {
        // Refused while the video is processing, as the render then is.
        let _ = lib.save_review(id, Review { title: Some(title.into()), preacher: Some("P".into()), ..Default::default() }, None);
        let dir = lib.inner().paths.clone().unwrap().video_dir(id).unwrap();
        lib.save_rendered_thumbnail(id, source, &test_key(&dir, source, title), TV, "", bytes).await
    }

    #[tokio::test]
    async fn a_render_of_inputs_no_longer_saved_is_refused() {
        let (_t, lib, id, dir) = processed_library();
        let source = "thumbnail_option_1.jpg";
        let review = |title: &str| Review { title: Some(title.into()), preacher: Some("P".into()), ..Default::default() };
        let key_a = test_key(&dir, source, "A");
        lib.save_review(&id, review("A"), None).unwrap();
        lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap();

        // The title became B: a render of A is stale.
        lib.save_review(&id, review("B"), None).unwrap();
        let err = lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap_err();
        assert_eq!(err, STALE_RENDER);
        assert_eq!(lib.detail(&id).unwrap().renders[0].render_key, key_a, "the stored render is kept");

        // And back to A: A's render is current again.
        lib.save_review(&id, review("A"), None).unwrap();
        lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap();
        // So is a render made with the speaker on one line, once saved so.
        let one_line = Review { single_line: true, ..review("A") };
        lib.save_review(&id, one_line, None).unwrap();
        let (_, version) = render_from(&dir, source);
        let key = manifest::render_key(TV, "A", "P", true, source, &version, "");
        lib.save_rendered_thumbnail(&id, source, &key, TV, "", png()).await.unwrap();
        assert!(lib.save_rendered_thumbnail(&id, source, &key, TV + 1, "", png()).await.is_err(), "another template");
        assert_eq!(rendered_files(&dir).len(), 1);

        // A logo chosen in Settings: a render without it is stale, one
        // drawn with it is current.
        let logo = "/Users/me/Pictures/logo.png";
        assert_eq!(lib.save_rendered_thumbnail(&id, source, &key, TV, logo, png()).await.unwrap_err(), STALE_RENDER);
        let with_logo = manifest::render_key(TV, "A", "P", true, source, &version, logo);
        assert_ne!(with_logo, key);
        lib.save_rendered_thumbnail(&id, source, &with_logo, TV, logo, png()).await.unwrap();
    }

    fn rendered_files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(RENDERED_PREFIX))
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn renders_replace_per_source_and_keep_the_selection() {
        let (_t, lib, id, dir) = processed_library();
        let first = rendered(&lib, &id, "thumbnail_option_1.jpg", "k1", png()).await.unwrap();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k1", png()).await.unwrap();
        // Re-rendering a source replaces its render and deletes the old file.
        let second = rendered(&lib, &id, "thumbnail_option_1.jpg", "k2", png()).await.unwrap();
        assert!(!Path::new(&first).exists());
        assert!(Path::new(&second).exists());
        assert_eq!(rendered_files(&dir).len(), 2);

        let d = lib.detail(&id).unwrap();
        assert_eq!(d.thumbnail_options.len(), 2, "renders never become options");
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        let r1 = d.renders.iter().find(|r| r.source == "thumbnail_option_1.jpg").unwrap();
        assert_eq!(r1.render_key, test_key(&dir, "thumbnail_option_1.jpg", "k2"));
        assert_eq!(r1.path, second);
        // Generated options render from their plain crop.
        assert!(d.thumbnail_options[0].render_from.ends_with("thumbnail_option_1_raw.jpg"));
        assert!(d.thumbnail_options[1].render_from.ends_with("thumbnail_option_2.jpg"));
        // video.json's selected_thumbnail (read by v0.4.0) is the selected render.
        let sel = d.selected_thumbnail.unwrap();
        assert!(Path::new(&sel).file_name().unwrap().to_string_lossy().starts_with(RENDERED_PREFIX));

        // Reopening shows the same renders and options: nothing stacks.
        let again = lib.detail(&id).unwrap();
        assert_eq!(again.thumbnail_options.len(), 2);
        assert_eq!(again.renders.len(), 2);
    }

    #[tokio::test]
    async fn saves_keep_unrecorded_renders_until_the_next_open_sweeps_them() {
        let (_t, lib, id, dir) = processed_library();
        let kept = rendered(&lib, &id, "thumbnail_option_1.jpg", "k1", png()).await.unwrap();
        // A render being written, and one renamed into place but not yet
        // recorded, while the review screen saves.
        let writing = format!("{}{}", manifest::RENDER_TMP_PREFIX, manifest::render_file_name("thumbnail_option_2.jpg"));
        std::fs::write(dir.join(&writing), b"half").unwrap();
        let unrecorded = manifest::render_file_name("thumbnail_option_2.jpg");
        std::fs::write(dir.join(&unrecorded), b"r").unwrap();
        lib.save_review(&id, Review::default(), Some("thumbnail_option_1.jpg".into())).unwrap();
        assert!(dir.join(&writing).exists());
        assert!(dir.join(&unrecorded).exists());

        // Reopened after a crash: both are left over, and go.
        lib.open_blocking(false);
        assert!(!dir.join(&writing).exists());
        assert!(!dir.join(&unrecorded).exists());
        assert!(Path::new(&kept).exists());
    }

    #[tokio::test]
    async fn adding_a_source_keeps_selection_and_renders() {
        let (_t, lib, id, dir) = processed_library();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();
        // What custom_thumbnail does: a new source at the front.
        std::fs::write(dir.join("custom_thumbnail_1.jpg"), b"jpg").unwrap();
        let mut data = manifest::load_video_data(&dir);
        data.thumbnail_options.insert(0, "custom_thumbnail_1.jpg".into());
        manifest::save_video_data(&dir, &data).unwrap();

        let d = lib.detail(&id).unwrap();
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        assert_eq!(d.renders.len(), 1);
        assert_eq!(d.renders[0].source, "thumbnail_option_2.jpg");
        // Dropping an unselected source keeps the selection too.
        let mut data = manifest::load_video_data(&dir);
        data.thumbnail_options.retain(|o| o != "thumbnail_option_1.jpg");
        manifest::save_video_data(&dir, &data).unwrap();
        assert_eq!(lib.detail(&id).unwrap().selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
    }

    #[tokio::test]
    async fn renders_are_allowed_during_upload_but_not_processing() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        rendered(&lib, &id, "thumbnail_option_1.jpg", "k", png()).await.unwrap();
        lib.finish_upload(&id, job, None, &public());

        lib.inner().queue.push_back(id.clone());
        let err = rendered(&lib, &id, "thumbnail_option_1.jpg", "k2", png()).await.unwrap_err();
        assert!(err.contains("processed"), "{err}");
        lib.inner().queue.clear();

        // Not one of the options, or a key that isn't plain ASCII.
        assert!(lib.save_rendered_thumbnail(&id, "../../x.jpg", "k", TV, "", png()).await.is_err());
        assert!(lib.save_rendered_thumbnail(&id, "other.jpg", "k", TV, "", png()).await.is_err());
        assert!(lib.save_rendered_thumbnail(&id, "thumbnail_option_1.jpg", "k/../", TV, "", png()).await.is_err());
    }

    #[test]
    fn detail_mentions_an_upload_of_the_same_file_under_another_entry() {
        let (_t, lib, id, _dir) = processed_library();
        assert_eq!(lib.detail(&id).unwrap().duplicate_upload, None);
        let job = lib.begin_upload(&id, None, None).unwrap();
        lib.finish_upload(&id, job, Some(&outcome("abc", vec![])), &public());
        // Its own upload isn't a duplicate.
        assert_eq!(lib.detail(&id).unwrap().duplicate_upload, None);

        let copy = {
            let mut g = lib.inner();
            let fp = g.entry(&id).unwrap().fingerprint.clone();
            let e = Entry::new("a copy.mp4".into(), fp, Some(60.0));
            let copy = e.id.clone();
            g.manifest.entries.push(e);
            let other = Entry::new("b.mp4".into(), Fingerprint { hash: "different".into(), ..g.entry(&id).unwrap().fingerprint.clone() }, None);
            g.manifest.entries.push(other);
            copy
        };
        assert_eq!(lib.detail(&copy).unwrap().duplicate_upload.as_deref(), Some(crate::upload::watch_url("abc").as_str()));
    }

    #[test]
    fn queued_videos_know_their_place_in_line() {
        let (_t, lib, id, _dir) = processed_library();
        let view = |id: &str| lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!((view(&id).queue_position, view(&id).queue_length), (None, None));
        {
            let mut g = lib.inner();
            g.running = Some(Running {
                id: "other".into(),
                stage: ProcessingStage::Transcribing,
                progress: 0.0,
                message: String::new(),
                cancel: CancellationToken::new(),
                previous: EntryStatus::New,
                committing: false,
                late_cancel: false,
            });
            g.queue.push_back("ahead".into());
            g.queue.push_back(id.clone());
        }
        let v = view(&id);
        assert_eq!(v.status, ViewStatus::Queued);
        assert_eq!((v.queue_position, v.queue_length), (Some(3), Some(3)));
        lib.inner().running = None;
        let v = view(&id);
        assert_eq!((v.queue_position, v.queue_length), (Some(2), Some(2)));
    }

    #[tokio::test]
    async fn upload_uses_the_current_render_or_the_source() {
        let (_t, lib, id, dir) = processed_library();
        let render = rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();

        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some(test_key(&dir, "thumbnail_option_2.jpg", "k"))).unwrap();
        assert_eq!(job.thumbnail_path.as_deref(), Some(Path::new(&render)));
        lib.finish_upload(&id, job, None, &public());

        // A stale key falls back to the source itself.
        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some("old".into())).unwrap();
        assert_eq!(job.thumbnail_path, Some(dir.join("thumbnail_option_2.jpg")));
        lib.finish_upload(&id, job, None, &public());

        // No source given: the selected one's render.
        let job = lib.begin_upload(&id, None, None).unwrap();
        assert_eq!(job.thumbnail_path.as_deref(), Some(Path::new(&render)));
        lib.finish_upload(&id, job, None, &public());

        assert!(lib.begin_upload(&id, Some("/etc/passwd".into()), None).is_err());
        assert!(!lib.inner().busy.contains(&id), "a refused upload doesn't stay busy");
    }

    #[tokio::test]
    async fn an_edit_during_an_upload_does_not_lose_its_thumbnail() {
        let (_t, lib, id, dir) = processed_library();
        let render = rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();
        let sent = std::fs::read(&render).unwrap();
        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some(test_key(&dir, "thumbnail_option_2.jpg", "k"))).unwrap();

        // The title changes mid-upload: a new render replaces (deletes) the old one.
        let mut other = image::RgbImage::new(8, 8);
        other.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        other.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k2", bytes.into_inner()).await.unwrap();
        assert!(!Path::new(&render).exists());

        assert_eq!(job.thumbnail_data.as_deref(), Some(sent.as_slice()));
        lib.finish_upload(&id, job, None, &public());
    }

    #[test]
    fn save_review_selects_by_source_id() {
        let (_t, lib, id, _dir) = processed_library();
        let review = Review { title: Some("T".into()), ..Default::default() };
        lib.save_review(&id, review.clone(), Some("thumbnail_option_1.jpg".into())).unwrap();
        assert_eq!(lib.detail(&id).unwrap().selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        // An unknown source keeps the old selection but saves the text.
        lib.save_review(&id, Review { title: Some("U".into()), ..Default::default() }, Some("gone.jpg".into())).unwrap();
        let d = lib.detail(&id).unwrap();
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        assert_eq!(d.review.title.as_deref(), Some("U"));
    }

    #[tokio::test]
    async fn trash_during_a_scan_leaves_no_ghost() {
        let (_t, lib, id, _dir) = processed_library();
        let root = lib.root().unwrap();
        // A scan is under way and has listed the file...
        let scanning = lib.scan_lock.lock().await;
        let known = scan::known_files(&lib.inner().manifest.entries);
        let probe = |_: &Path| Some(1.0);
        let obs = scan::observe(&root, &known, &mut ScanCache::default(), &probe, std::time::Duration::ZERO).unwrap();
        assert_eq!(obs.len(), 1);
        // ...when Move to Trash is clicked.
        let trash = tokio::spawn({
            let (lib, id) = (lib.clone(), id.clone());
            async move { lib.trash_with(&id, |p| std::fs::remove_file(p).map_err(|e| e.to_string())).await }
        });
        // One thread: yielding runs the trash until it waits on the scan
        // lock (no sleep needed to get it there).
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!trash.is_finished());
        assert!(root.join("a.mp4").exists(), "the trash waits for the scan");
        lib.inner().apply_scan(&obs);
        drop(scanning);
        trash.await.unwrap().unwrap();

        assert!(!root.join("a.mp4").exists());
        let g = lib.inner();
        assert!(g.manifest.entries.is_empty(), "no entry comes back for the trashed file");
        assert!(g.busy.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_library_stays_responsive_during_a_slow_trash() {
        let (_t, lib, id, dir) = processed_library();
        // Deleting the app's files waits on the video's lock, held here.
        let lock = lib.video_lock(&id);
        let held = hold(&lock);
        let trash = tokio::spawn({
            let (lib, id) = (lib.clone(), id.clone());
            async move {
                lib.trash_with(&id, |p| {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    std::fs::remove_file(p).map_err(|e| e.to_string())
                })
                .await
            }
        });
        // Wait for the trash to mark the video busy and get stuck on its lock.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !lib.inner().busy.contains(&id) {
            assert!(std::time::Instant::now() < deadline, "the trash never started");
            tokio::task::yield_now().await;
        }
        let started = std::time::Instant::now();
        let view = lib.view(vec![]);
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(view.videos.iter().any(|v| v.id == id && v.busy));
        assert!(dir.exists(), "the folder goes under the video's lock");
        drop(held);
        trash.await.unwrap().unwrap();
        assert!(!dir.exists());
        assert!(lib.view(vec![]).videos.is_empty());
    }

    #[test]
    fn an_entry_removed_mid_reset_is_reported() {
        let (_t, lib, id, dir) = processed_library();
        let lock = lib.video_lock(&id);
        let held = hold(&lock);
        let reset = std::thread::spawn({
            let (lib, id) = (lib.clone(), id.clone());
            move || lib.reset(&id)
        });
        while !lib.inner().busy.contains(&id) {
            std::thread::yield_now();
        }
        // E.g. the library was reopened and another computer had dropped it.
        lib.inner().manifest.entries.retain(|e| e.id != id);
        drop(held);
        assert_eq!(reset.join().unwrap(), Err(ENTRY_GONE.to_string()));
        assert!(!dir.exists());
        assert!(lib.inner().busy.is_empty());
    }

    #[test]
    fn review_is_refused_while_queued() {
        let (_t, lib, id, _dir) = processed_library();
        lib.inner().queue.push_back(id.clone());
        let err = lib.save_review(&id, Review::default(), None).unwrap_err();
        assert!(err.contains("processed"), "{err}");
        lib.inner().queue.clear();
        lib.save_review(&id, Review::default(), None).unwrap();
    }

    #[test]
    fn failed_reprocess_keeps_status_and_outputs() {
        let (_t, lib, id, dir) = processed_library();
        lib.inner().queue.push_back(id.clone());
        let (started, _token) = lib.start_next().unwrap();
        assert_eq!(started, id);
        assert_eq!(lib.view(vec![]).videos[0].status, ViewStatus::Processing);

        lib.finish_job(&id, &Some(Err("ffmpeg broke".into())));
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::ReadyForReview);
        assert_eq!(v.error.as_deref(), Some("Re-processing failed: ffmpeg broke"));
        assert!(dir.join("thumbnail_option_1.jpg").is_file());
        assert_eq!(lib.detail(&id).unwrap().thumbnail_options.len(), 2);

        // Cancelling the next try clears the old failure; a first-time
        // failure is an Error.
        lib.inner().queue.push_back(id.clone());
        lib.start_next().unwrap();
        lib.finish_job(&id, &None);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!((v.status, v.error), (ViewStatus::ReadyForReview, None));
        lib.inner().manifest.get_mut(&id).unwrap().status = EntryStatus::New;
        lib.inner().queue.push_back(id.clone());
        lib.start_next().unwrap();
        lib.finish_job(&id, &Some(Err("no network".into())));
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!((v.status, v.error.as_deref()), (ViewStatus::Error, Some("no network")));
    }

    #[tokio::test]
    async fn set_trim_recuts_and_records_the_real_start() {
        if !crate::tools::ffmpeg_for_tests() {
            return;
        }
        let (_t, lib, id, dir) = processed_library();
        let root = lib.root().unwrap();
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x120:rate=25", "-t", "20"])
            .args(["-c:v", "mpeg4", "-g", "50"])
            .arg(root.join("a.mp4"))
            .status()
            .unwrap();
        assert!(status.success());
        {
            let mut g = lib.inner();
            let fp = fsutil::fingerprint(&root.join("a.mp4")).unwrap();
            let e = g.entry_mut(&id).unwrap();
            e.fingerprint = fp;
            e.duration = Some(20.0);
            e.upload_session = Some(UploadSession::new("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=1".into(), "h", 5, "m"));
        }
        let none = |_: f32, _: &str| {};

        // Not while it's queued.
        lib.inner().queue.push_back(id.clone());
        assert!(lib.set_trim_with(&id, 3.0, 15.0, &none).await.is_err());
        lib.inner().queue.clear();

        // Keyframes every 2 s: a start of 5.2 snaps to 4.
        let (start, end) = lib.set_trim_with(&id, 5.2, 30.0, &none).await.unwrap();
        assert_eq!((start, end), (4.0, 20.0), "the end is kept inside the video");
        let d = lib.detail(&id).unwrap();
        assert_eq!((d.trim_start, d.trim_end), (Some(4.0), Some(20.0)));
        let trimmed = d.trimmed_path.unwrap();
        assert!((crate::ffmpeg::probe_duration_blocking(Path::new(&trimmed)).unwrap() - 16.0).abs() < 0.2);
        assert!(lib.upload_session(&id).is_some(), "the old cut's session is kept, for the next upload to settle");
        assert!(!lib.inner().busy.contains(&id));
        assert!(dir.join("thumbnail_option_1.jpg").exists(), "thumbnails are kept");
        assert!(lib.set_trim_with(&id, 9.5, 10.0, &none).await.is_err(), "at least a second");
    }

    #[test]
    fn a_trim_keeps_review_edits_saved_meanwhile() {
        let (_t, lib, id, dir) = processed_library();
        let staging = tempfile::tempdir_in(dir.parent().unwrap()).unwrap();
        std::fs::write(staging.path().join("trimmed_new.mp4"), b"cut").unwrap();
        let lock = lib.video_lock(&id);
        let guard = hold(&lock);
        std::thread::scope(|s| {
            let trim = s.spawn(|| lib.record_trim(&id, &dir, staging.path(), "trimmed_new.mp4", 4.0, 20.0));
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(!trim.is_finished(), "waits for the lock");
            // save_review, which holds the video's lock, runs meanwhile.
            let mut data = manifest::load_video_data(&dir);
            data.review.title = Some("Edited".into());
            manifest::save_video_data(&dir, &data).unwrap();
            drop(guard);
            trim.join().unwrap().unwrap();
        });
        let data = manifest::load_video_data(&dir);
        assert_eq!(data.review.title.as_deref(), Some("Edited"));
        assert_eq!((data.trimmed_file.as_deref(), data.trim_start, data.trim_end), (Some("trimmed_new.mp4"), Some(4.0), Some(20.0)));
    }

    /// A video with a cut at 2–10 s, and a new cut (5–20 s) staged.
    fn trim_fixture() -> (tempfile::TempDir, Arc<Library>, String, PathBuf, tempfile::TempDir) {
        let (t, lib, id, dir) = processed_library();
        std::fs::write(dir.join("trimmed.mp4"), b"old cut").unwrap();
        let mut data = manifest::load_video_data(&dir);
        (data.trimmed_file, data.trim_start, data.trim_end) = (Some("trimmed.mp4".into()), Some(2.0), Some(10.0));
        manifest::save_video_data(&dir, &data).unwrap();
        let staging = tempfile::tempdir_in(dir.parent().unwrap()).unwrap();
        std::fs::write(staging.path().join("trimmed.mp4"), b"new cut").unwrap();
        (t, lib, id, dir, staging)
    }

    fn cut_and_times(dir: &Path) -> (Vec<u8>, Option<f64>, Option<f64>) {
        let data = manifest::load_video_data(dir);
        (std::fs::read(dir.join(data.trimmed_file.unwrap())).unwrap(), data.trim_start, data.trim_end)
    }

    #[test]
    fn a_re_trim_swaps_the_cut_and_its_times_together() {
        let (_t, lib, id, dir, staging) = trim_fixture();
        lib.record_trim(&id, &dir, staging.path(), "trimmed.mp4", 5.0, 20.0).unwrap();
        assert_eq!(cut_and_times(&dir), (b"new cut".to_vec(), Some(5.0), Some(20.0)));
        assert!(!dir.join(COMMIT_BACKUP).exists());
        assert!(dir.join("thumbnail_option_1.jpg").exists(), "only the cut is replaced");

        // A crash at any point before the times are written: the old cut
        // and the old times, together.
        for steps in 0..=2 {
            let (_t, _lib, _id, dir, staging) = trim_fixture();
            crash_after_where(&dir, staging.path(), steps, &|n| n == "trimmed.mp4");
            recover_commit(&dir);
            assert_eq!(cut_and_times(&dir), (b"old cut".to_vec(), Some(2.0), Some(10.0)), "after {steps} moves");
            assert!(dir.join("thumbnail_option_1.jpg").exists());
        }
    }

    /// What a scan sees for a file in the library folder.
    fn seen(root: &Path, name: &str) -> FileObs {
        let fp = fsutil::fingerprint(&root.join(name)).unwrap();
        FileObs { name: name.into(), size: fp.size, mtime_ms: fp.mtime_ms, state: ObsState::Ready, hash: Some(fp.hash), duration: Some(60.0) }
    }

    #[tokio::test]
    async fn auto_process_takes_new_files_but_not_the_backlog() {
        let (_t, lib, old, _dir) = processed_library();
        let root = lib.root().unwrap();
        std::fs::write(root.join("backlog.mp4"), b"old service").unwrap();
        lib.inner().apply_scan(&[seen(&root, "a.mp4"), seen(&root, "backlog.mp4")]);
        let backlog = lib.inner().manifest.entries.iter().find(|e| e.file_name == "backlog.mp4").unwrap().id.clone();

        // Turned on now: what's there already stays put.
        let since = chrono::Utc::now().timestamp_millis() + 1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(lib.auto_process(since).await.is_empty());

        std::fs::write(root.join("sunday.mp4"), b"new service").unwrap();
        lib.inner().apply_scan(&[seen(&root, "a.mp4"), seen(&root, "backlog.mp4"), seen(&root, "sunday.mp4")]);
        let new = lib.inner().manifest.entries.iter().find(|e| e.file_name == "sunday.mp4").unwrap().id.clone();
        assert_eq!(lib.auto_process(since).await, vec![new.clone()]);
        let g = lib.inner();
        assert_eq!(g.queue.iter().cloned().collect::<Vec<_>>(), vec![new.clone()]);
        assert!(!g.queue.contains(&backlog) && !g.queue.contains(&old));
        drop(g);

        // Cancelled, it isn't queued again by itself.
        lib.cancel(&new).unwrap();
        assert!(lib.auto_process(since).await.is_empty());
    }

    #[tokio::test]
    async fn switching_folders_never_queues_the_backlog() {
        let (_t, lib, _id, _dir) = processed_library();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("old 1.mp4"), b"one").unwrap();
        std::fs::write(other.path().join("old 2.mp4"), b"two").unwrap();
        let since = chrono::Utc::now().timestamp_millis();
        lib.switch(other.path().to_string_lossy().to_string()).await.unwrap();
        let root = lib.root().unwrap();
        lib.inner().apply_scan(&[seen(&root, "old 1.mp4"), seen(&root, "old 2.mp4")]);
        assert_eq!(lib.inner().manifest.entries.len(), 2);
        assert!(lib.auto_process(since).await.is_empty());
        assert!(lib.inner().manifest.entries.iter().all(|e| e.first_seen == 0));

        // A file that arrives afterwards is new.
        std::fs::write(root.join("new.mp4"), b"three").unwrap();
        lib.inner().apply_scan(&[seen(&root, "old 1.mp4"), seen(&root, "old 2.mp4"), seen(&root, "new.mp4")]);
        assert_eq!(lib.auto_process(since).await.len(), 1);
    }

    #[tokio::test]
    async fn a_read_only_first_scan_keeps_the_backlog_out_after_a_regain() {
        let (_t, lib, _id, _dir) = processed_library();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("old 1.mp4"), b"one").unwrap();
        std::fs::write(other.path().join("old 2.mp4"), b"two").unwrap();
        // The new folder is open on another Mac.
        let elsewhere = LibraryPaths::new(other.path().canonicalize().unwrap());
        elsewhere.ensure_dir(&elsewhere.data_dir()).unwrap();
        let held = lock::LockInfo {
            host: "Office iMac".into(),
            pid: 1,
            session: "other".into(),
            app_version: "0".into(),
            heartbeat: chrono::Utc::now().timestamp(),
            machine_id: Some("another-mac".into()),
            user: Some("someone".into()),
        };
        fsutil::write_json_atomic(&elsewhere.lock_file(), &held).unwrap();
        let since = chrono::Utc::now().timestamp_millis();
        lib.switch(other.path().to_string_lossy().to_string()).await.unwrap();
        assert!(!lib.view(vec![]).writable);
        let root = lib.root().unwrap();
        let backlog = [seen(&root, "old 1.mp4"), seen(&root, "old 2.mp4")];
        lib.inner().apply_scan(&backlog);

        // The other Mac lets go; the first writable scan still sees a backlog.
        std::fs::remove_file(elsewhere.lock_file()).unwrap();
        assert_eq!(lib.heartbeat().await, HeartbeatOutcome::Regained);
        lib.reopen().await.unwrap();
        assert!(lib.view(vec![]).writable);
        lib.inner().apply_scan(&backlog);
        assert_eq!(lib.inner().manifest.entries.len(), 2);
        assert!(lib.auto_process(since).await.is_empty());
        assert!(!lib.inner().baseline_scan);
    }

    fn saved_queue(lib: &Library) -> Vec<String> {
        let paths = lib.inner().paths.clone().unwrap();
        match manifest::load(&paths).unwrap() {
            manifest::Loaded::Ok(m) => m.queue,
            _ => panic!("library.json unreadable"),
        }
    }

    #[tokio::test]
    async fn the_queue_is_saved_until_each_job_ends() {
        let (_t, lib, id, _dir) = processed_library();
        let root = lib.root().unwrap();
        std::fs::write(root.join("b.mp4"), b"second").unwrap();
        lib.inner().apply_scan(&[seen(&root, "a.mp4"), seen(&root, "b.mp4")]);
        let b = lib.inner().manifest.entries.iter().find(|e| e.file_name == "b.mp4").unwrap().id.clone();

        lib.enqueue(&id).await.unwrap();
        lib.enqueue(&b).await.unwrap();
        assert_eq!(saved_queue(&lib), vec![id.clone(), b.clone()]);
        // Starting a job keeps it there; finishing removes it.
        let (started, _token) = lib.start_next().unwrap();
        assert_eq!(started, id);
        assert_eq!(saved_queue(&lib), vec![id.clone(), b.clone()]);
        lib.finish_job(&id, &Some(Ok(())));
        assert_eq!(saved_queue(&lib), vec![b.clone()]);
        // Cancelling a waiting one removes it.
        lib.cancel(&b).unwrap();
        assert!(saved_queue(&lib).is_empty());

        // A cancelled running job leaves when it has stopped.
        lib.enqueue(&b).await.unwrap();
        lib.start_next().unwrap();
        lib.cancel(&b).unwrap();
        assert_eq!(saved_queue(&lib), vec![b.clone()]);
        lib.finish_job(&b, &None);
        assert!(saved_queue(&lib).is_empty());
    }

    /// A processing job of `id` at its commit, with a new cut and one
    /// thumbnail staged.
    async fn job_at_commit(lib: &Arc<Library>, id: &str) -> (StagingDir, JobOutputs, String) {
        lib.enqueue(id).await.unwrap();
        let (started, _token) = lib.start_next().unwrap();
        assert_eq!(started, id);
        let paths = lib.inner().paths.clone().unwrap();
        let staging = StagingDir::create(&paths, id).unwrap();
        std::fs::write(staging.0.join("trimmed.mp4"), b"new cut").unwrap();
        std::fs::write(staging.0.join("thumbnail_option_9.jpg"), b"new").unwrap();
        let hash = lib.inner().entry(id).unwrap().fingerprint.hash.clone();
        let outputs = JobOutputs {
            trimmed_name: "trimmed.mp4".into(),
            start: 1.5,
            end: 9.0,
            thumb_names: vec!["thumbnail_option_9.jpg".into()],
            duration: 60.0,
        };
        (staging, outputs, hash)
    }

    /// The commit swaps video.json under the video's lock, so a render
    /// being saved (which reads and writes it under that lock, not `inner`)
    /// can't interleave with it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_commit_waits_for_the_videos_lock() {
        let (_t, lib, id, dir) = processed_library();
        let (staging, outputs, hash) = job_at_commit(&lib, &id).await;
        let lock = lib.video_lock(&id);
        let held = hold(&lock);
        let (at_swap, reached) = std::sync::mpsc::channel();
        let commit = tokio::spawn({
            let (lib, id, dir, staging) = (lib.clone(), id.clone(), dir.clone(), staging.0.clone());
            async move { lib.commit_job(&id, &dir, &staging, &hash, outputs, move || at_swap.send(()).unwrap()).await }
        });
        reached.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // Can only fail if the swap doesn't wait for the lock.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(manifest::load_video_data(&dir).trimmed_file, None, "not swapped while the lock is held");
        assert!(!commit.is_finished());
        drop(held);
        commit.await.unwrap().unwrap();
        assert_eq!(manifest::load_video_data(&dir).trimmed_file.as_deref(), Some("trimmed.mp4"));
        drop(staging);
    }

    /// A render saved while a job is queued and committed: whichever comes
    /// first, video.json ends up the commit's, and every render it records
    /// is on disk, with no temp files left.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_render_saved_during_a_commit_stays_consistent() {
        for round in 0..20 {
            let (_t, lib, id, dir) = processed_library();
            let source = "thumbnail_option_1.jpg";
            let review = Review { title: Some("A".into()), preacher: Some("P".into()), ..Default::default() };
            lib.save_review(&id, review, None).unwrap();
            let key = test_key(&dir, source, "A");
            let render = tokio::spawn({
                let (lib, id, key) = (lib.clone(), id.clone(), key.clone());
                async move { lib.save_rendered_thumbnail(&id, source, &key, TV, "", png()).await }
            });
            if round % 2 == 0 {
                tokio::task::yield_now().await;
            }
            let (staging, outputs, hash) = job_at_commit(&lib, &id).await;
            lib.commit_job(&id, &dir, &staging.0, &hash, outputs, || {}).await.unwrap();
            let rendered = render.await.unwrap();
            drop(staging);
            lib.finish_job(&id, &Some(Ok(())));

            let data = manifest::load_video_data(&dir);
            assert_eq!((data.trimmed_file.as_deref(), data.trim_start), (Some("trimmed.mp4"), Some(1.5)), "round {round}");
            assert_eq!(data.thumbnail_options, ["thumbnail_option_9.jpg"]);
            for r in &data.renders {
                assert!(dir.join(&r.file).exists(), "round {round}: {} recorded but missing", r.file);
            }
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(manifest::RENDER_TMP_PREFIX))
                .collect();
            assert!(leftovers.is_empty(), "round {round}: {leftovers:?}");
            if let Err(e) = rendered {
                assert!(e.contains("processed"), "round {round}: {e}");
            }
        }
    }

    #[tokio::test]
    async fn a_cancel_during_the_commit_comes_too_late() {
        let (_t, lib, id, dir) = processed_library();
        let mut data = manifest::load_video_data(&dir);
        data.review.title = Some("Typed earlier".into());
        manifest::save_video_data(&dir, &data).unwrap();
        let (staging, outputs, hash) = job_at_commit(&lib, &id).await;

        lib.commit_job(&id, &dir, &staging.0, &hash, outputs, || lib.cancel(&id).unwrap()).await.unwrap();
        let token = lib.inner().running.as_ref().unwrap().cancel.clone();
        assert!(!token.is_cancelled(), "the commit isn't interrupted");
        let staged = staging.0.clone();
        drop(staging);
        assert!(!staged.exists(), "the work folder goes with the job");
        assert_eq!(lib.finish_job(&id, &Some(Ok(()))), Some(LATE_CANCEL));

        let d = lib.detail(&id).unwrap();
        assert_eq!(d.video.status, ViewStatus::ReadyForReview);
        assert_eq!((d.trim_start, d.trim_end), (Some(1.5), Some(9.0)));
        assert_eq!(std::fs::read(dir.join("trimmed.mp4")).unwrap(), b"new cut");
        assert_eq!(d.review.title.as_deref(), Some("Typed earlier"), "review edits are kept");
        assert!(!dir.join(COMMIT_BACKUP).exists());
        assert!(lib.inner().manifest.queue.is_empty());
    }

    #[tokio::test]
    async fn a_cancel_before_the_commit_changes_nothing() {
        let (_t, lib, id, dir) = processed_library();
        let (staging, outputs, hash) = job_at_commit(&lib, &id).await;
        lib.cancel(&id).unwrap();
        assert!(lib.commit_job(&id, &dir, &staging.0, &hash, outputs, || panic!("not committed")).await.is_err());
        drop(staging);
        assert_eq!(lib.finish_job(&id, &None), None);
        let d = lib.detail(&id).unwrap();
        assert_eq!(d.video.status, ViewStatus::ReadyForReview);
        assert!(!dir.join("trimmed.mp4").exists());
        assert_eq!(d.trim_start, None);
    }

    #[tokio::test]
    async fn a_queue_request_is_checked_again_after_the_file_check() {
        let (_t, lib, id, _dir) = processed_library();
        let root = lib.root().unwrap();
        let fp = lib.inner().entry(&id).unwrap().fingerprint.clone();

        // The library switched while the file was being looked at.
        let other = tempfile::tempdir().unwrap();
        lib.switch(other.path().to_string_lossy().to_string()).await.unwrap();
        assert!(lib.add_to_queue(&id, &root, &fp).is_err());
        assert!(lib.inner().queue.is_empty() && lib.inner().manifest.queue.is_empty());

        // Read-only, or the entry gone.
        let (_t, lib, id, _dir) = processed_library();
        let root = lib.root().unwrap();
        lib.inner().mode = Mode::Locked("Office iMac".into());
        assert!(lib.add_to_queue(&id, &root, &fp).is_err());
        lib.inner().mode = Mode::Writable;
        lib.inner().manifest.entries.clear();
        assert!(lib.add_to_queue(&id, &root, &fp).is_err());
        assert!(lib.inner().queue.is_empty());
    }

    #[test]
    fn the_saved_queue_is_restored_on_open() {
        let (_t, lib, done, _dir) = processed_library();
        let root = lib.root().unwrap();
        for f in ["waiting.mp4", "running.mp4", "gone.mp4", "ready.mp4"] {
            std::fs::write(root.join(f), f.as_bytes()).unwrap();
        }
        let ids: Vec<String> = {
            let mut g = lib.inner();
            g.entry_mut(&done).unwrap().status = EntryStatus::Done;
            let mut ids = Vec::new();
            for (name, status) in [
                ("waiting.mp4", EntryStatus::New),
                ("running.mp4", EntryStatus::Processing),
                ("gone.mp4", EntryStatus::New),
                ("ready.mp4", EntryStatus::ReadyForReview),
            ] {
                let mut e = Entry::new(name.into(), fsutil::fingerprint(&root.join(name)).unwrap(), Some(60.0));
                e.status = status;
                ids.push(e.id.clone());
                g.manifest.entries.push(e);
            }
            let unknown = crate::library::new_id();
            g.manifest.queue = vec![ids[1].clone(), done.clone(), ids[0].clone(), unknown, ids[2].clone(), ids[3].clone()];
            g.save().unwrap();
            ids
        };
        std::fs::remove_file(root.join("gone.mp4")).unwrap();

        let reopened = Library::new(root.to_string_lossy().to_string());
        reopened.open_blocking(true);
        // In order: the interrupted job, re-process requests of finished
        // videos, the one that was waiting; not an unknown id or a file
        // that's gone.
        let want = vec![ids[1].clone(), done.clone(), ids[0].clone(), ids[3].clone()];
        let g = reopened.inner();
        assert_eq!(g.queue.iter().cloned().collect::<Vec<_>>(), want);
        assert_eq!(g.manifest.queue, want);
        assert_eq!(g.status_of(g.manifest.get(&ids[1]).unwrap()), ViewStatus::Queued);
        assert_eq!(g.manifest.get(&ids[1]).unwrap().process_attempts, 1);
        assert_eq!(g.manifest.get(&ids[0]).unwrap().process_attempts, 0, "only the interrupted one counts");
        drop(g);
        assert_eq!(saved_queue(&reopened), want);
    }

    #[test]
    fn a_job_interrupted_twice_waits_for_retry() {
        let (_t, lib, _done, _dir) = processed_library();
        let root = lib.root().unwrap();
        std::fs::write(root.join("crashy.mp4"), b"crashy").unwrap();
        let id = {
            let mut g = lib.inner();
            let mut e = Entry::new("crashy.mp4".into(), fsutil::fingerprint(&root.join("crashy.mp4")).unwrap(), Some(60.0));
            e.status = EntryStatus::Processing;
            let id = e.id.clone();
            g.manifest.entries.push(e);
            g.manifest.queue = vec![id.clone()];
            g.save().unwrap();
            id
        };
        // The app stops while it processes; reopened, it starts again.
        let crash_and_reopen = || {
            let mut m = match manifest::load(&lib.inner().paths.clone().unwrap()).unwrap() {
                manifest::Loaded::Ok(m) => m,
                _ => panic!("library.json unreadable"),
            };
            m.get_mut(&id).unwrap().status = EntryStatus::Processing;
            manifest::save(&lib.inner().paths.clone().unwrap(), &m).unwrap();
            let reopened = Library::new(root.to_string_lossy().to_string());
            reopened.open_blocking(true);
            reopened
        };
        let once = crash_and_reopen();
        assert_eq!(once.inner().queue.iter().cloned().collect::<Vec<_>>(), vec![id.clone()]);
        drop(once);

        let twice = crash_and_reopen();
        let g = twice.inner();
        assert!(g.queue.is_empty() && g.manifest.queue.is_empty(), "not restarted a third time");
        let e = g.manifest.get(&id).unwrap();
        assert_eq!((e.status, e.process_attempts), (EntryStatus::Error, 2));
        assert_eq!(e.error.as_deref(), Some("Processing stopped unexpectedly twice — click Retry."));
        drop(g);

        // Retry starts the count again, and so does a job that succeeds.
        twice.inner().scanned = true;
        twice.inner().last.present.insert(id.clone());
        block_on(twice.enqueue(&id)).unwrap();
        assert_eq!(twice.inner().manifest.get(&id).unwrap().process_attempts, 0);
        twice.inner().entry_mut(&id).unwrap().process_attempts = 1;
        twice.start_next().unwrap();
        twice.finish_job(&id, &Some(Ok(())));
        assert_eq!(twice.inner().manifest.get(&id).unwrap().process_attempts, 0);
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn locked_library_errors_name_the_other_computer() {
        let (_t, lib, id, _dir) = processed_library();
        lib.inner().mode = Mode::Locked("Office iMac".into());
        let err = lib.reset(&id).unwrap_err();
        assert!(err.starts_with("This library is open on Office iMac."), "{err}");
        assert_eq!(lib.view(vec![]).message.as_deref(), Some(err.as_str()));
    }

    #[test]
    fn any_work_makes_the_library_not_idle() {
        let (_t, lib, id, _dir) = processed_library();
        assert!(lib.inner().is_idle());
        let work: [fn(&mut Inner, &str); 4] = [
            |g, id| g.queue.push_back(id.into()),
            |g, id| {
                g.busy.insert(id.into());
            },
            |g, _| g.rendering += 1,
            |g, id| {
                g.downloads.push(DownloadItem { job_id: "j".into(), youtube_id: id.into(), title: String::new(), cancel: None })
            },
        ];
        for add in work {
            add(&mut lib.inner(), &id);
            assert!(!lib.inner().is_idle());
            assert_eq!(lib.check_switch("/elsewhere"), Err(SWITCH_BUSY.to_string()));
            let mut g = lib.inner();
            g.queue.clear();
            g.busy.clear();
            g.rendering = 0;
            g.downloads.clear();
        }
    }

    #[test]
    fn busy_guard_frees_the_video_on_panic() {
        let (_t, lib, id, _dir) = processed_library();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _busy = lib.begin_op(&id).unwrap();
            assert!(lib.inner().busy.contains(&id));
            panic!("thumbnail pipeline crashed");
        }));
        assert!(r.is_err());
        assert!(!lib.inner().busy.contains(&id));
        assert!(lib.reset(&id).is_ok());
    }

    #[tokio::test]
    async fn heartbeat_reports_takeover_and_release() {
        let (_t, lib, _id, _dir) = processed_library();
        assert_eq!(lib.heartbeat().await, HeartbeatOutcome::Unchanged);
        let paths = lib.inner().paths.clone().unwrap();
        let other = lock::LockInfo {
            host: "Office iMac".into(),
            pid: 1,
            session: "other".into(),
            app_version: "0".into(),
            heartbeat: chrono::Utc::now().timestamp(),
            machine_id: Some("another-mac".into()),
            user: Some("someone".into()),
        };
        fsutil::write_json_atomic(&paths.lock_file(), &other).unwrap();
        assert_eq!(lib.heartbeat().await, HeartbeatOutcome::LostTo("Office iMac".into()));
        let view = lib.view(vec![]);
        assert!(!view.writable);
        assert_eq!(view.locked_by.as_deref(), Some("Office iMac"));
        assert_eq!(lib.heartbeat().await, HeartbeatOutcome::Unchanged);

        // The other Mac lets go.
        std::fs::remove_file(paths.lock_file()).unwrap();
        assert_eq!(lib.heartbeat().await, HeartbeatOutcome::Regained);
        lib.reopen().await.unwrap();
        assert!(lib.view(vec![]).writable);
    }

    #[tokio::test]
    async fn heartbeat_runs_while_a_scan_or_migration_holds_the_scan_lock() {
        let (_t, lib, _id, _dir) = processed_library();
        let paths = lib.inner().paths.clone().unwrap();
        let _scanning = lib.scan_lock.lock().await;
        let before = std::fs::read_to_string(paths.lock_file()).unwrap();
        lib.inner().mode = Mode::ReadOnly(UPDATING_LIBRARY.into());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let beat = tokio::time::timeout(std::time::Duration::from_secs(5), lib.heartbeat()).await;
        assert_eq!(beat.expect("heartbeat waited for the scan lock"), HeartbeatOutcome::Unchanged);
        // The migration's lock is kept fresh.
        assert_ne!(std::fs::read_to_string(paths.lock_file()).unwrap(), before);
    }

    #[tokio::test]
    async fn switch_refuses_while_busy() {
        let (_t, lib, id, _dir) = processed_library();
        let other = tempfile::tempdir().unwrap();
        let folder = other.path().to_string_lossy().to_string();
        let job = lib.begin_upload(&id, None, None).unwrap();
        assert!(lib.switch(folder.clone()).await.is_err());
        lib.finish_upload(&id, job, None, &public());
        lib.switch(folder).await.unwrap();
        let view = lib.view(vec![]);
        assert!(view.available && view.writable && view.videos.is_empty());
    }

    #[test]
    fn newer_schema_opens_read_only() {
        let (_t, lib, id, _dir) = processed_library();
        let paths = lib.inner().paths.clone().unwrap();
        lib.inner().save().unwrap();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(paths.manifest()).unwrap()).unwrap();
        raw["schema_version"] = serde_json::json!(SCHEMA_NEXT);
        raw["min_app_version"] = serde_json::json!("1.2.0");
        std::fs::write(paths.manifest(), raw.to_string()).unwrap();

        lib.open_blocking(true);
        let view = lib.view(vec![]);
        assert!(view.available && !view.writable);
        assert_eq!(view.videos.len(), 1, "still shown");
        let msg = view.message.unwrap();
        assert!(msg.starts_with("This library was updated by a newer Sermon Cut — please update"), "{msg}");
        assert!(msg.contains("1.2.0"), "{msg}");
        assert!(lib.reset(&id).is_err());
        // Never rewritten.
        let after: serde_json::Value = serde_json::from_slice(&std::fs::read(paths.manifest()).unwrap()).unwrap();
        assert_eq!(after["schema_version"], SCHEMA_NEXT);
    }

    const SCHEMA_NEXT: u32 = crate::library::SCHEMA_VERSION + 1;

    #[test]
    fn stale_volume_folders_are_unmounted_drives() {
        let stat = |devs: [(&'static str, u64, bool); 3]| {
            move |p: &Path| devs.iter().find(|(path, _, _)| Path::new(path) == p).map(|&(_, dev, symlink)| VolumeStat { dev, symlink })
        };
        let devs = [("/Volumes", 1, false), ("/Volumes/Church SSD", 1, false), ("/Volumes/Macintosh HD", 1, true)];
        // A leftover folder on the startup disk: the drive isn't there.
        assert_eq!(unmounted_volume(Path::new("/Volumes/Church SSD/Sermons"), stat(devs)).as_deref(), Some("Church SSD"));
        assert_eq!(unmounted_volume(Path::new("/Volumes/Church SSD"), stat(devs)).as_deref(), Some("Church SSD"));
        // The boot volume's link, a missing volume, and paths elsewhere are left to the usual checks.
        assert_eq!(unmounted_volume(Path::new("/Volumes/Macintosh HD/Users/a"), stat(devs)), None);
        assert_eq!(unmounted_volume(Path::new("/Volumes/Gone/Sermons"), stat(devs)), None);
        assert_eq!(unmounted_volume(Path::new("/Users/a/Movies"), stat(devs)), None);
        assert_eq!(unmounted_volume(Path::new("/Volumes"), stat(devs)), None);
        // Mounted: its own device.
        let mounted = [("/Volumes", 1, false), ("/Volumes/Church SSD", 7, false), ("/x", 0, false)];
        assert_eq!(unmounted_volume(Path::new("/Volumes/Church SSD/Sermons"), stat(mounted)), None);
    }

    #[test]
    fn lost_manifest_adopts_video_folders_on_the_next_scan() {
        let (_t, lib, id, dir) = processed_library();
        let paths = lib.inner().paths.clone().unwrap();
        let fp = lib.inner().manifest.get(&id).unwrap().fingerprint.clone();
        let hash = fp.hash.clone();
        manifest::save_transcript(
            &dir,
            &TranscriptFile {
                file_hash: hash,
                transcription: serde_json::from_value(serde_json::json!({"segments": [], "full_text": "hi"})).unwrap(),
            },
        )
        .unwrap();
        std::fs::write(dir.join("trimmed.mp4"), b"t").unwrap();
        let mut data = manifest::load_video_data(&dir);
        data.trimmed_file = Some("trimmed.mp4".into());
        manifest::save_video_data(&dir, &data).unwrap();
        lib.inner().save().unwrap();
        std::fs::remove_file(paths.manifest()).unwrap();
        let _ = std::fs::remove_file(paths.manifest_backup());

        lib.open_blocking(true);
        assert!(lib.view(vec![]).message.unwrap().contains("rebuilt"));
        // What a scan sees once the file is read (observe is tested in scan.rs).
        let obs = FileObs {
            name: "a.mp4".into(),
            size: fp.size,
            mtime_ms: fp.mtime_ms,
            state: ObsState::Ready,
            hash: Some(fp.hash.clone()),
            duration: Some(60.0),
        };
        lib.inner().apply_scan(&[obs]);
        let v = lib.view(vec![]).videos;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, id, "the folder's id comes back");
        assert_eq!(v[0].status, ViewStatus::ReadyForReview);
        assert_eq!(lib.detail(&id).unwrap().thumbnail_options.len(), 2);
        assert!(paths.manifest().exists());
    }

    #[tokio::test]
    async fn replaced_files_lose_their_old_outputs_outside_the_lock() {
        let (_t, lib, id, dir) = processed_library();
        assert!(dir.join("thumbnail_option_1.jpg").exists());
        let replaced = FileObs {
            name: "a.mp4".into(),
            size: 99,
            mtime_ms: i64::MAX / 2,
            state: ObsState::Ready,
            hash: Some("re-exported".into()),
            duration: Some(60.0),
        };
        // Busy from the reconcile until the folder is gone.
        let stale = lib.inner().apply_scan(std::slice::from_ref(&replaced));
        assert_eq!(stale, vec![id.clone()]);
        assert!(lib.inner().busy.contains(&id));
        assert!(lib.enqueue(&id).await.is_err());
        lib.inner().busy.remove(&id);

        let r = FileObs { hash: Some("again".into()), size: 100, ..replaced };
        lib.finish_scan(&[r]).await;
        assert!(!dir.exists());
        assert!(!lib.inner().busy.contains(&id));
        assert_eq!(lib.view(vec![]).videos[0].status, ViewStatus::New);
    }

    /// A video folder with old outputs, and a staging folder with new ones.
    fn commit_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("video");
        let staging = tmp.path().join("job");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&staging).unwrap();
        for (name, body) in [("transcript.json", "t"), ("video.json", "old"), ("trimmed.mp4", "old trim"), ("thumbnail_option_1.jpg", "old 1"), ("thumbnail_option_9.jpg", "old 9")] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        for (name, body) in [("trimmed.mp4", "new trim"), ("thumbnail_option_1.jpg", "new 1")] {
            std::fs::write(staging.join(name), body).unwrap();
        }
        (tmp, dir, staging)
    }

    fn contents(dir: &Path) -> Vec<(String, String)> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let body = std::fs::read_to_string(e.path()).unwrap_or_else(|_| "<dir>".into());
                (e.file_name().to_string_lossy().to_string(), body)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn commit_swaps_outputs_in() {
        let (_t, dir, staging) = commit_fixture();
        swap_in_outputs(&dir, &staging, || std::fs::write(dir.join("video.json"), "new").map_err(|e| e.to_string())).unwrap();
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            contents(&dir),
            vec![s("thumbnail_option_1.jpg", "new 1"), s("transcript.json", "t"), s("trimmed.mp4", "new trim"), s("video.json", "new")]
        );
    }

    #[test]
    fn failed_commit_puts_the_old_outputs_back() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        // The last staged item can't be moved in: a folder with something
        // in it already has its name.
        std::fs::create_dir_all(staging.join("zz_blocked")).unwrap();
        std::fs::write(staging.join("zz_blocked/x"), "x").unwrap();
        std::fs::create_dir_all(dir.join("zz_blocked")).unwrap();
        std::fs::write(dir.join("zz_blocked/y"), "y").unwrap();
        let before_with_blocker = contents(&dir);
        assert!(swap_in_outputs(&dir, &staging, || Ok(())).is_err());
        assert_eq!(contents(&dir), before_with_blocker);
        std::fs::remove_dir_all(dir.join("zz_blocked")).unwrap();
        assert_eq!(contents(&dir), before);

        // Writing video.json fails after everything moved: same.
        std::fs::remove_dir_all(staging.join("zz_blocked")).unwrap();
        assert!(swap_in_outputs(&dir, &staging, || Err("disk full".into())).is_err());
        assert_eq!(contents(&dir), before);
        assert!(staging.join("trimmed.mp4").exists(), "the new outputs are back in staging");
    }

    /// A crash during a commit, after `steps` of its moves: the journal is
    /// written, then each old output is moved aside, then each new one in.
    fn crash_after(dir: &Path, staging: &Path, steps: usize) {
        crash_after_where(dir, staging, steps, &|_| true);
    }

    #[test]
    fn a_leftover_backup_is_put_right_before_the_next_commit() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        // A crash midway left a backup; the next commit undoes it first.
        crash_after(&dir, &staging, 2);
        let (_t2, _d, staging2) = commit_fixture();
        let journal = begin_commit(&dir, &staging2, &|_| true).unwrap();
        assert!(journal.old.contains(&"thumbnail_option_1.jpg".to_string()), "the old outputs are back: {:?}", journal.old);
        let _ = std::fs::remove_dir_all(dir.join(COMMIT_BACKUP));
        assert_eq!(contents(&dir), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_backup_that_cant_be_put_back_stops_the_commit_with_a_way_out() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        crash_after(&dir, &staging, 2);
        // The backup can't be moved out of (as on a locked folder).
        let backup = dir.join(COMMIT_BACKUP);
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o500)).unwrap();
        let err = swap_in_outputs(&dir, &staging, || Ok(())).unwrap_err();
        assert_eq!(err, STUCK_COMMIT);
        assert!(err.contains(".commit-backup"), "the frontend keys its button on this");
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Once it can be, the next try puts it right.
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    fn crash_after_where(dir: &Path, staging: &Path, steps: usize, replaces: &dyn Fn(&str) -> bool) {
        let journal = begin_commit(dir, staging, replaces).unwrap();
        let backup = dir.join(COMMIT_BACKUP);
        let moves = journal
            .old
            .iter()
            .map(|n| (dir.join(n), backup.join(n)))
            .chain(journal.new.iter().map(|n| (staging.join(n), dir.join(n))));
        for (from, to) in moves.take(steps) {
            std::fs::rename(from, to).unwrap();
        }
    }

    #[test]
    fn a_crash_before_anything_moved_changes_nothing() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        // Only the backup folder was made.
        std::fs::create_dir(dir.join(COMMIT_BACKUP)).unwrap();
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
        // The journal was written too.
        crash_after(&dir, &staging, 0);
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    #[test]
    fn a_crash_while_moving_the_old_outputs_aside_keeps_them_all() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        // trimmed.mp4 is still in the folder; the thumbnails are aside.
        crash_after(&dir, &staging, 2);
        assert!(dir.join("trimmed.mp4").exists() && !dir.join("thumbnail_option_1.jpg").exists());
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    #[test]
    fn a_crash_while_moving_the_new_outputs_in_puts_the_old_back() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        std::fs::write(staging.join("thumbnail_option_5.jpg"), "new only").unwrap();
        // All three old outputs aside, two of three new ones in.
        crash_after(&dir, &staging, 5);
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    #[test]
    fn a_crash_after_video_json_was_written_puts_the_old_back() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        crash_after(&dir, &staging, 5);
        std::fs::write(dir.join("video.json"), "new").unwrap();
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    #[test]
    fn a_crash_during_a_rollback_finishes_it() {
        let (_t, dir, staging) = commit_fixture();
        let before = contents(&dir);
        crash_after(&dir, &staging, 5);
        let backup = dir.join(COMMIT_BACKUP);
        // The rollback took the new outputs out and put one old one back.
        for n in ["thumbnail_option_1.jpg", "trimmed.mp4"] {
            std::fs::rename(dir.join(n), staging.join(n)).unwrap();
        }
        std::fs::rename(backup.join("thumbnail_option_1.jpg"), dir.join("thumbnail_option_1.jpg")).unwrap();
        recover_commit(&dir);
        assert_eq!(contents(&dir), before);
    }

    #[test]
    fn a_crash_during_the_clean_up_keeps_the_new_outputs() {
        let (_t, dir, staging) = commit_fixture();
        crash_after(&dir, &staging, 5);
        std::fs::write(dir.join("video.json"), "new").unwrap();
        let backup = dir.join(COMMIT_BACKUP);
        let mut journal: CommitJournal = fsutil::read_json(&backup.join(COMMIT_JOURNAL), 1 << 20).unwrap().unwrap();
        journal.phase = CommitPhase::Done;
        write_journal(&backup, &journal).unwrap();
        let after = contents(&dir).into_iter().filter(|(n, _)| n != COMMIT_BACKUP).collect::<Vec<_>>();
        recover_commit(&dir);
        assert!(!backup.exists());
        assert_eq!(contents(&dir), after);
        assert!(!dir.join("thumbnail_option_9.jpg").exists());
    }

    /// A video with a custom thumbnail (rendered), two generated options
    /// (the second rendered and selected), and three regenerated ones staged.
    fn regenerate_fixture(selected: &str) -> (tempfile::TempDir, PathBuf, PathBuf, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("video");
        let staging = tmp.path().join("thumbs");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&staging).unwrap();
        let custom_render = "thumbnail_rendered_custom_thumbnail_1_00000000000000000000000000000001.jpg";
        let option_render = "thumbnail_rendered_thumbnail_option_2_00000000000000000000000000000002.jpg";
        for name in [
            "transcript.json",
            "trimmed.mp4",
            "custom_thumbnail_1.jpg",
            "thumbnail_option_1.jpg",
            "thumbnail_option_1_raw.jpg",
            "thumbnail_option_2.jpg",
            "thumbnail_option_2_raw.jpg",
            custom_render,
            option_render,
        ] {
            std::fs::write(dir.join(name), format!("old {name}")).unwrap();
        }
        let render = |source: &str, file: &str| Render { source: source.into(), file: file.into(), render_key: "k".into() };
        let data = VideoData {
            trimmed_file: Some("trimmed.mp4".into()),
            thumbnail_options: vec!["custom_thumbnail_1.jpg".into(), "thumbnail_option_1.jpg".into(), "thumbnail_option_2.jpg".into()],
            selected_source: Some(selected.into()),
            renders: vec![render("custom_thumbnail_1.jpg", custom_render), render("thumbnail_option_2.jpg", option_render)],
            ..Default::default()
        };
        manifest::save_video_data(&dir, &data).unwrap();
        let names: Vec<String> = (1..=3).map(|n| format!("thumbnail_option_{n}.jpg")).collect();
        for n in 1..=3 {
            for name in [format!("thumbnail_option_{n}.jpg"), format!("thumbnail_option_{n}_raw.jpg")] {
                std::fs::write(staging.join(&name), format!("new {name}")).unwrap();
            }
        }
        (tmp, dir, staging, names)
    }

    #[test]
    fn regenerating_keeps_custom_thumbnails_and_maps_the_selection() {
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        commit_regenerated(&dir, &staging, &names).unwrap();
        let d = manifest::load_video_data(&dir);
        assert_eq!(
            d.thumbnail_options,
            vec!["custom_thumbnail_1.jpg", "thumbnail_option_1.jpg", "thumbnail_option_2.jpg", "thumbnail_option_3.jpg"]
        );
        // The second generated option was selected: the new second one is.
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        // The custom one's render stays; the replaced option's goes.
        assert_eq!(d.renders.iter().map(|r| r.source.as_str()).collect::<Vec<_>>(), vec!["custom_thumbnail_1.jpg"]);
        let files: Vec<String> = contents(&dir).into_iter().map(|(n, _)| n).collect();
        assert!(!files.iter().any(|f| f.contains("thumbnail_option_2_0000")), "{files:?}");
        assert!(files.iter().any(|f| f.contains("custom_thumbnail_1_0000")));
        for (name, body) in contents(&dir) {
            if name.starts_with("thumbnail_option_") {
                assert!(body.starts_with("new"), "{name}: {body}");
            } else if name != "video.json" && name != "transcript.json" {
                assert!(body.starts_with("old"), "{name}: {body}");
            }
        }

        // A selected custom thumbnail stays selected.
        let (_t, dir, staging, names) = regenerate_fixture("custom_thumbnail_1.jpg");
        commit_regenerated(&dir, &staging, &names).unwrap();
        assert_eq!(manifest::load_video_data(&dir).selected_source.as_deref(), Some("custom_thumbnail_1.jpg"));

        // A selected option past the new ones' end: the first new one.
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        for f in ["thumbnail_option_2.jpg", "thumbnail_option_2_raw.jpg", "thumbnail_option_3.jpg", "thumbnail_option_3_raw.jpg"] {
            std::fs::remove_file(staging.join(f)).unwrap();
        }
        commit_regenerated(&dir, &staging, &names[..1]).unwrap();
        let d = manifest::load_video_data(&dir);
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        assert_eq!(d.thumbnail_options, vec!["custom_thumbnail_1.jpg", "thumbnail_option_1.jpg"]);
        assert!(!dir.join("thumbnail_option_2.jpg").exists() && !dir.join("thumbnail_option_2_raw.jpg").exists());
    }

    #[test]
    fn a_crash_while_regenerating_leaves_the_old_thumbnails() {
        // After each step: journal written, each old file moved aside, each
        // new one moved in, and video.json written.
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        let plan = RegeneratePlan::new(&manifest::load_video_data(&dir), &names);
        let replaces = |n: &str| plan.replaces(n);
        let moves = names_in(&dir).unwrap().iter().filter(|n| is_output(n) && replaces(n)).count() + 6;
        assert_eq!(moves, 5 + 6);
        for steps in 0..=moves {
            for write_data in [false, true] {
                let (_t, dir, staging, _) = regenerate_fixture("thumbnail_option_2.jpg");
                let before = contents(&dir);
                crash_after_where(&dir, &staging, steps, &replaces);
                if write_data {
                    manifest::save_video_data(&dir, &plan.data).unwrap();
                }
                recover_commit(&dir);
                assert_eq!(contents(&dir), before, "after {steps} moves");
            }
        }

        // A crash during the clean-up, once the commit was marked done: the
        // new set stays, and the custom thumbnail with it.
        crash_after_where(&dir, &staging, moves, &replaces);
        manifest::save_video_data(&dir, &plan.data).unwrap();
        let backup = dir.join(COMMIT_BACKUP);
        let mut journal: CommitJournal = fsutil::read_json(&backup.join(COMMIT_JOURNAL), 1 << 20).unwrap().unwrap();
        journal.phase = CommitPhase::Done;
        write_journal(&backup, &journal).unwrap();
        recover_commit(&dir);
        assert!(!backup.exists());
        let d = manifest::load_video_data(&dir);
        assert_eq!(d.thumbnail_options.len(), 4);
        assert_eq!(std::fs::read_to_string(dir.join("custom_thumbnail_1.jpg")).unwrap(), "old custom_thumbnail_1.jpg");
        assert_eq!(std::fs::read_to_string(dir.join("thumbnail_option_3.jpg")).unwrap(), "new thumbnail_option_3.jpg");
    }

    #[test]
    fn raw_variants() {
        assert_eq!(raw_variant("thumbnail_option_3.jpg").as_deref(), Some("thumbnail_option_3_raw.jpg"));
        assert_eq!(raw_variant("thumbnail_option_3_raw.jpg"), None);
        assert_eq!(raw_variant("custom_thumbnail_1.jpg"), None);
    }

    #[test]
    fn custom_thumbnails_keep_three_newest_and_the_selected_one() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut options = names(&[
            "custom_thumbnail_5.jpg",
            "custom_thumbnail_4.jpg",
            "custom_thumbnail_3.jpg",
            "custom_thumbnail_2.jpg",
            "custom_thumbnail_1.jpg",
            "thumbnail_option_1.jpg",
        ]);
        let pruned = prune_custom_thumbnails(&mut options, Some("custom_thumbnail_1.jpg"));
        assert_eq!(pruned, names(&["custom_thumbnail_2.jpg"]));
        assert_eq!(
            options,
            names(&[
                "custom_thumbnail_5.jpg",
                "custom_thumbnail_4.jpg",
                "custom_thumbnail_3.jpg",
                "custom_thumbnail_1.jpg",
                "thumbnail_option_1.jpg",
            ])
        );
        // Generated options are never pruned.
        let mut options = names(&["thumbnail_option_1.jpg", "thumbnail_option_2.jpg"]);
        assert!(prune_custom_thumbnails(&mut options, None).is_empty());
        assert_eq!(options.len(), 2);
    }

    #[test]
    fn file_in_dir_rejects_outside_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("v");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("thumbnail_option_1.jpg"), b"x").unwrap();
        std::fs::write(tmp.path().join("secret.jpg"), b"x").unwrap();
        let ok = dir.join("thumbnail_option_1.jpg");
        assert_eq!(file_in_dir(&dir, &ok.to_string_lossy()).unwrap(), "thumbnail_option_1.jpg");
        assert!(file_in_dir(&dir, &tmp.path().join("secret.jpg").to_string_lossy()).is_err());
        assert!(file_in_dir(&dir, &dir.join("../secret.jpg").to_string_lossy()).is_err());
        assert!(file_in_dir(&dir, "/etc/passwd").is_err());
    }
}
