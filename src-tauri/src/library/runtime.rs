//! The open library: state, scanning, the processing queue and actions.
//!
//! Locking: `inner` is a std mutex held only for short, non-async work.
//! Slow I/O (hashing, ffprobe, ffmpeg, uploads) happens outside it.

use super::fsutil::{self, TMP_MARKER};
use super::lock::{self, Acquire};
use super::manifest::{self, Loaded, Manifest, Review, TranscriptFile, VideoData};
use super::scan::{self, FileObs, ObsState, Reconciled, ScanCache};
use super::{Entry, EntryStatus, Fingerprint, LibraryPaths, UploadRecord, MAX_TRANSCRIBE_SECS};
use crate::state::{AppState, ProcessingStage, TranscriptionResult};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

pub type AppStateHandle = Arc<tokio::sync::Mutex<AppState>>;

const FILE_CHANGED: &str =
    "The file changed since it was added. If it's still copying, wait for it to finish, then try again.";

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Writable,
    /// Shown, but nothing is written: newer format, another Mac has it open,
    /// or the folder can't be written.
    ReadOnly(String),
    /// Folder missing or unreadable (drive unplugged). Last-known list shown.
    Unavailable(String),
}

struct Running {
    id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
    cancel: CancellationToken,
    previous: EntryStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadItem {
    pub job_id: String,
    pub youtube_id: String,
    pub title: String,
    #[serde(skip)]
    cancel: Option<tokio::sync::watch::Sender<bool>>,
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
    /// YouTube uploads in flight: entry id → percent.
    uploads: std::collections::HashMap<String, u32>,
    downloads: Vec<DownloadItem>,
    session: String,
    notice: Option<String>,
}

pub struct Library {
    inner: Mutex<Inner>,
    scan_lock: tokio::sync::Mutex<()>,
    wake: tokio::sync::Notify,
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
    pub thumbnail_options: Vec<String>,
    pub selected_thumbnail: Option<String>,
    pub review: Review,
}

// ---------------------------------------------------------------------------

fn path_string(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
}

#[derive(Debug, Clone, Serialize)]
struct ProcessingProgressEvent {
    video_id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Result<T, String>> {
    let handle = tokio::task::spawn_blocking(f);
    async move { handle.await.map_err(|e| format!("Background task failed: {}", e)) }
}

/// Logo setting, or the bundled default.
pub fn effective_logo(app: &AppHandle, logo_path: Option<String>) -> Option<String> {
    match logo_path {
        Some(path) if !path.is_empty() => Some(path),
        _ => app
            .path()
            .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
            .ok()
            .map(|p| path_string(&p)),
    }
}

impl Inner {
    fn writable(&self) -> Result<&LibraryPaths, String> {
        match (&self.mode, &self.paths) {
            (Mode::Writable, Some(paths)) => Ok(paths),
            (Mode::ReadOnly(why), _) | (Mode::Unavailable(why), _) => Err(why.clone()),
            _ => Err("The library isn't open".into()),
        }
    }

    fn save(&self) -> Result<(), String> {
        let paths = self.writable()?;
        manifest::save(paths, &self.manifest)
    }

