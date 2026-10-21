//! Matching the files in the folder against library.json.
//!
//! A scan has two halves so the library lock is never held during slow I/O:
//! `observe` lists the folder and does any hashing/probing (no lock), then
//! `reconcile` applies the observations to the manifest (pure, fast).

use super::fsutil::{self, Completeness};
use super::{is_candidate_video, match_key, Entry, EntryStatus, Fingerprint};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A file must keep the same size and mtime this long to count as copied.
pub const STABLE_FOR: Duration = Duration::from_secs(5);
/// Files last modified longer ago than this that still can't be read are
/// reported as unreadable straight away rather than "copying".
const SETTLED_AGE_MS: i64 = 60_000;
/// After this long without becoming readable, a file is "unreadable"
/// rather than "copying".
const UNREADABLE_AFTER: Duration = Duration::from_secs(60);
const REPROBE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObsState {
    Copying,
    Unreadable,
    Ready,
}

#[derive(Debug, Clone)]
pub struct FileObs {
    pub name: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub state: ObsState,
    /// Set when the state is Ready.
    pub hash: Option<String>,
    pub duration: Option<f64>,
}

struct CacheItem {
    size: u64,
    mtime_ms: i64,
    first_seen: Instant,
    complete: Option<Completeness>,
    probe: Option<(Instant, Option<f64>)>,
    hash: Option<String>,
}

/// Remembers per-file work between scans so an unchanged folder costs one
/// readdir plus a stat per file: no hashing, no ffprobe.
#[derive(Default)]
pub struct ScanCache {
    items: HashMap<String, CacheItem>,
}

/// What the scan needs to know about an existing entry.
#[derive(Debug, Clone)]
pub struct KnownFile {
    pub key: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub hash: String,
    pub duration: Option<f64>,
}

