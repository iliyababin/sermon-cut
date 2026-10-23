use super::*;

impl Library {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{outcome, processed_library, public};

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
}
