//! Moving pre-0.4 videos (state.json) into the library.
//!
//! Every step can be repeated: entries remember the old id (`legacy_id`),
//! moves treat "source gone, destination present" as done, and the caller
//! only drops the old videos from state.json once this returns Ok.

use super::fsutil::{self, move_file};
use super::manifest::{self, Manifest, TranscriptFile};
use super::{match_key, Entry, EntryStatus, LibraryPaths};
use crate::state::{VideoInfo, VideoStatus};
use std::path::{Path, PathBuf};

/// Tries per old video (one per launch) before it is given up on.
pub const MAX_ATTEMPTS: u64 = 3;
/// Added to an old video in state.json to count failed tries.
const ATTEMPTS_KEY: &str = "_migration_attempts";

#[derive(Debug, Default)]
pub struct Outcome {
    /// Keys of old videos that are dealt with (moved, or nothing to move)
    /// and can be dropped from state.json.
    pub done: Vec<String>,
    /// Old videos that failed, with why; they stay for the next launch.
    pub failed: Vec<Failed>,
    /// Titles of videos whose file isn't in the library folder.
    pub outside: Vec<String>,
    /// Titles of videos whose file no longer exists.
    pub missing: Vec<String>,
    /// Old WAVs that can be deleted once the migration is committed.
    pub leftover_audio: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Failed {
    pub key: String,
    pub title: String,
    pub error: String,
}

fn is_inside(paths: &LibraryPaths, file: &Path) -> bool {
    let parent = file.parent().and_then(|p| p.canonicalize().ok());
    parent.as_deref() == Some(paths.root.as_path())
}

fn move_into(src: &str, dir: &Path) -> Result<Option<String>, String> {
    let src = Path::new(src);
    let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return Ok(None);
    };
    if !super::is_valid_file_name(&name) {
        return Ok(None);
    }
    let dst = dir.join(&name);
    if !src.exists() && !dst.exists() {
        return Ok(None); // lost already; nothing to carry over
    }
    move_file(src, &dst).map_err(|e| format!("Failed to move {}: {}", src.display(), e))?;
    Ok(Some(name))
}

/// Move a generated thumbnail and its _raw / _debug siblings.
fn move_thumbnail(src: &str, dir: &Path) -> Result<Option<String>, String> {
    if let Some(base) = src.strip_suffix(".jpg") {
        for sibling in ["_raw.jpg", "_debug.jpg"] {
            let path = format!("{}{}", base, sibling);
            if Path::new(&path).exists() {
                move_into(&path, dir)?;
            }
        }
    }
    move_into(src, dir)
}

/// Move every old video it can. One that fails is recorded in the
/// outcome and the rest carry on; entries made so far stay in `manifest`
/// (their files were already moved) and a later run picks up from there.
pub fn migrate(paths: &LibraryPaths, manifest: &mut Manifest, legacy: &serde_json::Map<String, serde_json::Value>) -> Outcome {
    let mut outcome = Outcome::default();
    for (key, value) in legacy {
        let video: VideoInfo = match serde_json::from_value(value.clone()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[migrate] Skipping unreadable old video {}: {}", key, e);
                outcome.done.push(key.clone());
                continue;
            }
        };
        match migrate_one(paths, manifest, &video, &mut outcome) {
            Ok(()) => {
                outcome.done.push(key.clone());
                if let Some(audio) = &video.audio_path {
                    outcome.leftover_audio.push(PathBuf::from(audio));
                }
            }
            Err(error) => {
                eprintln!("[migrate] {} failed: {}", key, error);
                outcome.failed.push(Failed { key: key.clone(), title: video.title.clone(), error });
            }
        }
    }
    outcome
}

