//! The open library: state, scanning, the processing queue and actions.
//!
//! Locking: `inner` is a std mutex held only for short, non-async work.
//! Slow I/O (hashing, ffprobe, ffmpeg, uploads) happens outside it.

use super::fsutil::{self, TMP_MARKER};
use super::lock::{self, Acquire};
use super::manifest::{self, Loaded, Manifest, Orphan, Render, Review, TranscriptFile, VideoData};
use super::scan::{self, FileObs, ObsState, Reconciled, ScanCache};
use super::{is_valid_file_name, is_valid_id, new_id, Entry, EntryStatus, Fingerprint, LibraryPaths, UploadRecord, MAX_RESTARTS, MAX_TRANSCRIBE_SECS, VIDEO_EXTENSIONS};
use crate::state::{AppState, ProcessingStage, TranscriptionResult};
use crate::upload::{SessionStore, UploadOutcome, UploadSession, UploadWarning, WarningKind};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

#[path = "commit.rs"]
mod commit;
pub use commit::*;
#[path = "open.rs"]
mod open;
#[path = "jobs.rs"]
mod jobs;
#[path = "thumbs.rs"]
mod thumbs;
use thumbs::*;
#[path = "uploads.rs"]
mod uploads;
pub use uploads::*;
#[path = "downloads.rs"]
mod downloads;
#[path = "view.rs"]
mod view;
pub use view::*;

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
    /// Busy entries whose folder is being deleted (a replaced file, a
    /// reset, forget or trash): a review saved now would bring it back.
    removing: HashSet<String>,
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
    /// Files that baseline scan saw still settling (copying, or not yet
    /// readable): they were there too, so when a later scan adds them they
    /// count as already there as well.
    baseline_settling: HashSet<String>,
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

