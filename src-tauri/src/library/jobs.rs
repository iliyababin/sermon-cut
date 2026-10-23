use super::*;

#[derive(Debug, Clone, Serialize)]
struct ProcessingProgressEvent {
    video_id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
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

fn fingerprint_matches(now: &Fingerprint, expected: &Fingerprint) -> bool {
    now.size == expected.size && now.hash == expected.hash
}

impl Library {
    // -----------------------------------------------------------------------
    // Queue

    pub async fn enqueue(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (root, expected) = self.check_enqueue(id).await?;
        self.add_to_queue(id, &root, &expected)
    }

    /// The first half of `enqueue`: everything that could refuse it, so a
    /// caller can prepare (see `reprocess`) only once it would be taken.
    /// Returns the library root and the file's fingerprint for
    /// `add_to_queue`.
    pub(super) async fn check_enqueue(self: &Arc<Self>, id: &str) -> Result<(PathBuf, Fingerprint), String> {
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
        let stat = path.clone();
        let meta = blocking(move || std::fs::symlink_metadata(stat))
            .await?
            .map_err(|e| format!("Can't read {}: {}", path.display(), e))?;
        if meta.len() != expected.size || fsutil::mtime_ms(&meta) != expected.mtime_ms {
            return Err(FILE_CHANGED.into());
        }
        Ok((root, expected))
    }

    /// The second half of `enqueue`, after the file was checked without the
    /// lock: everything may have changed meanwhile (the library switched,
    /// went read-only, the entry went or its file changed), so look again.
    pub(super) fn add_to_queue(&self, id: &str, root: &Path, expected: &Fingerprint) -> Result<(), String> {
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
    pub(super) fn start_next(&self) -> Option<(String, CancellationToken)> {
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
    pub(super) fn finish_job(&self, id: &str, result: &Option<Result<(), String>>) -> Option<&'static str> {
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
    /// is kept on a re-process. `before_swap` runs once the commit can no longer be
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
        let keep_review = {
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
            // A re-process keeps the review; a first processing (a new or
            // replaced file, a reset) starts without an old one.
            matches!(running.previous, EntryStatus::ReadyForReview | EntryStatus::Done)
        };
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
                review: if keep_review { old.review } else { Review::default() },
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
        let (_busy, paths, entry, _data) = self.begin_op(id).await?;
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
        let video_path = paths.original(&entry.file_name)?;
        let (work_paths, video) = (paths.clone(), id.to_string());
        let (dir, staging) = blocking(move || fresh_work_dir(&work_paths, &video, &format!("trim-{}", video))).await??;

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
        remove_work_dir(&staging).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{crash_after_where, png, processed_library, saved_queue, seen, test_key, TV};

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

    /// The first processing of a new or replaced file doesn't take a
    /// review left in its folder (one of the old content's).
    #[tokio::test]
    async fn a_first_processing_starts_without_an_old_review() {
        let (_t, lib, id, dir) = processed_library();
        let mut data = manifest::load_video_data(&dir);
        data.review.title = Some("Old content's title".into());
        manifest::save_video_data(&dir, &data).unwrap();
        lib.inner().entry_mut(&id).unwrap().status = EntryStatus::New;
        let (staging, outputs, hash) = job_at_commit(&lib, &id).await;
        lib.commit_job(&id, &dir, &staging.0, &hash, outputs, || {}).await.unwrap();
        drop(staging);
        lib.finish_job(&id, &Some(Ok(())));
        assert_eq!(manifest::load_video_data(&dir).review.title, None);
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
}