fn migrate_one(paths: &LibraryPaths, manifest: &mut Manifest, video: &VideoInfo, outcome: &mut Outcome) -> Result<(), String> {
    let Some(file_path) = video.file_path.as_deref().map(PathBuf::from) else {
        return Ok(()); // never finished downloading
    };
    if !file_path.exists() {
        outcome.missing.push(video.title.clone());
        return Ok(());
    }
    let is_local = video.id.starts_with("local_");
    let file_name = file_path.file_name().map(|n| n.to_string_lossy().to_string());
    let Some(file_name) = file_name.filter(|n| is_inside(paths, &file_path) && super::is_candidate_video(n)) else {
        outcome.outside.push(video.title.clone());
        return Ok(());
    };

    // Find the entry from an earlier (partial) run, or one a scan made.
    let existing = manifest
        .entries
        .iter()
        .position(|e| e.legacy_id.as_deref() == Some(video.id.as_str()))
        .or_else(|| {
            manifest
                .entries
                .iter()
                .position(|e| e.legacy_id.is_none() && match_key(&e.file_name) == match_key(&file_name))
        });
    let index = match existing {
        Some(i) => i,
        None => {
            let fingerprint = fsutil::fingerprint(&file_path)?;
            manifest.entries.push(Entry::new(file_name.clone(), fingerprint, video.duration));
            manifest.entries.len() - 1
        }
    };
    let entry = &mut manifest.entries[index];
    entry.legacy_id = Some(video.id.clone());
    if !is_local {
        entry.title.get_or_insert(video.title.clone());
        entry.youtube_id.get_or_insert(video.id.clone());
    }
    if video.url.starts_with("https://") || video.url.starts_with("http://") {
        entry.source_url.get_or_insert(video.url.clone());
    }
    if entry.channel.is_none() {
        entry.channel = video.channel.clone();
    }
    if entry.duration.is_none() {
        entry.duration = video.duration;
    }
    let id = entry.id.clone();
    let hash = entry.fingerprint.hash.clone();

    let dir = paths.ensure_video_dir(&id)?;
    if let Some(t) = &video.transcription {
        if manifest::load_transcript(&dir).is_none() {
            manifest::save_transcript(
                &dir,
                &TranscriptFile {
                    file_hash: hash,
                    transcription: t.clone(),
                },
            )?;
        }
    }

    let mut data = manifest::load_video_data(&dir);
    if let Some(trimmed) = &video.trimmed_path {
        if let Some(name) = move_into(trimmed, &dir)? {
            data.trimmed_file = Some(name);
        }
    }
    for option in video.thumbnail_options.iter().flatten() {
        if let Some(name) = move_thumbnail(option, &dir)? {
            if !data.thumbnail_options.contains(&name) {
                data.thumbnail_options.push(name);
            }
        }
    }
    if let Some(selected) = &video.thumbnail_path {
        if let Some(name) = move_thumbnail(selected, &dir)? {
            if !data.thumbnail_options.contains(&name) {
                data.thumbnail_options.insert(0, name.clone());
            }
            data.selected_thumbnail = Some(name);
        }
    }
    if data.trim_start.is_none() {
        data.trim_start = video.transcription.as_ref().and_then(|t| t.sermon_start);
        data.trim_end = video.transcription.as_ref().and_then(|t| t.sermon_end);
    }
    manifest::save_video_data(&dir, &data)?;

    let entry = &mut manifest.entries[index];
    let was_ready = matches!(video.status, VideoStatus::ReadyForReview | VideoStatus::Ready);
    if entry.status == EntryStatus::New && was_ready && data.trimmed_file.is_some() {
        entry.status = EntryStatus::ReadyForReview;
    }
    Ok(())
}

/// Apply a run's outcome to state.json's old videos: finished ones are
/// dropped, failed ones count a try and are dropped after MAX_ATTEMPTS.
/// Returns notices for the library screen (dismissible).
pub fn settle(legacy: &mut serde_json::Map<String, serde_json::Value>, outcome: &Outcome) -> Vec<String> {
    let mut notices = Vec::new();
    for key in &outcome.done {
        legacy.remove(key);
    }
    let mut given_up = Vec::new();
    for f in &outcome.failed {
        let Some(serde_json::Value::Object(video)) = legacy.get_mut(&f.key) else {
            legacy.remove(&f.key);
            continue;
        };
        let attempts = video.get(ATTEMPTS_KEY).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
        if attempts >= MAX_ATTEMPTS {
            legacy.remove(&f.key);
            given_up.push(format!("{} ({})", f.title, f.error));
        } else {
            video.insert(ATTEMPTS_KEY.into(), attempts.into());
        }
    }
    if !outcome.outside.is_empty() {
        notices.push(format!(
            "These videos from before the library update are outside the library folder, so they're no longer listed. Copy them into the library folder to keep working on them: {}",
            outcome.outside.join(", ")
        ));
    }
    if !outcome.missing.is_empty() {
        notices.push(format!(
            "These videos from before the library update couldn't be found, so they aren't in the library: {}",
            outcome.missing.join(", ")
        ));
    }
    if !given_up.is_empty() {
        notices.push(format!(
            "Sermon Cut couldn't bring these videos from before the library update into the library after {} tries. Their files haven't been deleted: {}",
            MAX_ATTEMPTS,
            given_up.join("; ")
        ));
    }
    notices
}