fn path_string(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
    crate::notify::refresh_badge(app);
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
        let known: Option<HashSet<String>> = (self.baseline_scan || !self.baseline_settling.is_empty())
            .then(|| self.manifest.entries.iter().map(|e| e.id.clone()).collect());
        let reconciled =
            scan::reconcile_adopting(&mut self.manifest.entries, obs, &busy, last_present.as_ref(), &mut self.orphans);
        if let Some(known) = known {
            let baseline = self.baseline_scan;
            for e in self.manifest.entries.iter_mut().filter(|e| !known.contains(&e.id)) {
                if baseline || self.baseline_settling.contains(&e.file_name) {
                    e.first_seen = 0;
                }
            }
        }
        let mut stale = Vec::new();
        if self.mode == Mode::Writable {
            // Only a saved scan settles the backlog: a read-only one is
            // forgotten when the library is reopened for writing. Files it
            // couldn't add yet (a scan only adds a file it has watched hold
            // still) stay backlog until they're added.
            let settling: HashSet<String> =
                reconciled.copying.iter().chain(&reconciled.unreadable).map(|o| o.name.clone()).collect();
            if self.baseline_scan {
                self.baseline_settling = settling;
            } else {
                self.baseline_settling.retain(|n| settling.contains(n));
            }
            self.baseline_scan = false;
            stale = reconciled.replaced.clone();
            self.busy.extend(stale.iter().cloned());
            self.removing.extend(stale.iter().cloned());
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
                removing: HashSet::new(),
                rendering: 0,
                uploads: std::collections::HashMap::new(),
                downloads: Vec::new(),
                session: super::new_id(),
                notice: None,
                orphans: std::collections::HashMap::new(),
                updating: false,
                baseline_scan: false,
                baseline_settling: HashSet::new(),
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
            let (lib, ids) = (self.clone(), stale.clone());
            let _ = blocking(move || {
                for id in &ids {
                    if let Err(e) = lib.remove_video_files(&paths, id) {
                        eprintln!("[library] {}", e);
                    }
                }
            })
            .await;
        }
        let mut g = self.inner();
        for id in &stale {
            g.busy.remove(id);
            g.removing.remove(id);
        }
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

    /// Mark a video busy (see `mark_busy`) to delete its folder: until the
    /// guard drops, a review can't be saved into it (`save_review`).
    fn mark_removing(&self, id: &str, check: impl FnOnce(&Inner) -> Result<(), String>) -> Result<(BusyGuard<'_>, LibraryPaths), String> {
        let marked = self.mark_busy(id, check)?;
        self.inner().removing.insert(id.to_string());
        Ok(marked)
    }

    /// Delete a video's folder under its lock. Blocking.
    fn remove_video_files(&self, paths: &LibraryPaths, id: &str) -> Result<(), String> {
        let lock = self.video_lock(id);
        let _v = hold(&lock);
        manifest::remove_video_dir(paths, id)
    }

    /// Process a video again from a new transcription: its saved transcript
    /// goes (under the video's lock, never `inner`) once it would be
    /// queued, then it is queued. The results so far stay until the new
    /// ones are committed, and the review typed so far (title, description, speaker) is kept (`commit_job`).
    pub async fn reprocess(self: &Arc<Self>, id: &str) -> Result<(), String> {
        // Everything that could refuse the queueing first, so a refused
        // reprocess loses nothing.
        let (root, expected) = self.check_enqueue(id).await?;
        let dir = self.with_entry(id, |_, paths| paths.video_dir(id))?;
        let lock = self.video_lock(id);
        blocking(move || {
            let _v = hold(&lock);
            match std::fs::remove_file(dir.join("transcript.json")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove the old transcript: {}", e)),
                _ => Ok(()),
            }
        })
        .await??;
        self.add_to_queue(id, &root, &expected)
    }

    /// Back to New: delete what processing made. Keeps upload history.
    /// Blocking (deletes files): call it from a blocking thread.
    pub fn reset(&self, id: &str) -> Result<(), String> {
        let (_busy, paths) = self.mark_removing(id, |_| Ok(()))?;
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
        let (_busy, paths) = self.mark_removing(id, |g| {
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
        let (_busy, paths) = self.mark_removing(id, |_| Ok(()))?;
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
            // Its folder is being deleted (file replaced, reset, forget,
            // trash), or was: the review on screen is of the old results.
            if g.removing.contains(id) || g.entry(id)?.status == EntryStatus::New {
                return Err("This video's results were cleared (its file was replaced or reset), so these edits weren't saved.".into());
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
        g.removing.remove(&self.id);
        g.uploads.remove(&self.id);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    pub(super) fn outcome(video_id: &str, warnings: Vec<UploadWarning>) -> UploadOutcome {
        UploadOutcome { url: crate::upload::watch_url(video_id), video_id: video_id.into(), warnings, earlier: None, before_cancel: false }
    }

    pub(super) fn public() -> UploadOptions {
        UploadOptions { privacy: "public".into(), playlist_id: None }
    }

    /// A writable library with one processed video that has two generated
    /// thumbnail options.
    pub(super) fn processed_library() -> (tempfile::TempDir, Arc<Library>, String, PathBuf) {
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

    pub(super) fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(4, 4).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    pub(super) const TV: u32 = 4;

    /// The key a render of `source` must carry once the review's title is
    /// `title` (and the speaker "P").
    pub(super) fn test_key(dir: &Path, source: &str, title: &str) -> String {
        let (_, version) = render_from(dir, source);
        manifest::render_key(TV, title, "P", false, source, &version, "")
    }

    /// Save the review with `title`, then a render of `source` made from it.
    pub(super) async fn rendered(lib: &Arc<Library>, id: &str, source: &str, title: &str, bytes: Vec<u8>) -> Result<String, String> {
        // Refused while the video is processing, as the render then is.
        let _ = lib.save_review(id, Review { title: Some(title.into()), preacher: Some("P".into()), ..Default::default() }, None);
        let dir = lib.inner().paths.clone().unwrap().video_dir(id).unwrap();
        lib.save_rendered_thumbnail(id, source, &test_key(&dir, source, title), TV, "", bytes).await
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

    /// What a scan sees for a file in the library folder.
    pub(super) fn seen(root: &Path, name: &str) -> FileObs {
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

    /// Found by the smoke test: the first scan after a regain (or switch)
    /// sees files it hasn't watched hold still as copying and adds them on a
    /// later scan. They're the backlog all the same.
    #[tokio::test]
    async fn backlog_files_still_settling_at_the_first_scan_stay_backlog() {
        let (_t, lib, _id, _dir) = processed_library();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("old 1.mp4"), b"one").unwrap();
        std::fs::write(other.path().join("old 2.mp4"), b"two").unwrap();
        let since = chrono::Utc::now().timestamp_millis();
        lib.switch(other.path().to_string_lossy().to_string()).await.unwrap();
        let root = lib.root().unwrap();
        let settling = |name: &str| FileObs { state: ObsState::Copying, hash: None, duration: None, ..seen(&root, name) };
        lib.inner().apply_scan(&[settling("old 1.mp4"), seen(&root, "old 2.mp4")]);
        assert_eq!(lib.inner().manifest.entries.len(), 1);

        // It settles; a file that starts arriving now is new.
        std::fs::write(root.join("new.mp4"), b"three").unwrap();
        lib.inner().apply_scan(&[seen(&root, "old 1.mp4"), seen(&root, "old 2.mp4"), settling("new.mp4")]);
        assert_eq!(lib.inner().manifest.entries.len(), 2);
        assert!(lib.auto_process(since).await.is_empty());
        lib.inner().apply_scan(&[seen(&root, "old 1.mp4"), seen(&root, "old 2.mp4"), seen(&root, "new.mp4")]);
        let new = lib.inner().manifest.entries.iter().find(|e| e.file_name == "new.mp4").unwrap().id.clone();
        assert_eq!(lib.auto_process(since).await, vec![new]);
        assert!(lib.inner().baseline_settling.is_empty());
    }

    pub(super) fn saved_queue(lib: &Library) -> Vec<String> {
        let paths = lib.inner().paths.clone().unwrap();
        match manifest::load(&paths).unwrap() {
            manifest::Loaded::Ok(m) => m.queue,
            _ => panic!("library.json unreadable"),
        }
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

    /// A replaced file's old folder goes under the video's lock, and a
    /// review autosaved from the old results (during or after) can't bring
    /// it back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replaced_files_folder_goes_under_its_lock_and_stays_gone() {
        let (_t, lib, id, dir) = processed_library();
        let review = || Review { title: Some("Old title".into()), ..Default::default() };
        lib.save_review(&id, review(), None).unwrap();
        let replaced = FileObs {
            name: "a.mp4".into(),
            size: 99,
            mtime_ms: i64::MAX / 2,
            state: ObsState::Ready,
            hash: Some("re-exported".into()),
            duration: Some(60.0),
        };
        let lock = lib.video_lock(&id);
        let held = hold(&lock);
        let scan = tokio::spawn({
            let lib = lib.clone();
            async move { lib.finish_scan(&[replaced]).await }
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(dir.exists(), "not deleted while the video's lock is held");
        assert!(lib.inner().removing.contains(&id));
        drop(held);
        // Refused while the folder is going (it would recreate it).
        let during = lib.save_review(&id, review(), None);
        scan.await.unwrap();
        assert!(during.is_err() || !dir.exists());
        assert!(!dir.exists());
        assert!(!lib.inner().removing.contains(&id));
        // And after: the entry is New again, with nothing to review.
        assert!(lib.save_review(&id, review(), None).is_err());
        assert!(!dir.exists(), "an autosave doesn't bring the old review back");
    }

    /// A reprocess that queueing would refuse keeps the paid transcript.
    #[tokio::test]
    async fn a_refused_reprocess_keeps_the_transcript() {
        let (_t, lib, id, dir) = processed_library();
        std::fs::write(dir.join("transcript.json"), b"{}").unwrap();
        lib.inner().entry_mut(&id).unwrap().duration = Some(MAX_TRANSCRIBE_SECS + 60.0);
        assert!(lib.reprocess(&id).await.is_err());
        assert!(dir.join("transcript.json").exists());

        lib.inner().entry_mut(&id).unwrap().duration = Some(60.0);
        lib.inner().last.copying_ids.insert(id.clone());
        assert_eq!(lib.reprocess(&id).await.unwrap_err(), FILE_CHANGED);
        assert!(dir.join("transcript.json").exists());

        lib.inner().last.copying_ids.clear();
        lib.reprocess(&id).await.unwrap();
        assert!(!dir.join("transcript.json").exists());
        assert_eq!(lib.inner().queue.front(), Some(&id));
    }

    /// Reset, forget and trash refuse a review while their folder goes.
    #[test]
    fn a_review_is_refused_while_the_folder_is_being_removed() {
        let (_t, lib, id, dir) = processed_library();
        let (guard, _paths) = lib.mark_removing(&id, |_| Ok(())).unwrap();
        assert!(lib.save_review(&id, Review::default(), None).is_err());
        drop(guard);
        assert!(!lib.inner().removing.contains(&id));
        lib.save_review(&id, Review::default(), None).unwrap();
        assert!(dir.exists());
    }

    pub(super) fn contents(dir: &Path) -> Vec<(String, String)> {
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

    pub(super) fn crash_after_where(dir: &Path, staging: &Path, steps: usize, replaces: &dyn Fn(&str) -> bool) {
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
}
