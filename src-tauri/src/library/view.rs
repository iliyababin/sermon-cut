use super::*;

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

impl Inner {
    pub(super) fn status_of(&self, e: &Entry) -> ViewStatus {
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

/// Disk space for the library page: per video, and the drive it's on.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StorageView {
    pub videos: Vec<VideoStorage>,
    /// Sermon Cut's own files that belong to no video (the library list,
    /// leftovers in its temp folder).
    pub other: u64,
    /// The drive the library is on; None if it couldn't be read.
    pub disk_free: Option<u64>,
    pub disk_total: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VideoStorage {
    pub id: String,
    /// The recording in the library folder (0 if it's gone).
    pub original: u64,
    /// Sermon Cut's files for it: the cut, audio, thumbnails.
    pub files: u64,
}

/// Bytes in a folder and everything under it, not following links.
/// Missing or unreadable parts count as nothing.
fn folder_size(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    read.flatten()
        .map(|item| match item.file_type() {
            Ok(t) if t.is_dir() => folder_size(&item.path()),
            Ok(t) if t.is_file() => item.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

impl Library {
    pub fn view(&self, notices: Vec<String>) -> LibraryView {
        self.inner().view(notices)
    }

    /// How much space each video takes and how much the drive has left.
    /// Blocking (walks the video folders); nothing is read under `inner`.
    pub fn storage(&self) -> Result<StorageView, String> {
        let (paths, entries) = {
            let g = self.inner();
            let paths = g.paths.clone().ok_or("The library isn't open")?;
            let entries: Vec<(String, String)> =
                g.manifest.entries.iter().map(|e| (e.id.clone(), e.file_name.clone())).collect();
            (paths, entries)
        };
        let videos: Vec<VideoStorage> = entries
            .into_iter()
            .map(|(id, file_name)| {
                let original = paths
                    .original(&file_name)
                    .ok()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .unwrap_or(0);
                let files = paths.video_dir(&id).map(|d| folder_size(&d)).unwrap_or(0);
                VideoStorage { id, original, files }
            })
            .collect();
        let in_videos: u64 = videos.iter().map(|v| v.files).sum();
        // Folders of videos no longer listed count as "other" too.
        let other = folder_size(&paths.data_dir()).saturating_sub(in_videos);
        // statvfs's "available": on macOS it leaves out purgeable space, so
        // Finder can show a little more free.
        let disk = fs4::statvfs(&paths.root).ok();
        Ok(StorageView {
            videos,
            other,
            disk_free: disk.as_ref().map(|d| d.available_space()),
            disk_total: disk.as_ref().map(|d| d.total_space()),
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{outcome, processed_library, public};

    #[tokio::test]
    async fn detail_mentions_an_upload_of_the_same_file_under_another_entry() {
        let (_t, lib, id, _dir) = processed_library();
        assert_eq!(lib.detail(&id).unwrap().duplicate_upload, None);
        let job = lib.begin_upload(&id, None, None).await.unwrap();
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
    fn storage_counts_the_recording_and_the_video_folder() {
        let (t, lib, id, dir) = processed_library();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/audio.opus"), vec![0u8; 1000]).unwrap();
        let before = lib.storage().unwrap();
        let v = before.videos.iter().find(|v| v.id == id).unwrap();
        // a.mp4 holds "video"; the folder has three 3-byte jpgs, video.json
        // and the 1000-byte audio.
        assert_eq!(v.original, 5);
        let json = std::fs::metadata(dir.join("video.json")).unwrap().len();
        assert_eq!(v.files, 9 + json + 1000);
        assert!(before.disk_total.unwrap() >= before.disk_free.unwrap());
        assert!(before.disk_free.unwrap() > 0);

        // A missing recording counts as nothing rather than failing.
        std::fs::remove_file(t.path().join("lib/a.mp4")).unwrap();
        assert_eq!(lib.storage().unwrap().videos.iter().find(|v| v.id == id).unwrap().original, 0);
    }

    #[test]
    fn storage_counts_unlisted_leftovers_as_other() {
        let (_t, lib, _id, _dir) = processed_library();
        let base = lib.storage().unwrap().other;
        let paths = lib.inner().paths.clone().unwrap();
        let stray = paths.videos_dir().join("gone-video");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("trimmed.mp4"), vec![0u8; 500]).unwrap();
        assert_eq!(lib.storage().unwrap().other, base + 500);
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
}