/// After a committed migration: delete old WAVs and the old output folders
/// if they ended up empty. Never touches anything else.
pub fn cleanup(paths: &LibraryPaths, outcome: &Outcome) {
    for wav in &outcome.leftover_audio {
        if wav.extension().is_some_and(|e| e == "wav") {
            let _ = std::fs::remove_file(wav);
        }
    }
    for dir in ["audio", "trimmed", "thumbnails"] {
        let _ = std::fs::remove_dir(paths.root.join(dir)); // only succeeds if empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TranscriptionResult;

    fn old_video(id: &str, file: &Path, root: &Path, status: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "title": format!("Title {}", id), "url": format!("https://www.youtube.com/watch?v={}", id),
            "thumbnail": null, "duration": 3600.0, "channel": "Church", "upload_date": null,
            "file_path": file.to_string_lossy(),
            "audio_path": root.join("audio").join(format!("{}.wav", id)).to_string_lossy(),
            "thumbnail_path": root.join("thumbnails").join(format!("{}_thumbnail_option_1.jpg", id)).to_string_lossy(),
            "transcription": TranscriptionResult {
                segments: vec![], full_text: "hi".into(), sermon_start: Some(10.0), sermon_end: Some(20.0),
                suggested_title: Some("Sermon".into()), suggested_description: None, suggested_chapters: None, analysis_error: None,
            },
            "status": status, "download_progress": 100.0, "created_at": "",
            "trimmed_path": root.join("trimmed").join(format!("{}_trimmed.mp4", id)).to_string_lossy(),
            "thumbnail_options": [root.join("thumbnails").join(format!("{}_thumbnail_option_1.jpg", id)).to_string_lossy()],
        })
    }

    fn setup() -> (tempfile::TempDir, LibraryPaths, serde_json::Map<String, serde_json::Value>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for d in ["audio", "trimmed", "thumbnails"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        std::fs::write(root.join("abc.webm"), b"video abc").unwrap();
        std::fs::write(root.join("audio/abc.wav"), b"wav").unwrap();
        std::fs::write(root.join("trimmed/abc_trimmed.mp4"), b"trim").unwrap();
        std::fs::write(root.join("thumbnails/abc_thumbnail_option_1.jpg"), b"jpg").unwrap();
        std::fs::write(root.join("thumbnails/abc_thumbnail_option_1_raw.jpg"), b"raw").unwrap();

        let elsewhere = tempfile::tempdir().unwrap();
        let outside_file = elsewhere.path().join("home video.mov");
        std::fs::write(&outside_file, b"x").unwrap();

        let mut legacy = serde_json::Map::new();
        legacy.insert("abc".into(), old_video("abc", &root.join("abc.webm"), &root, "ready_for_review"));
        legacy.insert("local_1".into(), old_video("local_1", &outside_file, &root, "downloaded"));
        legacy.insert("gone".into(), old_video("gone", &root.join("gone.mp4"), &root, "downloaded"));

        let paths = LibraryPaths::new(root);
        paths.ensure_dir(&paths.data_dir()).unwrap();
        (tmp, paths, legacy, elsewhere)
    }

    #[test]
    fn migrates_inside_videos_and_reports_outside_ones() {
        let (_t, paths, legacy, _e) = setup();
        let mut m = Manifest::default();
        let outcome = migrate(&paths, &mut m, &legacy);

        assert_eq!(m.entries.len(), 1);
        let e = &m.entries[0];
        assert_eq!(e.file_name, "abc.webm");
        assert_eq!(e.title.as_deref(), Some("Title abc"));
        assert_eq!(e.status, EntryStatus::ReadyForReview);
        assert_eq!(outcome.outside, vec!["Title local_1".to_string()]);
        assert_eq!(outcome.missing, vec!["Title gone".to_string()]);
        assert!(outcome.failed.is_empty());
        assert_eq!(outcome.done.len(), 3);

        let dir = paths.video_dir(&e.id).unwrap();
        assert!(dir.join("abc_trimmed.mp4").exists());
        assert!(dir.join("abc_thumbnail_option_1.jpg").exists());
        assert!(dir.join("abc_thumbnail_option_1_raw.jpg").exists());
        assert!(manifest::load_transcript(&dir).is_some());
        let data = manifest::load_video_data(&dir);
        assert_eq!(data.trimmed_file.as_deref(), Some("abc_trimmed.mp4"));
        assert_eq!(data.selected_thumbnail.as_deref(), Some("abc_thumbnail_option_1.jpg"));
        assert_eq!(data.trim_start, Some(10.0));

        cleanup(&paths, &outcome);
        assert!(!paths.root.join("audio").exists());
        assert!(!paths.root.join("trimmed").exists());
        assert!(!paths.root.join("thumbnails").exists());
        assert!(paths.root.join("abc.webm").exists());
    }

    #[test]
    fn rerunning_after_partial_run_reuses_entries() {
        let (_t, paths, legacy, _e) = setup();
        let mut m = Manifest::default();
        migrate(&paths, &mut m, &legacy);
        let id = m.entries[0].id.clone();
        // A second launch (e.g. the first one crashed before committing).
        migrate(&paths, &mut m, &legacy);
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].id, id);
        let data = manifest::load_video_data(&paths.video_dir(&id).unwrap());
        assert_eq!(data.thumbnail_options.len(), 1);
        assert_eq!(data.trimmed_file.as_deref(), Some("abc_trimmed.mp4"));
    }

    #[test]
    fn adopts_entry_a_scan_already_made() {
        let (_t, paths, legacy, _e) = setup();
        let mut m = Manifest::default();
        let fp = fsutil::fingerprint(&paths.root.join("abc.webm")).unwrap();
        m.entries.push(Entry::new("abc.webm".into(), fp, None));
        let id = m.entries[0].id.clone();
        migrate(&paths, &mut m, &legacy);
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].id, id);
        assert_eq!(m.entries[0].legacy_id.as_deref(), Some("abc"));
    }

    #[cfg(unix)]
    #[test]
    fn one_bad_video_doesnt_stop_the_rest_and_is_retried_then_given_up() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, paths, mut legacy, _e) = setup();
        let bad = paths.root.join("bad.mp4");
        std::fs::write(&bad, b"unreadable").unwrap();
        legacy.insert("bad".into(), old_video("bad", &bad, &paths.root, "downloaded"));
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut m = Manifest::default();
        let outcome = migrate(&paths, &mut m, &legacy);
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].key, "bad");
        // The good one still moved.
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].file_name, "abc.webm");
        let notices = settle(&mut legacy, &outcome);
        assert_eq!(legacy.keys().collect::<Vec<_>>(), vec!["bad"]);
        assert_eq!(legacy["bad"][ATTEMPTS_KEY], 1);
        assert_eq!(notices.len(), 2, "outside and missing: {notices:?}");
        assert!(notices[1].contains("Title gone"));

        // Second launch: fails again, no notice yet.
        let outcome = migrate(&paths, &mut m, &legacy);
        assert!(settle(&mut legacy, &outcome).is_empty());
        assert_eq!(legacy["bad"][ATTEMPTS_KEY], 2);

        // Fixed before the third: it moves and is dropped.
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut retry = legacy.clone();
        let outcome = migrate(&paths, &mut m, &retry);
        assert!(outcome.failed.is_empty());
        assert!(settle(&mut retry, &outcome).is_empty());
        assert!(retry.is_empty());
        assert!(m.entries.iter().any(|e| e.file_name == "bad.mp4"));

        // Still broken on the third try instead: given up, with a notice.
        let mut m2 = Manifest::default();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = migrate(&paths, &mut m2, &legacy);
        let notices = settle(&mut legacy, &outcome);
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(legacy.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Title bad") && notices[0].contains("3 tries"), "{}", notices[0]);
    }
}
