use super::*;

/// A job's work folder, unique to one run (so nothing left running by a
/// cancelled run can write into the next one's), deleted when the job ends
/// however it ends.
pub(super) struct StagingDir(pub(super) PathBuf);

impl StagingDir {
    pub(super) fn create(paths: &LibraryPaths, id: &str) -> Result<Self, String> {
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

/// Where a commit keeps a video's previous outputs (and video.json) until
/// the new ones are in. Hidden, so no video.json can name it. Its journal,
/// written before anything moves, lists what moves and how far it got.
pub(super) const COMMIT_BACKUP: &str = ".commit-backup";
pub(super) const COMMIT_JOURNAL: &str = ".journal.json";
/// What the backup's copy of video.json is called.
const COMMIT_DATA: &str = "video.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CommitPhase {
    /// Moving files; a crash now means the old outputs go back.
    Swapping,
    /// The new outputs and video.json are in; only the clean-up is left.
    Done,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub(super) struct CommitJournal {
    pub(super) phase: CommitPhase,
    /// Old outputs, moved from the folder into the backup.
    pub(super) old: Vec<String>,
    /// New outputs, moved from staging into the folder.
    pub(super) new: Vec<String>,
}

/// Generated files in a video folder: everything but the transcript and
/// the review data.
pub(super) fn is_output(name: &str) -> bool {
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
pub(super) fn names_in(dir: &Path) -> std::io::Result<Vec<String>> {
    Ok(files_in(dir)?.into_iter().filter_map(|n| n.into_string().ok()).collect())
}

pub(super) fn write_journal(backup: &Path, journal: &CommitJournal) -> Result<(), String> {
    fsutil::write_json_atomic(&backup.join(COMMIT_JOURNAL), journal)
}

/// A commit's backup folder is still there after `recover_commit` tried
/// to put it back (the frontend offers to show it: `isStuckCommit`).
pub const STUCK_COMMIT: &str = "An earlier save of this video didn't finish and couldn't be undone, so nothing was changed. Click Show leftovers, move the \".commit-backup\" folder out of the video's folder, then try again.";

/// Start a commit: the backup folder, its journal, and a copy of the
/// current video.json, before anything moves. The old outputs are those
/// `replaces` picks. A backup left by an earlier commit is put right
/// first; if it can't be, nothing is done.
pub(super) fn begin_commit(dir: &Path, staging: &Path, replaces: &dyn Fn(&str) -> bool) -> Result<CommitJournal, String> {
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
pub(super) fn swap_in_outputs(dir: &Path, staging: &Path, write_data: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    swap_in_outputs_where(dir, staging, &|_| true, write_data)
}

/// `swap_in_outputs`, replacing only the old outputs `replaces` picks; the
/// others stay as they are.
pub(super) fn swap_in_outputs_where(
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
pub(super) fn recover_commit(dir: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{contents, crash_after_where};

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
}
