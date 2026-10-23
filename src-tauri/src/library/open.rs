use super::*;

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
    /// (Re)open the configured folder. Blocking: call from a blocking thread.
    pub(super) fn open_blocking(&self, force_lock: bool) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{processed_library, saved_queue};

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
}