pub fn known_files(entries: &[Entry]) -> Vec<KnownFile> {
    entries
        .iter()
        .map(|e| KnownFile {
            key: match_key(&e.file_name),
            size: e.fingerprint.size,
            mtime_ms: e.fingerprint.mtime_ms,
            hash: e.fingerprint.hash.clone(),
            duration: e.duration,
        })
        .collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// List the folder and work out each video's state. `probe` returns the
/// duration if ffprobe can read the file.
pub fn observe(
    root: &Path,
    known: &[KnownFile],
    cache: &mut ScanCache,
    probe: &dyn Fn(&Path) -> Option<f64>,
    stable_for: Duration,
) -> std::io::Result<Vec<FileObs>> {
    let now = Instant::now();
    let wall_now = now_ms();
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for dirent in std::fs::read_dir(root)? {
        let Ok(dirent) = dirent else { continue };
        let Ok(name) = dirent.file_name().into_string() else {
            continue; // not UTF-8; can't be stored or shown safely
        };
        if !is_candidate_video(&name) {
            continue;
        }
        // symlink_metadata: symlinks are never listed, processed or trashed.
        let Ok(meta) = std::fs::symlink_metadata(dirent.path()) else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let size = meta.len();
        let mtime_ms = fsutil::mtime_ms(&meta);
        seen.insert(name.clone());

        let key = match_key(&name);
        if let Some(k) = known
            .iter()
            .find(|k| k.key == key && k.size == size && k.mtime_ms == mtime_ms)
        {
            out.push(FileObs {
                name,
                size,
                mtime_ms,
                state: ObsState::Ready,
                hash: Some(k.hash.clone()),
                duration: k.duration,
            });
            continue;
        }

        let item = cache.items.entry(name.clone()).or_insert_with(|| CacheItem {
            size,
            mtime_ms,
            first_seen: now,
            complete: None,
            probe: None,
            hash: None,
        });
        if item.size != size || item.mtime_ms != mtime_ms {
            *item = CacheItem {
                size,
                mtime_ms,
                first_seen: now,
                complete: None,
                probe: None,
                hash: None,
            };
        }

        // Even an old mtime doesn't prove a copy is finished (some copiers set
        // it first), so a new file always has to hold still across scans.
        let unchanged_for = now.duration_since(item.first_seen);
        let stable = unchanged_for >= stable_for;
        let long_unchanged = wall_now - mtime_ms >= SETTLED_AGE_MS || unchanged_for >= UNREADABLE_AFTER;
        let mut obs = FileObs {
            name: name.clone(),
            size,
            mtime_ms,
            state: ObsState::Copying,
            hash: None,
            duration: None,
        };
        if !stable || size == 0 {
            out.push(obs);
            continue;
        }

        let path = dirent.path();
        let lower = name.to_lowercase();
        if lower.ends_with(".mp4") || lower.ends_with(".mov") || lower.ends_with(".m4v") {
            let complete = *item.complete.get_or_insert_with(|| fsutil::mp4_completeness(&path, size));
            if complete == Completeness::Truncated {
                obs.state = if long_unchanged { ObsState::Unreadable } else { ObsState::Copying };
                out.push(obs);
                continue;
            }
        }

        let needs_probe = match item.probe {
            None => true,
            Some((at, None)) => now.duration_since(at) >= REPROBE_AFTER,
            Some((_, Some(_))) => false,
        };
        if needs_probe {
            item.probe = Some((now, probe(&path)));
        }
        let Some((_, Some(duration))) = item.probe else {
            obs.state = if long_unchanged { ObsState::Unreadable } else { ObsState::Copying };
            out.push(obs);
            continue;
        };

        if item.hash.is_none() {
            match fsutil::content_hash(&path, size) {
                Ok(h) => item.hash = Some(h),
                Err(_) => {
                    out.push(obs);
                    continue;
                }
            }
        }
        obs.state = ObsState::Ready;
        obs.hash = item.hash.clone();
        obs.duration = Some(duration);
        out.push(obs);
    }

    cache.items.retain(|name, _| seen.contains(name));
    Ok(out)
}

#[derive(Debug, Default)]
pub struct Reconciled {
    pub changed: bool,
    /// Entries whose file is in the folder.
    pub present: HashSet<String>,
    /// Entries whose file is present but changing (being re-copied).
    pub copying_ids: HashSet<String>,
    /// Entries whose file changed and now can't be read.
    pub unreadable_ids: HashSet<String>,
    /// Entries whose file was replaced; their generated files are stale.
    pub replaced: Vec<String>,
    /// New files not yet in the library.
    pub copying: Vec<FileObs>,
    pub unreadable: Vec<FileObs>,
}

/// Apply a scan to the entries. `busy` entries (queued/processing/uploading)
/// are never reset here; the job re-checks the file itself.
pub fn reconcile(entries: &mut Vec<Entry>, obs: &[FileObs], busy: &HashSet<String>) -> Reconciled {
    let mut r = Reconciled::default();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        by_key.entry(match_key(&e.file_name)).or_insert(i);
    }
    let mut claimed: HashSet<usize> = HashSet::new();
    let mut unmatched: Vec<&FileObs> = Vec::new();

    // Pass 1: same name.
    for o in obs {
        let Some(&i) = by_key.get(&match_key(&o.name)) else {
            unmatched.push(o);
            continue;
        };
        if !claimed.insert(i) {
            unmatched.push(o);
            continue;
        }
        let e = &mut entries[i];
        r.present.insert(e.id.clone());
        if e.file_name != o.name {
            e.file_name = o.name.clone();
            r.changed = true;
        }
        if e.fingerprint.size == o.size && e.fingerprint.mtime_ms == o.mtime_ms {
            continue;
        }
        let (ObsState::Ready, Some(hash)) = (o.state, o.hash.as_ref()) else {
            if o.state == ObsState::Unreadable {
                r.unreadable_ids.insert(e.id.clone());
            } else {
                r.copying_ids.insert(e.id.clone());
            }
            continue;
        };
        if busy.contains(&e.id) {
            continue;
        }
        if *hash == e.fingerprint.hash {
            // Touched but identical.
            e.fingerprint.size = o.size;
            e.fingerprint.mtime_ms = o.mtime_ms;
            r.changed = true;
            continue;
        }
        // Replaced with different content: start over, keep upload history.
        e.fingerprint = Fingerprint {
            size: o.size,
            mtime_ms: o.mtime_ms,
            hash: hash.clone(),
        };
        e.status = EntryStatus::New;
        e.error = None;
        e.duration = o.duration;
        r.replaced.push(e.id.clone());
        r.changed = true;
    }

    // Pass 2: renames (same content as an entry whose file is gone), then new files.
    for o in unmatched {
        match o.state {
            ObsState::Copying => {
                r.copying.push(o.clone());
                continue;
            }
            ObsState::Unreadable => {
                r.unreadable.push(o.clone());
                continue;
            }
            ObsState::Ready => {}
        }
        let Some(hash) = o.hash.as_ref() else { continue };
        let renamed = (0..entries.len()).find(|i| {
            !claimed.contains(i)
                && entries[*i].fingerprint.hash == *hash
                && entries[*i].fingerprint.size == o.size
        });
        if let Some(i) = renamed {
            claimed.insert(i);
            let e = &mut entries[i];
            e.file_name = o.name.clone();
            e.fingerprint.mtime_ms = o.mtime_ms;
            r.present.insert(e.id.clone());
            r.changed = true;
            continue;
        }
        let entry = Entry::new(
            o.name.clone(),
            Fingerprint {
                size: o.size,
                mtime_ms: o.mtime_ms,
                hash: hash.clone(),
            },
            o.duration,
        );
        claimed.insert(entries.len());
        r.present.insert(entry.id.clone());
        entries.push(entry);
        r.changed = true;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn ready(name: &str, size: u64, mtime: i64, hash: &str) -> FileObs {
        FileObs {
            name: name.into(),
            size,
            mtime_ms: mtime,
            state: ObsState::Ready,
            hash: Some(hash.into()),
            duration: Some(60.0),
        }
    }

    fn entry(name: &str, size: u64, mtime: i64, hash: &str) -> Entry {
        Entry::new(
            name.into(),
            Fingerprint { size, mtime_ms: mtime, hash: hash.into() },
            Some(60.0),
        )
    }

    #[test]
    fn new_file_becomes_new_entry() {
        let mut entries = vec![];
        let r = reconcile(&mut entries, &[ready("a.mp4", 10, 1, "h1")], &HashSet::new());
        assert!(r.changed);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, EntryStatus::New);
        assert!(r.present.contains(&entries[0].id));
    }

    #[test]
    fn unchanged_file_changes_nothing() {
        let mut entries = vec![entry("a.mp4", 10, 1, "h1")];
        let r = reconcile(&mut entries, &[ready("a.mp4", 10, 1, "h1")], &HashSet::new());
        assert!(!r.changed);
        assert!(r.present.contains(&entries[0].id));
    }

    #[test]
    fn missing_file_is_not_present_and_kept() {
        let mut entries = vec![entry("a.mp4", 10, 1, "h1")];
        let r = reconcile(&mut entries, &[], &HashSet::new());
        assert!(!r.changed);
        assert_eq!(entries.len(), 1);
        assert!(r.present.is_empty());
    }

    #[test]
    fn rename_keeps_state() {
        let mut e = entry("a.mp4", 10, 1, "h1");
        e.status = EntryStatus::Done;
        let id = e.id.clone();
        let mut entries = vec![e];
        let r = reconcile(&mut entries, &[ready("Renamed.mp4", 10, 1, "h1")], &HashSet::new());
        assert!(r.changed);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].file_name, "Renamed.mp4");
        assert_eq!(entries[0].status, EntryStatus::Done);
    }

    #[test]
    fn duplicate_copy_is_a_new_entry() {
        let mut entries = vec![entry("a.mp4", 10, 1, "h1")];
        reconcile(
            &mut entries,
            &[ready("a.mp4", 10, 1, "h1"), ready("a copy.mp4", 10, 2, "h1")],
            &HashSet::new(),
        );
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn replaced_file_resets_but_keeps_upload_history() {
        let mut e = entry("a.mp4", 10, 1, "h1");
        e.status = EntryStatus::Done;
        e.uploads.push(crate::library::UploadRecord {
            url: "https://youtube.com/watch?v=x".into(),
            uploaded_at: "t".into(),
            file_hash: "h1".into(),
        });
        let mut entries = vec![e];
        let r = reconcile(&mut entries, &[ready("a.mp4", 20, 2, "h2")], &HashSet::new());
        assert_eq!(r.replaced, vec![entries[0].id.clone()]);
        assert_eq!(entries[0].status, EntryStatus::New);
        assert_eq!(entries[0].uploads.len(), 1);
        assert_eq!(entries[0].fingerprint.hash, "h2");
    }

    #[test]
    fn touched_file_with_same_content_keeps_state() {
        let mut e = entry("a.mp4", 10, 1, "h1");
        e.status = EntryStatus::ReadyForReview;
        let mut entries = vec![e];
        let r = reconcile(&mut entries, &[ready("a.mp4", 10, 5, "h1")], &HashSet::new());
        assert!(r.replaced.is_empty());
        assert_eq!(entries[0].status, EntryStatus::ReadyForReview);
        assert_eq!(entries[0].fingerprint.mtime_ms, 5);
    }

    #[test]
    fn busy_entries_are_not_reset() {
        let e = entry("a.mp4", 10, 1, "h1");
        let busy: HashSet<String> = [e.id.clone()].into();
        let mut entries = vec![e];
        let r = reconcile(&mut entries, &[ready("a.mp4", 20, 2, "h2")], &busy);
        assert!(r.replaced.is_empty());
        assert_eq!(entries[0].fingerprint.hash, "h1");
    }

    #[test]
    fn copying_files_are_not_added() {
        let mut entries = vec![];
        let obs = FileObs {
            name: "big.mov".into(),
            size: 5,
            mtime_ms: 1,
            state: ObsState::Copying,
            hash: None,
            duration: None,
        };
        let r = reconcile(&mut entries, &[obs], &HashSet::new());
        assert!(entries.is_empty());
        assert_eq!(r.copying.len(), 1);
    }

    #[test]
    fn nfd_name_matches_nfc_entry_and_keeps_disk_spelling() {
        let mut entries = vec![entry("Caf\u{e9}.mp4", 10, 1, "h1")];
        let r = reconcile(&mut entries, &[ready("Cafe\u{301}.mp4", 10, 1, "h1")], &HashSet::new());
        assert_eq!(entries.len(), 1);
        assert!(r.present.contains(&entries[0].id));
        assert_eq!(entries[0].file_name, "Cafe\u{301}.mp4");
    }

    fn write_settled(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(old).unwrap();
    }

    #[test]
    fn observe_filters_and_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_settled(&root.join("Service.MKV"), b"movie");
        write_settled(&root.join("._Service.MKV"), b"appledouble");
        write_settled(&root.join("notes.txt"), b"x");
        write_settled(&root.join("partial.mp4.part"), b"x");
        std::fs::create_dir(root.join("folder.mp4")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("Service.MKV"), root.join("link.mp4")).unwrap();

        let probe = |_: &Path| Some(42.0);
        let obs = observe(root, &[], &mut ScanCache::default(), &probe, Duration::ZERO).unwrap();
        assert_eq!(obs.len(), 1, "{obs:?}");
        assert_eq!(obs[0].name, "Service.MKV");
        assert_eq!(obs[0].state, ObsState::Ready);
        assert_eq!(obs[0].duration, Some(42.0));
        assert!(obs[0].hash.is_some());
    }

    #[test]
    fn fresh_growing_file_is_copying_until_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("new.mkv");
        std::fs::write(&path, b"growing").unwrap();
        let probe = |_: &Path| Some(1.0);
        let mut cache = ScanCache::default();
        let obs = observe(tmp.path(), &[], &mut cache, &probe, STABLE_FOR).unwrap();
        assert_eq!(obs[0].state, ObsState::Copying);
        // The file grows: still copying.
        std::fs::write(&path, b"growing more").unwrap();
        let obs = observe(tmp.path(), &[], &mut cache, &probe, STABLE_FOR).unwrap();
        assert_eq!(obs[0].state, ObsState::Copying);
    }

    #[test]
    fn truncated_mp4_is_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        // ftyp box, then an mdat that claims more bytes than exist.
        let mut bytes = 16u32.to_be_bytes().to_vec();
        bytes.extend(b"ftypisom\0\0\0\0");
        bytes.extend(10_000u32.to_be_bytes());
        bytes.extend(b"mdat");
        bytes.extend([0u8; 100]);
        write_settled(&tmp.path().join("cut.mp4"), &bytes);
        // ffprobe would report a duration for a faststart file; the box walk still catches it.
        let probe = |_: &Path| Some(1.0);
        let obs = observe(tmp.path(), &[], &mut ScanCache::default(), &probe, Duration::ZERO).unwrap();
        assert_eq!(obs[0].state, ObsState::Unreadable);
    }

    #[test]
    fn unchanged_known_files_are_not_reread() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.mp4");
        write_settled(&path, b"data");
        let meta = std::fs::metadata(&path).unwrap();
        let known = vec![KnownFile {
            key: "a.mp4".into(),
            size: 4,
            mtime_ms: fsutil::mtime_ms(&meta),
            hash: "cached".into(),
            duration: Some(5.0),
        }];
        let calls = Cell::new(0);
        let probe = |_: &Path| {
            calls.set(calls.get() + 1);
            Some(1.0)
        };
        let obs = observe(tmp.path(), &known, &mut ScanCache::default(), &probe, Duration::ZERO).unwrap();
        assert_eq!(calls.get(), 0);
        assert_eq!(obs[0].hash.as_deref(), Some("cached"));
    }

    #[test]
    fn hashes_and_probes_are_cached_between_scans() {
        let tmp = tempfile::tempdir().unwrap();
        write_settled(&tmp.path().join("a.webm"), b"data");
        let calls = Cell::new(0);
        let probe = |_: &Path| {
            calls.set(calls.get() + 1);
            Some(1.0)
        };
        let mut cache = ScanCache::default();
        observe(tmp.path(), &[], &mut cache, &probe, Duration::ZERO).unwrap();
        observe(tmp.path(), &[], &mut cache, &probe, Duration::ZERO).unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn unreadable_after_failing_probe() {
        let tmp = tempfile::tempdir().unwrap();
        write_settled(&tmp.path().join("broken.avi"), b"junk");
        let probe = |_: &Path| None;
        let obs = observe(tmp.path(), &[], &mut ScanCache::default(), &probe, Duration::ZERO).unwrap();
        assert_eq!(obs[0].state, ObsState::Unreadable);
    }

    #[test]
    fn missing_folder_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("unplugged");
        let probe = |_: &Path| Some(1.0);
        assert!(observe(&gone, &[], &mut ScanCache::default(), &probe, Duration::ZERO).is_err());
    }
}
