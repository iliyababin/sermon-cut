use super::*;

impl Library {
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
            g.require_not_sweeping()?;
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
}