    fn save_logged(&self) {
        if matches!(self.mode, Mode::Writable) {
            if let Err(e) = self.save() {
                eprintln!("[library] Failed to save library.json: {}", e);
            }
        }
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
        LibraryVideo {
            id: e.id.clone(),
            file_name: e.file_name.clone(),
            file_path: paths.map(|p| path_string(&p.root.join(&e.file_name))).unwrap_or_default(),
            title: e.display_title(),
            status,
            error: match status {
                ViewStatus::Error => e.error.clone().or_else(|| Some("Something went wrong".into())),
                ViewStatus::Unreadable => Some("The file changed and Sermon Cut can't read it any more. It may be incomplete or damaged.".into()),
                _ => None,
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
            upload_progress: self.uploads.get(&e.id).copied(),
            busy: self.is_busy(&e.id),
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
        }
    }

    fn view(&self, notices: Vec<String>) -> LibraryView {
        let mut videos: Vec<LibraryVideo> = self.manifest.entries.iter().map(|e| self.video_view(e)).collect();
        videos.extend(self.last.copying.iter().chain(self.last.unreadable.iter()).map(|o| self.pending_view(o)));
        // Newest first.
        videos.sort_by(|a, b| b.added_at.cmp(&a.added_at));

        let (available, writable, message, locked_by) = match &self.mode {
            Mode::Writable => (true, true, self.notice.clone(), None),
            Mode::ReadOnly(why) => {
                let locked = why.strip_prefix("locked:").map(|h| h.to_string());
                let message = match &locked {
                    Some(host) => format!(
                        "This library is open on {}. It's read-only here so the two computers don't overwrite each other's changes.",
                        host
                    ),
                    None => why.clone(),
                };
                (true, false, Some(message), locked)
            }
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

/// Delete stale temp files from a crashed run: everything in tmp/, and
/// half-written JSON (`*.tmp-*`) next to the real files.
fn sweep(paths: &LibraryPaths) {
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
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().contains(TMP_MARKER) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Remove generated files from a video folder, keeping the transcript and
/// review data (used before committing new outputs).
fn clear_outputs(dir: &Path, keep_custom: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let keep = name == "transcript.json"
            || name == "video.json"
            || (keep_custom && name.starts_with("custom_thumbnail_"));
        if !keep && e.path().is_file() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn fingerprint_matches(now: &Fingerprint, expected: &Fingerprint) -> bool {
    now.size == expected.size && now.hash == expected.hash
}

fn open_paths(configured: &str) -> Result<LibraryPaths, Mode> {
    let raw = PathBuf::from(configured);
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
                uploads: std::collections::HashMap::new(),
                downloads: Vec::new(),
                session: super::new_id(),
                notice: None,
            }),
            scan_lock: tokio::sync::Mutex::new(()),
            wake: tokio::sync::Notify::new(),
        });
        lib.inner().configured = configured;
        lib
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let (manifest, mut mode) = match paths.ensure_dir(&paths.data_dir()) {
            Err(e) => (
                Manifest::default(),
                Mode::ReadOnly(format!("Sermon Cut can't save its data in this folder ({}). Choose a folder you can write to.", e)),
            ),
            Ok(()) => match manifest::load(&paths) {
                Ok(Loaded::Ok(m)) => (m, Mode::Writable),
                Ok(Loaded::Corrupt(m, aside)) => {
                    notice = Some(format!(
                        "The library's tracking file couldn't be read, so it was set aside as .sermon-cut/{} and the list was rebuilt. Videos show as new.",
                        aside
                    ));
                    (m, Mode::Writable)
                }
                Ok(Loaded::Newer(m, v)) => (
                    m,
                    Mode::ReadOnly(format!(
                        "This library was last used by a newer version of Sermon Cut (format {}). Update Sermon Cut to make changes.",
                        v
                    )),
                ),
                Err(e) => (Manifest::default(), Mode::Unavailable(e)),
            },
        };

        let session = self.inner().session.clone();
        if mode == Mode::Writable {
            match lock::acquire(&paths, &session, force_lock) {
                Ok(Acquire::Acquired) => {}
                Ok(Acquire::HeldBy(host)) => mode = Mode::ReadOnly(format!("locked:{}", host)),
                Err(e) => mode = Mode::ReadOnly(format!("Sermon Cut can't write to this folder: {}", e)),
            }
        }

        let mut manifest = manifest;
        if mode == Mode::Writable {
            sweep(&paths);
            if manifest::mark_interrupted(&mut manifest) {
                if let Err(e) = manifest::save(&paths, &manifest) {
                    eprintln!("[library] Failed to save library.json: {}", e);
                }
            }
        }

        let mut g = self.inner();
        // Interrupted entries get their running job back if one is somehow still alive.
        g.paths = Some(paths);
        g.manifest = manifest;
        g.mode = mode;
        g.notice = notice;
        g.cache = ScanCache::default();
        g.last = Reconciled::default();
        g.scanned = false;
    }

    /// Whether the library could move to `configured` right now.
    pub fn check_switch(&self, configured: &str) -> Result<(), String> {
        let g = self.inner();
        if g.configured == configured {
            return Ok(());
        }
        if g.running.is_some() || !g.queue.is_empty() || !g.downloads.is_empty() || !g.busy.is_empty() {
            return Err("Wait for processing, downloads and uploads to finish before changing the library folder.".into());
        }
        Ok(())
    }

    /// Point the library at a different folder (Settings).
    pub async fn switch(self: &Arc<Self>, configured: String) -> Result<(), String> {
        self.check_switch(&configured)?;
        if self.inner().configured == configured {
            return Ok(());
        }
        let _scan = self.scan_lock.lock().await;
        let lib = self.clone();
        blocking(move || {
            let (paths, session) = {
                let mut g = lib.inner();
                g.configured = configured;
                (g.paths.take(), g.session.clone())
            };
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

    pub fn release_lock(&self) {
        let g = self.inner();
        if let (Some(paths), Mode::Writable) = (&g.paths, &g.mode) {
            lock::release(paths, &g.session);
        }
    }

    pub async fn take_over(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let lib = self.clone();
        blocking(move || lib.open_blocking(true)).await
    }

    /// Refresh lock.json; go read-only if another computer took over.
    /// Returns true when another computer has let go of a library we were
    /// locked out of, so it should be reopened for writing.
    pub fn heartbeat(&self) -> bool {
        let mut g = self.inner();
        let Some(paths) = g.paths.clone() else { return false };
        match g.mode.clone() {
            Mode::Writable => {
                match lock::heartbeat(&paths, &g.session) {
                    Ok(Some(host)) => g.mode = Mode::ReadOnly(format!("locked:{}", host)),
                    Ok(None) => {}
                    Err(e) => eprintln!("[library] Lock heartbeat failed: {}", e),
                }
                false
            }
            Mode::ReadOnly(why) if why.starts_with("locked:") => {
                matches!(lock::acquire(&paths, &g.session, false), Ok(Acquire::Acquired))
            }
            _ => false,
        }
    }

    pub async fn reopen(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let lib = self.clone();
        blocking(move || lib.open_blocking(false)).await
    }

    /// Migrate pre-0.4 videos if there are any and the library is writable.
    async fn migrate_if_needed(self: &Arc<Self>, app_state: &AppStateHandle) {
        let legacy = {
            let state = app_state.lock().await;
            if state.legacy_videos.is_empty() {
                return;
            }
            state.legacy_videos.clone()
        };
        let lib = self.clone();
        let result = blocking(move || {
            let mut g = lib.inner();
            let paths = g.writable()?.clone();
            let result = super::migrate::migrate(&paths, &mut g.manifest, &legacy);
            // Save even after a partial failure: entries made so far point at
            // files already moved, and a re-run picks up from there.
            g.save()?;
            result.map(|outcome| (paths, outcome))
        })
        .await
        .and_then(|r| r);

        match result {
            Ok((paths, outcome)) => {
                let mut state = app_state.lock().await;
                if let Err(e) = AppState::backup_pre_library_state() {
                    eprintln!("[migrate] {}", e);
                    return;
                }
                state.legacy_videos.clear();
                if !outcome.outside.is_empty() {
                    state.notices.push(format!(
                        "These videos from before the library update are outside the library folder, so they're no longer listed. Copy them into the library folder to keep working on them: {}",
                        outcome.outside.join(", ")
                    ));
                }
                match state.save() {
                    Ok(()) => super::migrate::cleanup(&paths, &outcome),
                    Err(e) => eprintln!("[migrate] Failed to save state: {}", e),
                }
                println!("[migrate] Moved old videos into the library");
            }
            // Not writable yet (e.g. drive unplugged) or a move failed:
            // state.json is left alone and the next scan tries again.
            Err(e) => eprintln!("[migrate] Deferred: {}", e),
        }
    }

    /// Scan the folder. One at a time; I/O happens without the lock.
    pub async fn rescan(self: &Arc<Self>, app_state: &AppStateHandle) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;

        let needs_open = !matches!(self.inner().mode, Mode::Writable | Mode::ReadOnly(_));
        if needs_open {
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

        let mut g = self.inner();
        let (result, cache) = observed;
        g.cache = cache;
        let obs = match result {
            Ok(obs) => obs,
            Err(e) => {
                let folder = g.paths.as_ref().map(|p| path_string(&p.root)).unwrap_or_default();
                g.mode = Mode::Unavailable(format!(
                    "Can't read the library folder {} ({}). If it's on an external drive, plug it back in.",
                    folder, e
                ));
                return Ok(());
            }
        };
        let busy = g.busy_set();
        let reconciled = scan::reconcile(&mut g.manifest.entries, &obs, &busy);
        if g.mode == Mode::Writable {
            if let Some(paths) = g.paths.clone() {
                for id in &reconciled.replaced {
                    if let Err(e) = manifest::remove_video_dir(&paths, id) {
                        eprintln!("[library] {}", e);
                    }
                }
            }
            if reconciled.changed {
                g.save_logged();
            }
        }
        g.last = reconciled;
        g.scanned = true;
        Ok(())
    }

    pub fn view(&self, notices: Vec<String>) -> LibraryView {
        self.inner().view(notices)
    }

    pub fn detail(&self, id: &str) -> Result<VideoDetail, String> {
        let g = self.inner();
        let e = g.entry(id)?;
        let video = g.video_view(e);
        let paths = g.paths.clone().ok_or("The library isn't open")?;
        let hash = e.fingerprint.hash.clone();
        drop(g);

        let dir = paths.video_dir(id)?;
        let data = manifest::load_video_data(&dir);
        let transcription = manifest::load_transcript(&dir)
            .filter(|t| t.file_hash == hash)
            .map(|t| t.transcription);
        let abs = |name: &String| path_string(&dir.join(name));
        Ok(VideoDetail {
            video,
            transcription,
            trimmed_path: data.trimmed_file.as_ref().map(abs).filter(|p| Path::new(p).exists()),
            trim_start: data.trim_start,
            trim_end: data.trim_end,
            thumbnail_options: data.thumbnail_options.iter().map(abs).collect(),
            selected_thumbnail: data.selected_thumbnail.as_ref().map(abs),
            review: data.review,
        })
    }

    // -----------------------------------------------------------------------
    // Queue

    pub async fn enqueue(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (path, expected) = {
            let g = self.inner();
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
            (paths.original(&e.file_name)?, e.fingerprint.clone())
        };

        // Quick re-check now (size + mtime); the job re-hashes before and after.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("Can't read {}: {}", path.display(), e))?;
        if meta.len() != expected.size || fsutil::mtime_ms(&meta) != expected.mtime_ms {
            return Err(FILE_CHANGED.into());
        }

        let mut g = self.inner();
        if g.is_busy(id) {
            return Err("This video is already queued or processing".into());
        }
        g.queue.push_back(id.to_string());
        drop(g);
        self.wake.notify_one();
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let mut g = self.inner();
        g.queue.retain(|q| q != id);
        if let Some(r) = g.running.as_ref().filter(|r| r.id == id) {
            r.cancel.cancel();
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

    /// Runs queued jobs one at a time, forever.
    pub async fn worker(self: Arc<Self>, app: AppHandle, app_state: AppStateHandle) {
        loop {
            let next = {
                let mut g = self.inner();
                let next = g.queue.pop_front();
                if let Some(id) = &next {
                    let token = CancellationToken::new();
                    let previous = match g.entry_mut(id) {
                        Ok(e) => {
                            let previous = e.status;
                            e.status = EntryStatus::Processing;
                            e.error = None;
                            previous
                        }
                        Err(_) => continue,
                    };
                    g.running = Some(Running {
                        id: id.clone(),
                        stage: ProcessingStage::ExtractingAudio,
                        progress: 0.0,
                        message: "Starting...".into(),
                        cancel: token.clone(),
                        previous,
                    });
                    g.save_logged();
                    next.map(|id| (id, token))
                } else {
                    None
                }
            };
            let Some((id, token)) = next else {
                self.wake.notified().await;
                continue;
            };
            emit_changed(&app);

            let result = tokio::select! {
                r = self.run_job(&app, &app_state, &id) => Some(r),
                _ = token.cancelled() => None,
            };

            let staging = {
                let mut g = self.inner();
                let running = g.running.take();
                let staging = g.paths.as_ref().map(|p| p.tmp_dir().join(format!("job-{}", id)));
                if let Ok(e) = g.entry_mut(&id) {
                    match &result {
                        Some(Ok(())) => {}
                        Some(Err(err)) => {
                            e.status = EntryStatus::Error;
                            e.error = Some(err.clone());
                        }
                        None => {
                            e.status = running.map(|r| r.previous).unwrap_or(EntryStatus::New);
                            if e.status == EntryStatus::Processing {
                                e.status = EntryStatus::New;
                            }
                            e.error = None;
                        }
                    }
                }
                g.save_logged();
                staging
            };
            if let Some(staging) = staging {
                let _ = std::fs::remove_dir_all(staging);
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
        let (paths, file_name, expected, duration, display_title) = {
            let g = self.inner();
            let paths = g.writable()?.clone();
            let e = g.entry(id)?;
            (paths, e.file_name.clone(), e.fingerprint.clone(), e.duration, e.display_title())
        };
        let (api_url, api_key, logo) = {
            let s = app_state.lock().await;
            (
                s.settings.modal_api_url.clone(),
                s.settings.modal_api_key.clone(),
                s.settings.logo_path.clone(),
            )
        };
        let video_path = paths.original(&file_name)?;

        self.set_progress(app, id, ProcessingStage::ExtractingAudio, 0.0, "Checking the video file...");
        let check_path = video_path.clone();
        let now = blocking(move || fsutil::fingerprint(&check_path)).await??;
        if !fingerprint_matches(&now, &expected) {
            return Err(FILE_CHANGED.into());
        }

        paths.ensure_dir(&paths.tmp_dir())?;
        let staging = paths.tmp_dir().join(format!("job-{}", id));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir(&staging).map_err(|e| format!("Failed to create work folder: {}", e))?;
        let video_dir = paths.ensure_video_dir(id)?;

        let cached = manifest::load_transcript(&video_dir).filter(|t| t.file_hash == expected.hash);
        let transcription = match cached {
            Some(t) => {
                println!("[process] Using saved transcript for {}", file_name);
                t.transcription
            }
            None => {
                self.set_progress(app, id, ProcessingStage::ExtractingAudio, 5.0, "Extracting audio from video...");
                let audio = staging.join("audio.ogg");
                crate::ffmpeg::extract_audio(&video_path, &audio).await?;

                self.set_progress(app, id, ProcessingStage::Transcribing, 25.0, "Transcribing audio with AI...");
                let result = crate::modal::transcribe_audio(&path_string(&audio), &api_url, api_key.as_deref()).await;
                let _ = std::fs::remove_file(&audio);
                let transcription = result?;
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

        self.set_progress(app, id, ProcessingStage::Trimming, 50.0, "Trimming video to sermon boundaries...");
        let duration = match duration {
            Some(d) => d,
            None => crate::ffmpeg::get_duration(&path_string(&video_path)).await?,
        };
        let mut start = transcription.sermon_start.unwrap_or(0.0).clamp(0.0, duration);
        let mut end = transcription.sermon_end.unwrap_or(duration).clamp(0.0, duration);
        if end <= start {
            start = 0.0;
            end = duration;
        }
        let ext = Path::new(&file_name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| super::VIDEO_EXTENSIONS.contains(&e.as_str()))
            .unwrap_or_else(|| "mp4".into());
        let trimmed_name = format!("trimmed.{}", ext);
        crate::ffmpeg::trim_video(&video_path, start, end, &staging.join(&trimmed_name)).await?;

        self.set_progress(app, id, ProcessingStage::GeneratingThumbnails, 75.0, "Generating thumbnail options...");
        let title = transcription.suggested_title.clone().unwrap_or(display_title);
        let logo = effective_logo(app, logo);
        let thumbs = crate::thumbnail::generate_thumbnail_options(&video_path, start, end, &title, &staging, 10, logo.as_deref()).await?;
        let thumb_names: Vec<String> = thumbs
            .iter()
            .filter_map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        let check_path = video_path.clone();
        let after = blocking(move || fsutil::fingerprint(&check_path)).await??;
        if !fingerprint_matches(&after, &expected) {
            return Err("The file changed while it was being processed, so the results were thrown away. Try again once it has finished copying.".into());
        }

        // Commit: swap the new outputs in and mark the video ready.
        {
            let mut g = self.inner();
            g.writable()?;
            let e = g.entry(id)?;
            if e.fingerprint.hash != expected.hash {
                return Err(FILE_CHANGED.into());
            }
            let old = manifest::load_video_data(&video_dir);
            clear_outputs(&video_dir, false);
            let files = std::fs::read_dir(&staging).map_err(|e| format!("Failed to read work folder: {}", e))?;
            for f in files.flatten() {
                std::fs::rename(f.path(), video_dir.join(f.file_name()))
                    .map_err(|e| format!("Failed to save results: {}", e))?;
            }
            let data = VideoData {
                trimmed_file: Some(trimmed_name),
                trim_start: Some(start),
                trim_end: Some(end),
                selected_thumbnail: thumb_names.first().cloned(),
                thumbnail_options: thumb_names,
                review: Review {
                    title: None,
                    description: None,
                    preacher: old.review.preacher,
                },
            };
            manifest::save_video_data(&video_dir, &data)?;
            let e = g.entry_mut(id)?;
            e.status = EntryStatus::ReadyForReview;
            e.error = None;
            e.duration = Some(duration);
            g.save()?;
        }
        self.set_progress(app, id, ProcessingStage::Complete, 100.0, "Processing complete!");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Actions

    fn with_entry<T>(&self, id: &str, f: impl FnOnce(&mut Inner, &LibraryPaths) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.inner();
        let paths = g.writable()?.clone();
        g.entry(id)?;
        f(&mut g, &paths)
    }

    /// Back to New: delete what processing made. Keeps upload history.
    pub fn reset(&self, id: &str) -> Result<(), String> {
        self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            manifest::remove_video_dir(paths, id)?;
            let e = g.entry_mut(id)?;
            e.status = EntryStatus::New;
            e.error = None;
            g.save()
        })
    }

    pub fn set_hidden(&self, id: &str, hidden: bool) -> Result<(), String> {
        self.with_entry(id, |g, _| {
            g.require_idle(id)?;
            g.entry_mut(id)?.hidden = hidden;
            g.save()
        })
    }

    /// Drop a video whose file is gone.
    pub fn forget(&self, id: &str) -> Result<(), String> {
        self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            if g.present(id) {
                return Err("The file is still in the library folder. Use Move to Trash or Hide instead.".into());
            }
            manifest::remove_video_dir(paths, id)?;
            g.manifest.entries.retain(|e| e.id != id);
            g.save()
        })
    }

    /// Move the user's file to the Trash, then delete the app's data for it.
    pub async fn trash(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let path = self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            let e = g.entry(id)?;
            let path = paths.original(&e.file_name)?;
            g.busy.insert(id.to_string());
            Ok(path)
        })?;
        let lib = self.clone();
        let check = path.clone();
        let result = blocking(move || -> Result<(), String> {
            let paths = lib.inner().paths.clone().ok_or("The library isn't open")?;
            paths.verify_original(&check)?;
            move_to_trash(&check)
        })
        .await
        .and_then(|r| r);

        let mut g = self.inner();
        g.busy.remove(id);
        result.map_err(|e| {
            format!(
                "Couldn't move \"{}\" to the Trash: {}. Some network and external drives have no Trash; delete the file in Finder if you want it gone.",
                path.file_name().unwrap_or_default().to_string_lossy(),
                e
            )
        })?;
        if let Some(paths) = g.paths.clone() {
            if let Err(e) = manifest::remove_video_dir(&paths, id) {
                eprintln!("[library] {}", e);
            }
        }
        g.manifest.entries.retain(|e| e.id != id);
        g.last.present.remove(id);
        g.save()
    }

    pub fn save_review(&self, id: &str, review: Review, selected_thumbnail: Option<String>) -> Result<(), String> {
        self.with_entry(id, |g, paths| {
            if g.running.as_ref().is_some_and(|r| r.id == id) {
                return Err("This video is being processed".into());
            }
            let dir = paths.ensure_video_dir(id)?;
            let mut data = manifest::load_video_data(&dir);
            data.review = review;
            // A selection that's no longer a file here (e.g. an overlay render
            // replaced it mid-edit) is ignored rather than losing the text edits.
            if let Some(name) = selected_thumbnail.and_then(|sel| file_in_dir(&dir, &sel).ok()) {
                data.selected_thumbnail = Some(name);
            }
            manifest::save_video_data(&dir, &data)
        })
    }

    /// Mark an id busy for an operation that runs outside the lock.
    fn begin_op(&self, id: &str) -> Result<(LibraryPaths, Entry, VideoData), String> {
        self.with_entry(id, |g, paths| {
            g.require_idle(id)?;
            g.busy.insert(id.to_string());
            let e = g.entry(id)?.clone();
            let data = manifest::load_video_data(&paths.video_dir(id)?);
            Ok((paths.clone(), e, data))
        })
    }

    fn end_op(&self, id: &str) {
        self.inner().busy.remove(id);
    }

    pub async fn regenerate_thumbnails(self: &Arc<Self>, app: &AppHandle, app_state: &AppStateHandle, id: &str) -> Result<Vec<String>, String> {
        let (paths, entry, data) = self.begin_op(id)?;
        let result = async {
            let logo = effective_logo(app, app_state.lock().await.settings.logo_path.clone());
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
                crate::thumbnail::generate_thumbnail_options(&video_path, start, end, &title, &staging, 10, logo.as_deref()).await;
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

            let _g = self.inner();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for f in entries.flatten() {
                    let n = f.file_name().to_string_lossy().to_string();
                    // Overlays rendered from the old options are stale too.
                    if n.starts_with("thumbnail_option_") || n.starts_with(RENDERED_PREFIX) {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
            }
            if let Ok(files) = std::fs::read_dir(&staging) {
                for f in files.flatten() {
                    let _ = std::fs::rename(f.path(), dir.join(f.file_name()));
                }
            }
            let _ = std::fs::remove_dir_all(&staging);
            let mut data = manifest::load_video_data(&dir);
            data.thumbnail_options = names.clone();
            data.selected_thumbnail = names.first().cloned();
            manifest::save_video_data(&dir, &data)?;
            Ok(names.iter().map(|n| path_string(&dir.join(n))).collect())
        }
        .await;
        self.end_op(id);
        result
    }

    pub async fn custom_thumbnail(
        self: &Arc<Self>,
        app: &AppHandle,
        app_state: &AppStateHandle,
        id: &str,
        source: String,
        crop: crate::thumbnail::CropRect,
        grading: bool,
        with_logo: bool,
    ) -> Result<String, String> {
        let (paths, _entry, _data) = self.begin_op(id)?;
        let result = async {
            let logo = if with_logo {
                effective_logo(app, app_state.lock().await.settings.logo_path.clone())
            } else {
                None
            };
            let dir = paths.ensure_video_dir(id)?;
            let path = crate::thumbnail::process_custom_thumbnail(&source, &dir, &crop, grading, logo.as_deref()).await?;
            let name = file_in_dir(&dir, &path)?;
            let _g = self.inner();
            let mut data = manifest::load_video_data(&dir);
            if !data.thumbnail_options.contains(&name) {
                data.thumbnail_options.insert(0, name.clone());
            }
            data.selected_thumbnail = Some(name.clone());
            manifest::save_video_data(&dir, &data)?;
            Ok(path_string(&dir.join(name)))
        }
        .await;
        self.end_op(id);
        result
    }

    /// Persist a thumbnail rendered in the webview (template system) into the
    /// video's library folder and select it, mirroring `custom_thumbnail` so
    /// the choice survives a restart.
    pub async fn save_rendered_thumbnail(
        self: &Arc<Self>,
        id: &str,
        png_bytes: Vec<u8>,
        filename: String,
    ) -> Result<String, String> {
        let (paths, _entry, _data) = self.begin_op(id)?;
        let result = async {
            let dir = paths.ensure_video_dir(id)?;
            let name = rendered_file_name(&filename)?;
            let out_path = dir.join(&name);
            crate::thumbnail::save_png_as_jpeg(&png_bytes, &out_path)?;

            let _g = self.inner();
            let mut data = manifest::load_video_data(&dir);

            // Each render pass writes fresh timestamped files (the webview
            // caches images by path) and saves them one at a time. Drop files
            // from earlier passes, but not this pass's siblings.
            let batch = render_batch(&name);
            let superseded: Vec<String> = data
                .thumbnail_options
                .iter()
                .filter(|n| n.starts_with(RENDERED_PREFIX) && render_batch(n) != batch)
                .cloned()
                .collect();
            for old in &superseded {
                let _ = std::fs::remove_file(dir.join(old));
            }
            data.thumbnail_options.retain(|n| !superseded.contains(n));

            if !data.thumbnail_options.contains(&name) {
                data.thumbnail_options.insert(0, name.clone());
            }
            data.selected_thumbnail = Some(name.clone());
            manifest::save_video_data(&dir, &data)?;
            Ok(path_string(&dir.join(name)))
        }
        .await;
        self.end_op(id);
        result
    }

    /// Everything an upload needs, with the video marked busy.
    pub fn begin_upload(&self, id: &str, thumbnail: Option<String>) -> Result<UploadJob, String> {
        let (paths, entry, data) = self.begin_op(id)?;
        let prepare = || -> Result<UploadJob, String> {
            let dir = paths.video_dir(id)?;
            let video_path = match data.trimmed_file.as_ref().map(|n| dir.join(n)).filter(|p| p.exists()) {
                Some(p) => p,
                None => paths.original(&entry.file_name)?,
            };
            let thumbnail_path = match thumbnail {
                Some(t) => Some(dir.join(file_in_dir(&dir, &t)?)),
                None => None,
            };
            let segments = manifest::load_transcript(&dir)
                .filter(|t| t.file_hash == entry.fingerprint.hash)
                .map(|t| t.transcription.segments);
            Ok(UploadJob {
                video_path,
                thumbnail_path,
                segments,
                file_hash: entry.fingerprint.hash.clone(),
            })
        };
        let job = prepare();
        match &job {
            Ok(_) => {
                self.inner().uploads.insert(id.to_string(), 0);
            }
            Err(_) => self.end_op(id),
        }
        job
    }

    pub fn set_upload_progress(&self, id: &str, percent: u32) {
        if let Some(p) = self.inner().uploads.get_mut(id) {
            *p = percent;
        }
    }

    pub fn finish_upload(&self, id: &str, file_hash: &str, url: Option<&str>) {
        let mut g = self.inner();
        g.busy.remove(id);
        g.uploads.remove(id);
        let Some(url) = url else { return };
        if let Ok(e) = g.entry_mut(id) {
            e.uploads.push(UploadRecord {
                url: url.to_string(),
                uploaded_at: chrono::Utc::now().to_rfc3339(),
                file_hash: file_hash.to_string(),
            });
            e.status = EntryStatus::Done;
            e.error = None;
        }
        g.save_logged();
    }

    /// What would be lost by quitting now, for the quit warning.
    pub fn work_in_progress(&self) -> Option<String> {
        let g = self.inner();
        let mut parts = Vec::new();
        if !g.uploads.is_empty() {
            parts.push(match g.uploads.len() {
                1 => "a YouTube upload".to_string(),
                n => format!("{} YouTube uploads", n),
            });
        }
        if g.running.is_some() || !g.queue.is_empty() {
            parts.push("video processing".to_string());
        }
        if !g.downloads.is_empty() {
            parts.push("a download".to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" and "))
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

    /// Delete .sermon-cut entirely (never the videos) and start fresh.
    pub async fn clear_data(self: &Arc<Self>) -> Result<(), String> {
        let _scan = self.scan_lock.lock().await;
        let paths = {
            let g = self.inner();
            let paths = g.writable()?.clone();
            if g.running.is_some() || !g.queue.is_empty() || !g.busy.is_empty() || !g.downloads.is_empty() {
                return Err("Wait for processing, downloads and uploads to finish first.".into());
            }
            paths
        };
        let lib = self.clone();
        blocking(move || {
            let data = paths.data_dir();
            if std::fs::symlink_metadata(&data).is_ok_and(|m| m.is_dir()) {
                std::fs::remove_dir_all(&data).map_err(|e| format!("Failed to delete {}: {}", data.display(), e))?;
            }
            lib.open_blocking(false);
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
        let info = crate::youtube_dl::get_video_info(url).await?;
        let job_id = super::new_id();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let paths = {
            let mut g = self.inner();
            let paths = g.writable()?.clone();
            g.downloads.push(DownloadItem {
                job_id: job_id.clone(),
                youtube_id: info.id.clone(),
                title: info.title.clone(),
                cancel: Some(cancel_tx),
            });
            paths
        };
        emit_changed(app);

        let work = paths.tmp_dir().join(format!("dl-{}", job_id));
        let result = async {
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

            // Hold the lock across the move and the new entry, so a scan
            // can't see the file before it has its YouTube details.
            let mut g = self.inner();
            g.writable()?;
            let name = move_into_library(&file, &paths.root, &download_file_name(&info.title, &info.id, &ext))?;
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
        for d in g.downloads.iter().filter(|d| d.youtube_id == youtube_id || d.job_id == youtube_id) {
            if let Some(tx) = &d.cancel {
                let _ = tx.send(true);
            }
        }
    }
}

pub struct UploadJob {
    pub video_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub segments: Option<Vec<crate::state::TranscriptSegment>>,
    pub file_hash: String,
}

/// Filename prefix for thumbnails rendered by the template system.
const RENDERED_PREFIX: &str = "thumbnail_rendered_";

/// Name for a rendered thumbnail: a plain file name in the video's folder,
/// always with the rendered prefix (so cleanup can find it) and .jpg (it is
/// re-encoded as JPEG).
fn rendered_file_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if !super::is_valid_file_name(name) {
        return Err(format!("Invalid thumbnail filename: {}", name));
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = stem.strip_prefix(RENDERED_PREFIX).unwrap_or(&stem);
    Ok(format!("{}{}.jpg", RENDERED_PREFIX, stem))
}

/// The render pass a rendered thumbnail belongs to: the timestamp after the
/// last underscore ("thumbnail_rendered_3_1729530000000.jpg" → "1729530000000").
fn render_batch(name: &str) -> &str {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.rsplit_once('_').map_or(stem, |(_, b)| b)
}

/// Resolve a path the frontend sent back to a file directly inside `dir`.
fn file_in_dir(dir: &Path, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| super::is_valid_file_name(n))
        .ok_or_else(|| format!("Invalid file: {}", path))?;
    let parent_ok = p
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(dir.canonicalize().ok())
        .is_some_and(|(a, b)| a == b);
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

        let job = lib.begin_upload(&id, None).unwrap();
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Uploading);
        assert_eq!(v.upload_progress, Some(0));
        assert!(v.busy);
        assert_eq!(lib.work_in_progress().as_deref(), Some("a YouTube upload"));
        // A second upload, a reset or a trash can't start meanwhile.
        assert!(lib.begin_upload(&id, None).is_err());
        assert!(lib.reset(&id).is_err());

        lib.set_upload_progress(&id, 42);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.upload_progress, Some(42));

        lib.finish_upload(&id, &job.file_hash, Some("https://youtube.com/watch?v=x"));
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Done);
        assert_eq!(v.upload_progress, None);
        assert_eq!(v.uploads.len(), 1);
        assert!(lib.work_in_progress().is_none());

        // A failed upload goes back to how it was, with nothing recorded.
        let job = lib.begin_upload(&id, None).unwrap();
        lib.finish_upload(&id, &job.file_hash, None);
        let v = lib.view(vec![]).videos.into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, ViewStatus::Done);
        assert_eq!(v.uploads.len(), 1);
        assert!(!v.busy);
    }

    #[test]
    fn rendered_names_are_safe_and_batched() {
        assert_eq!(rendered_file_name("thumbnail_rendered_1_123.jpg").unwrap(), "thumbnail_rendered_1_123.jpg");
        assert_eq!(rendered_file_name("x_9.png").unwrap(), "thumbnail_rendered_x_9.jpg");
        for bad in ["..", "../x.jpg", "a/b.jpg", ".hidden.jpg", ""] {
            assert!(rendered_file_name(bad).is_err(), "{bad}");
        }
        assert_eq!(render_batch("thumbnail_rendered_3_1729530000000.jpg"), "1729530000000");
        assert_eq!(render_batch("thumbnail_rendered_10_1729530000000.jpg"), render_batch("thumbnail_rendered_1_1729530000000.jpg"));
        assert_ne!(render_batch("thumbnail_rendered_1_1.jpg"), render_batch("thumbnail_rendered_1_2.jpg"));
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
