use super::*;

impl Library {
    pub async fn regenerate_thumbnails(self: &Arc<Self>, id: &str) -> Result<Vec<String>, String> {
        let (_busy, paths, entry, data) = self.begin_op(id)?;
        async {
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
                crate::thumbnail::generate_thumbnail_options(&video_path, start, end, &title, &staging, 10).await;
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

            // Under the video's lock, on a blocking thread, never `inner`.
            let lock = self.video_lock(id);
            let (commit_dir, commit_staging, commit_names) = (dir.clone(), staging.clone(), names.clone());
            let committed = blocking(move || {
                let _v = hold(&lock);
                commit_regenerated(&commit_dir, &commit_staging, &commit_names)
            })
            .await
            .and_then(|r| r);
            let _ = std::fs::remove_dir_all(&staging);
            committed?;
            Ok(names.iter().map(|n| path_string(&dir.join(n))).collect())
        }
        .await
    }

    pub async fn custom_thumbnail(
        self: &Arc<Self>,
        id: &str,
        source: PathBuf,
        crop: crate::thumbnail::CropRect,
        grading: bool,
    ) -> Result<String, String> {
        let (_busy, paths, _entry, _data) = self.begin_op(id)?;
        async {
            let dir = paths.ensure_video_dir(id)?;
            let path = crate::thumbnail::process_custom_thumbnail(&source, &dir, &crop, grading).await?;
            let name = file_in_dir(&dir, &path)?;
            // Under the video's lock, on a blocking thread, never `inner`
            // (the video is busy, so its entry stays).
            let lock = self.video_lock(id);
            let (commit_dir, added) = (dir.clone(), name.clone());
            blocking(move || {
                let _v = hold(&lock);
                let mut data = manifest::load_video_data(&commit_dir);
                if !data.thumbnail_options.contains(&added) {
                    data.thumbnail_options.insert(0, added);
                }
                // The selection is by id, so adding a source never moves it; the
                // review screen selects the new one itself.
                let pruned = prune_custom_thumbnails(&mut data.thumbnail_options, data.selected_source.as_deref());
                manifest::save_video_data(&commit_dir, &data)?;
                for old in pruned {
                    let _ = std::fs::remove_file(commit_dir.join(old));
                }
                // Renders of the dropped sources.
                manifest::prune_renders(&commit_dir, &data);
                Ok::<_, String>(())
            })
            .await??;
            Ok(path_string(&dir.join(name)))
        }
        .await
    }

    /// Persist a template render of one thumbnail source, replacing that
    /// source's previous render. Never changes the selection. Allowed while
    /// the video uploads (the review screen stays usable), refused while it's
    /// queued or processing, which replaces every thumbnail.
    pub async fn save_rendered_thumbnail(
        self: &Arc<Self>,
        id: &str,
        source: &str,
        render_key: &str,
        template_version: u32,
        logo: &str,
        image_bytes: Vec<u8>,
    ) -> Result<String, String> {
        if !valid_render_key(render_key) {
            return Err("Invalid render key".into());
        }
        let source = source_id(source).ok_or_else(|| format!("Invalid thumbnail: {}", source))?;
        // The files are read and written under the video's lock, on a
        // blocking thread, never under `inner` (docs/INVARIANTS.md #4);
        // `inner` only decides.
        let paths = self.with_entry(id, |g, paths| {
            g.require_render_allowed(id)?;
            Ok(paths.clone())
        })?;
        let job = RenderSave {
            lib: self.clone(),
            id: id.to_string(),
            source,
            render_key: render_key.to_string(),
            template_version,
            logo: logo.to_string(),
        };
        let dir = {
            let job = job.clone();
            blocking(move || {
                let lock = job.lib.video_lock(&job.id);
                let _v = hold(&lock);
                let dir = paths.ensure_video_dir(&job.id)?;
                job.check(&dir, &manifest::load_video_data(&dir))?;
                Ok::<_, String>(dir)
            })
            .await??
        };

        // Named here, never by the webview. Written under a hidden temp name
        // (swept on the next open if we crash) that no save or prune touches.
        let name = manifest::render_file_name(&job.source);
        let tmp = dir.join(format!("{}{}", manifest::RENDER_TMP_PREFIX, name));
        let out = tmp.clone();
        let rendering = RenderingGuard::new(self);
        let written = blocking(move || crate::thumbnail::save_for_youtube(&image_bytes, &out)).await.and_then(|r| r);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // Moved into place and recorded together, under the video's lock, so
        // no prune or commit can see the file unrecorded; then the replaced
        // render goes.
        let committed = {
            let (dir, name, tmp) = (dir.clone(), name.clone(), tmp.clone());
            blocking(move || {
                let lock = job.lib.video_lock(&job.id);
                let _v = hold(&lock);
                let mut data = manifest::load_video_data(&dir);
                job.check(&dir, &data)?;
                std::fs::rename(&tmp, dir.join(&name)).map_err(|e| format!("Failed to save thumbnail: {}", e))?;
                data.renders.retain(|r| r.source != job.source);
                data.renders.push(Render { source: job.source.clone(), file: name.clone(), render_key: job.render_key.clone() });
                if let Err(e) = manifest::save_video_data(&dir, &data) {
                    let _ = std::fs::remove_file(dir.join(&name));
                    return Err(e);
                }
                manifest::prune_renders(&dir, &data);
                Ok(())
            })
            .await
            .and_then(|r| r)
        };
        drop(rendering);
        if let Err(e) = committed {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(path_string(&dir.join(name)))
    }
}

/// A render being saved (see `save_rendered_thumbnail`).
#[derive(Clone)]
struct RenderSave {
    lib: Arc<Library>,
    id: String,
    source: String,
    render_key: String,
    template_version: u32,
    logo: String,
}

impl RenderSave {
    /// Whether the render may still be saved: the video isn't being
    /// processed (asked of `inner`, briefly), its source is still an option,
    /// and its key is the saved review's. Called holding the video's lock.
    fn check(&self, dir: &Path, data: &VideoData) -> Result<(), String> {
        self.lib.with_entry(&self.id, |g, _| g.require_render_allowed(&self.id))?;
        if !data.thumbnail_options.contains(&self.source) {
            return Err("That thumbnail is no longer one of this video's options".into());
        }
        check_render_key(dir, data, &self.source, self.template_version, &self.logo, &self.render_key)
    }
}

/// Counts a render being written, so a reopen doesn't sweep its temp file.
struct RenderingGuard<'a> {
    lib: &'a Library,
}

impl<'a> RenderingGuard<'a> {
    fn new(lib: &'a Library) -> Self {
        lib.inner().rendering += 1;
        RenderingGuard { lib }
    }
}

impl Drop for RenderingGuard<'_> {
    fn drop(&mut self) {
        self.lib.inner().rendering -= 1;
    }
}

/// A thumbnail source id from what the webview sent: a file name, or a path
/// whose file name is used.
pub(super) fn source_id(s: &str) -> Option<String> {
    Path::new(s)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| super::is_valid_file_name(n))
}

fn valid_render_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The plain crop written next to a generated option
/// ("thumbnail_option_3.jpg" → "thumbnail_option_3_raw.jpg").
fn raw_variant(id: &str) -> Option<String> {
    let stem = id.strip_suffix(".jpg")?;
    let n = stem.strip_prefix("thumbnail_option_")?;
    n.bytes().all(|b| b.is_ascii_digit()).then(|| format!("{}_raw.jpg", stem))
}

/// Thumbnail sources the user added; regenerating keeps them.
fn is_custom_thumbnail(name: &str) -> bool {
    name.starts_with("custom_thumbnail_")
}

/// Swap regenerated thumbnail options (staged as thumbnail_option_N.jpg and
/// their plain crops) in for the generated ones, through the commit journal,
/// so a crash leaves the old set or the new one. Custom thumbnails and their
/// renders stay. A selected custom one stays selected; a selected generated
/// one becomes the new option at the same place (else the first). The
/// replaced options' renders go in the same commit. Call it under the
/// video's lock.
fn commit_regenerated(dir: &Path, staging: &Path, names: &[String]) -> Result<(), String> {
    let plan = RegeneratePlan::new(&manifest::load_video_data(dir), names);
    swap_in_outputs_where(dir, staging, &|n| plan.replaces(n), || manifest::save_video_data(dir, &plan.data))?;
    manifest::prune_renders(dir, &plan.data);
    Ok(())
}

/// What regenerating changes (see `commit_regenerated`).
struct RegeneratePlan {
    /// The new video.json.
    data: VideoData,
    /// The generated options being replaced, and their renders' files.
    replaced: HashSet<String>,
    replaced_renders: HashSet<String>,
    trimmed: Option<String>,
}

impl RegeneratePlan {
    fn new(old: &VideoData, names: &[String]) -> Self {
        let replaced: HashSet<String> = old.thumbnail_options.iter().filter(|o| !is_custom_thumbnail(o)).cloned().collect();
        let replaced_renders = old.renders.iter().filter(|r| replaced.contains(&r.source)).map(|r| r.file.clone()).collect();
        let generated: Vec<&String> = old.thumbnail_options.iter().filter(|o| replaced.contains(*o)).collect();
        let selected = match old.selected_or_first() {
            Some(s) if !replaced.contains(s) => Some(s.clone()),
            Some(s) => generated.iter().position(|g| *g == s).and_then(|i| names.get(i)).or(names.first()).cloned(),
            None => names.first().cloned(),
        };
        let mut data = old.clone();
        data.thumbnail_options = old
            .thumbnail_options
            .iter()
            .filter(|o| !replaced.contains(*o))
            .chain(names.iter())
            .cloned()
            .collect();
        data.renders.retain(|r| !replaced.contains(&r.source));
        data.selected_source = selected;
        RegeneratePlan { data, replaced, replaced_renders, trimmed: old.trimmed_file.clone() }
    }

    /// Whether the file `name` in the video's folder is replaced (moved aside,
    /// then deleted): the generated options with their plain crops, and
    /// their renders. Never the trimmed video.
    fn replaces(&self, name: &str) -> bool {
        self.trimmed.as_deref() != Some(name)
            && (name.starts_with("thumbnail_option_") || self.replaced.contains(name) || self.replaced_renders.contains(name))
    }
}

/// What the template draws over for source `id` (its plain crop, if it's a
/// generated option that has one), and that file's version (its mtime), part
/// of the render key.
pub(super) fn render_from(dir: &Path, id: &str) -> (PathBuf, String) {
    let raw = raw_variant(id).map(|r| dir.join(r)).filter(|p| p.is_file());
    let from = raw.unwrap_or_else(|| dir.join(id));
    let version = std::fs::metadata(&from)
        .map(|m| fsutil::mtime_ms(&m).to_string())
        .unwrap_or_default();
    (from, version)
}

/// Refuse a render whose key isn't the one the saved review gives for this
/// source now: it was drawn with a title, speaker or image that has changed
/// since. Keys, not counters: inputs that go A→B→A make A's render current.
fn check_render_key(dir: &Path, data: &VideoData, source: &str, template_version: u32, logo: &str, key: &str) -> Result<(), String> {
    let (_, version) = render_from(dir, source);
    let r = &data.review;
    let expected = manifest::render_key(
        template_version,
        r.title.as_deref().unwrap_or(""),
        r.preacher.as_deref().unwrap_or(""),
        r.single_line,
        source,
        &version,
        logo,
    );
    if expected != key {
        return Err(STALE_RENDER.into());
    }
    Ok(())
}

const STALE_RENDER: &str = "The title, speaker, logo or image changed while the thumbnail was being drawn. It will be drawn again.";

/// Which file to send to YouTube as the thumbnail (see `begin_upload`).
pub(super) fn resolve_thumbnail(
    dir: &Path,
    data: &VideoData,
    source: Option<&str>,
    render_key: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let source = match source {
        Some(s) => {
            let id = source_id(s).filter(|id| data.thumbnail_options.contains(id));
            Some(id.ok_or_else(|| format!("{} isn't one of this video's thumbnails", s))?)
        }
        None => data.selected_or_first().cloned(),
    };
    let Some(source) = source else { return Ok(None) };
    let render = data
        .render_for(&source)
        .filter(|r| render_key.is_none_or(|k| k == r.render_key))
        .map(|r| dir.join(&r.file))
        .filter(|p| p.is_file());
    let path = render.unwrap_or_else(|| dir.join(&source));
    Ok(path.is_file().then_some(path))
}

/// Resolve a path the frontend sent back to a file directly inside `dir`.
/// Custom thumbnails kept per video, besides the selected one.
const KEEP_CUSTOM_THUMBNAILS: usize = 3;

/// Drop all but the newest custom thumbnails (they're added at the front)
/// and the selected one from `options`; returns the dropped names.
fn prune_custom_thumbnails(options: &mut Vec<String>, selected: Option<&str>) -> Vec<String> {
    let mut customs = 0;
    let mut pruned = Vec::new();
    options.retain(|name| {
        if !name.starts_with("custom_thumbnail_") {
            return true;
        }
        customs += 1;
        if customs <= KEEP_CUSTOM_THUMBNAILS || Some(name.as_str()) == selected {
            return true;
        }
        pruned.push(name.clone());
        false
    });
    pruned
}

fn file_in_dir(dir: &Path, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| super::is_valid_file_name(n))
        .ok_or_else(|| format!("Invalid file: {}", path))?;
    let parent_ok = fsutil::is_directly_in(p, dir);
    if !parent_ok || !dir.join(&name).is_file() {
        return Err(format!("{} isn't one of this video's files", path));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::manifest::RENDERED_PREFIX;
    use super::*;
    use super::super::tests::{contents, crash_after_where, png, processed_library, public, rendered, test_key, TV};

    /// Regression (55e4d3b): a custom thumbnail is cut only from the video's
    /// own images or a picked one.
    #[tokio::test]
    async fn a_custom_thumbnail_reads_only_allowed_images() {
        let (_t, lib, id, dir) = processed_library();
        // Not under the temp folder, which the app may use.
        let elsewhere = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        let outside = elsewhere.path().join("Pictures/pastor.png");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, png()).unwrap();
        std::fs::write(dir.join("thumbnail_option_1_raw.jpg"), png()).unwrap();
        let rect = crate::thumbnail::CropRect { x: 0, y: 0, width: 4, height: 4 };
        let root = lib.root();
        let check = |picked: bool| {
            let root = root.clone();
            move |p: &str| crate::commands::check_path_in(root, None, picked, p, crate::commands::Access::Data)
        };
        let custom = super::super::super::commands::custom_thumbnail_checked;
        let options = || manifest::load_video_data(&dir).thumbnail_options;
        let before = options();

        let err = custom(&lib, check(false), &id, &outside.to_string_lossy(), rect.clone(), false).await.unwrap_err();
        assert!(err.contains("only use files in its own folders"), "{err}");
        assert_eq!(options(), before, "nothing added");
        assert!(lib.inner().busy.is_empty());

        // The video's own option, cropped again: allowed.
        let own = dir.join("thumbnail_option_1_raw.jpg");
        let made = custom(&lib, check(false), &id, &own.to_string_lossy(), rect.clone(), false).await.unwrap();
        assert_eq!(options()[0], Path::new(&made).file_name().unwrap().to_string_lossy());
        // A picked image: allowed.
        custom(&lib, check(true), &id, &outside.to_string_lossy(), rect, false).await.unwrap();
    }

    #[tokio::test]
    async fn a_render_of_inputs_no_longer_saved_is_refused() {
        let (_t, lib, id, dir) = processed_library();
        let source = "thumbnail_option_1.jpg";
        let review = |title: &str| Review { title: Some(title.into()), preacher: Some("P".into()), ..Default::default() };
        let key_a = test_key(&dir, source, "A");
        lib.save_review(&id, review("A"), None).unwrap();
        lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap();

        // The title became B: a render of A is stale.
        lib.save_review(&id, review("B"), None).unwrap();
        let err = lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap_err();
        assert_eq!(err, STALE_RENDER);
        assert_eq!(lib.detail(&id).unwrap().renders[0].render_key, key_a, "the stored render is kept");

        // And back to A: A's render is current again.
        lib.save_review(&id, review("A"), None).unwrap();
        lib.save_rendered_thumbnail(&id, source, &key_a, TV, "", png()).await.unwrap();
        // So is a render made with the speaker on one line, once saved so.
        let one_line = Review { single_line: true, ..review("A") };
        lib.save_review(&id, one_line, None).unwrap();
        let (_, version) = render_from(&dir, source);
        let key = manifest::render_key(TV, "A", "P", true, source, &version, "");
        lib.save_rendered_thumbnail(&id, source, &key, TV, "", png()).await.unwrap();
        assert!(lib.save_rendered_thumbnail(&id, source, &key, TV + 1, "", png()).await.is_err(), "another template");
        assert_eq!(rendered_files(&dir).len(), 1);

        // A logo chosen in Settings: a render without it is stale, one
        // drawn with it is current.
        let logo = "/Users/me/Pictures/logo.png";
        assert_eq!(lib.save_rendered_thumbnail(&id, source, &key, TV, logo, png()).await.unwrap_err(), STALE_RENDER);
        let with_logo = manifest::render_key(TV, "A", "P", true, source, &version, logo);
        assert_ne!(with_logo, key);
        lib.save_rendered_thumbnail(&id, source, &with_logo, TV, logo, png()).await.unwrap();
    }

    fn rendered_files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(RENDERED_PREFIX))
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn renders_replace_per_source_and_keep_the_selection() {
        let (_t, lib, id, dir) = processed_library();
        let first = rendered(&lib, &id, "thumbnail_option_1.jpg", "k1", png()).await.unwrap();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k1", png()).await.unwrap();
        // Re-rendering a source replaces its render and deletes the old file.
        let second = rendered(&lib, &id, "thumbnail_option_1.jpg", "k2", png()).await.unwrap();
        assert!(!Path::new(&first).exists());
        assert!(Path::new(&second).exists());
        assert_eq!(rendered_files(&dir).len(), 2);

        let d = lib.detail(&id).unwrap();
        assert_eq!(d.thumbnail_options.len(), 2, "renders never become options");
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        let r1 = d.renders.iter().find(|r| r.source == "thumbnail_option_1.jpg").unwrap();
        assert_eq!(r1.render_key, test_key(&dir, "thumbnail_option_1.jpg", "k2"));
        assert_eq!(r1.path, second);
        // Generated options render from their plain crop.
        assert!(d.thumbnail_options[0].render_from.ends_with("thumbnail_option_1_raw.jpg"));
        assert!(d.thumbnail_options[1].render_from.ends_with("thumbnail_option_2.jpg"));
        // video.json's selected_thumbnail (read by v0.4.0) is the selected render.
        let sel = d.selected_thumbnail.unwrap();
        assert!(Path::new(&sel).file_name().unwrap().to_string_lossy().starts_with(RENDERED_PREFIX));

        // Reopening shows the same renders and options: nothing stacks.
        let again = lib.detail(&id).unwrap();
        assert_eq!(again.thumbnail_options.len(), 2);
        assert_eq!(again.renders.len(), 2);
    }

    #[tokio::test]
    async fn saves_keep_unrecorded_renders_until_the_next_open_sweeps_them() {
        let (_t, lib, id, dir) = processed_library();
        let kept = rendered(&lib, &id, "thumbnail_option_1.jpg", "k1", png()).await.unwrap();
        // A render being written, and one renamed into place but not yet
        // recorded, while the review screen saves.
        let writing = format!("{}{}", manifest::RENDER_TMP_PREFIX, manifest::render_file_name("thumbnail_option_2.jpg"));
        std::fs::write(dir.join(&writing), b"half").unwrap();
        let unrecorded = manifest::render_file_name("thumbnail_option_2.jpg");
        std::fs::write(dir.join(&unrecorded), b"r").unwrap();
        lib.save_review(&id, Review::default(), Some("thumbnail_option_1.jpg".into())).unwrap();
        assert!(dir.join(&writing).exists());
        assert!(dir.join(&unrecorded).exists());

        // Reopened after a crash: both are left over, and go.
        lib.open_blocking(false);
        assert!(!dir.join(&writing).exists());
        assert!(!dir.join(&unrecorded).exists());
        assert!(Path::new(&kept).exists());
    }

    #[tokio::test]
    async fn adding_a_source_keeps_selection_and_renders() {
        let (_t, lib, id, dir) = processed_library();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();
        // What custom_thumbnail does: a new source at the front.
        std::fs::write(dir.join("custom_thumbnail_1.jpg"), b"jpg").unwrap();
        let mut data = manifest::load_video_data(&dir);
        data.thumbnail_options.insert(0, "custom_thumbnail_1.jpg".into());
        manifest::save_video_data(&dir, &data).unwrap();

        let d = lib.detail(&id).unwrap();
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        assert_eq!(d.renders.len(), 1);
        assert_eq!(d.renders[0].source, "thumbnail_option_2.jpg");
        // Dropping an unselected source keeps the selection too.
        let mut data = manifest::load_video_data(&dir);
        data.thumbnail_options.retain(|o| o != "thumbnail_option_1.jpg");
        manifest::save_video_data(&dir, &data).unwrap();
        assert_eq!(lib.detail(&id).unwrap().selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
    }

    #[tokio::test]
    async fn renders_are_allowed_during_upload_but_not_processing() {
        let (_t, lib, id, _dir) = processed_library();
        let job = lib.begin_upload(&id, None, None).unwrap();
        rendered(&lib, &id, "thumbnail_option_1.jpg", "k", png()).await.unwrap();
        lib.finish_upload(&id, job, None, &public());

        lib.inner().queue.push_back(id.clone());
        let err = rendered(&lib, &id, "thumbnail_option_1.jpg", "k2", png()).await.unwrap_err();
        assert!(err.contains("processed"), "{err}");
        lib.inner().queue.clear();

        // Not one of the options, or a key that isn't plain ASCII.
        assert!(lib.save_rendered_thumbnail(&id, "../../x.jpg", "k", TV, "", png()).await.is_err());
        assert!(lib.save_rendered_thumbnail(&id, "other.jpg", "k", TV, "", png()).await.is_err());
        assert!(lib.save_rendered_thumbnail(&id, "thumbnail_option_1.jpg", "k/../", TV, "", png()).await.is_err());
    }

    #[tokio::test]
    async fn upload_uses_the_current_render_or_the_source() {
        let (_t, lib, id, dir) = processed_library();
        let render = rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();

        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some(test_key(&dir, "thumbnail_option_2.jpg", "k"))).unwrap();
        assert_eq!(job.thumbnail_path.as_deref(), Some(Path::new(&render)));
        lib.finish_upload(&id, job, None, &public());

        // A stale key falls back to the source itself.
        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some("old".into())).unwrap();
        assert_eq!(job.thumbnail_path, Some(dir.join("thumbnail_option_2.jpg")));
        lib.finish_upload(&id, job, None, &public());

        // No source given: the selected one's render.
        let job = lib.begin_upload(&id, None, None).unwrap();
        assert_eq!(job.thumbnail_path.as_deref(), Some(Path::new(&render)));
        lib.finish_upload(&id, job, None, &public());

        assert!(lib.begin_upload(&id, Some("/etc/passwd".into()), None).is_err());
        assert!(!lib.inner().busy.contains(&id), "a refused upload doesn't stay busy");
    }

    #[tokio::test]
    async fn an_edit_during_an_upload_does_not_lose_its_thumbnail() {
        let (_t, lib, id, dir) = processed_library();
        let render = rendered(&lib, &id, "thumbnail_option_2.jpg", "k", png()).await.unwrap();
        let sent = std::fs::read(&render).unwrap();
        let job = lib.begin_upload(&id, Some("thumbnail_option_2.jpg".into()), Some(test_key(&dir, "thumbnail_option_2.jpg", "k"))).unwrap();

        // The title changes mid-upload: a new render replaces (deletes) the old one.
        let mut other = image::RgbImage::new(8, 8);
        other.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        other.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        rendered(&lib, &id, "thumbnail_option_2.jpg", "k2", bytes.into_inner()).await.unwrap();
        assert!(!Path::new(&render).exists());

        assert_eq!(job.thumbnail_data.as_deref(), Some(sent.as_slice()));
        lib.finish_upload(&id, job, None, &public());
    }

    /// A video with a custom thumbnail (rendered), two generated options
    /// (the second rendered and selected), and three regenerated ones staged.
    fn regenerate_fixture(selected: &str) -> (tempfile::TempDir, PathBuf, PathBuf, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("video");
        let staging = tmp.path().join("thumbs");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&staging).unwrap();
        let custom_render = "thumbnail_rendered_custom_thumbnail_1_00000000000000000000000000000001.jpg";
        let option_render = "thumbnail_rendered_thumbnail_option_2_00000000000000000000000000000002.jpg";
        for name in [
            "transcript.json",
            "trimmed.mp4",
            "custom_thumbnail_1.jpg",
            "thumbnail_option_1.jpg",
            "thumbnail_option_1_raw.jpg",
            "thumbnail_option_2.jpg",
            "thumbnail_option_2_raw.jpg",
            custom_render,
            option_render,
        ] {
            std::fs::write(dir.join(name), format!("old {name}")).unwrap();
        }
        let render = |source: &str, file: &str| Render { source: source.into(), file: file.into(), render_key: "k".into() };
        let data = VideoData {
            trimmed_file: Some("trimmed.mp4".into()),
            thumbnail_options: vec!["custom_thumbnail_1.jpg".into(), "thumbnail_option_1.jpg".into(), "thumbnail_option_2.jpg".into()],
            selected_source: Some(selected.into()),
            renders: vec![render("custom_thumbnail_1.jpg", custom_render), render("thumbnail_option_2.jpg", option_render)],
            ..Default::default()
        };
        manifest::save_video_data(&dir, &data).unwrap();
        let names: Vec<String> = (1..=3).map(|n| format!("thumbnail_option_{n}.jpg")).collect();
        for n in 1..=3 {
            for name in [format!("thumbnail_option_{n}.jpg"), format!("thumbnail_option_{n}_raw.jpg")] {
                std::fs::write(staging.join(&name), format!("new {name}")).unwrap();
            }
        }
        (tmp, dir, staging, names)
    }

    #[test]
    fn regenerating_keeps_custom_thumbnails_and_maps_the_selection() {
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        commit_regenerated(&dir, &staging, &names).unwrap();
        let d = manifest::load_video_data(&dir);
        assert_eq!(
            d.thumbnail_options,
            vec!["custom_thumbnail_1.jpg", "thumbnail_option_1.jpg", "thumbnail_option_2.jpg", "thumbnail_option_3.jpg"]
        );
        // The second generated option was selected: the new second one is.
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_2.jpg"));
        // The custom one's render stays; the replaced option's goes.
        assert_eq!(d.renders.iter().map(|r| r.source.as_str()).collect::<Vec<_>>(), vec!["custom_thumbnail_1.jpg"]);
        let files: Vec<String> = contents(&dir).into_iter().map(|(n, _)| n).collect();
        assert!(!files.iter().any(|f| f.contains("thumbnail_option_2_0000")), "{files:?}");
        assert!(files.iter().any(|f| f.contains("custom_thumbnail_1_0000")));
        for (name, body) in contents(&dir) {
            if name.starts_with("thumbnail_option_") {
                assert!(body.starts_with("new"), "{name}: {body}");
            } else if name != "video.json" && name != "transcript.json" {
                assert!(body.starts_with("old"), "{name}: {body}");
            }
        }

        // A selected custom thumbnail stays selected.
        let (_t, dir, staging, names) = regenerate_fixture("custom_thumbnail_1.jpg");
        commit_regenerated(&dir, &staging, &names).unwrap();
        assert_eq!(manifest::load_video_data(&dir).selected_source.as_deref(), Some("custom_thumbnail_1.jpg"));

        // A selected option past the new ones' end: the first new one.
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        for f in ["thumbnail_option_2.jpg", "thumbnail_option_2_raw.jpg", "thumbnail_option_3.jpg", "thumbnail_option_3_raw.jpg"] {
            std::fs::remove_file(staging.join(f)).unwrap();
        }
        commit_regenerated(&dir, &staging, &names[..1]).unwrap();
        let d = manifest::load_video_data(&dir);
        assert_eq!(d.selected_source.as_deref(), Some("thumbnail_option_1.jpg"));
        assert_eq!(d.thumbnail_options, vec!["custom_thumbnail_1.jpg", "thumbnail_option_1.jpg"]);
        assert!(!dir.join("thumbnail_option_2.jpg").exists() && !dir.join("thumbnail_option_2_raw.jpg").exists());
    }

    #[test]
    fn a_crash_while_regenerating_leaves_the_old_thumbnails() {
        // After each step: journal written, each old file moved aside, each
        // new one moved in, and video.json written.
        let (_t, dir, staging, names) = regenerate_fixture("thumbnail_option_2.jpg");
        let plan = RegeneratePlan::new(&manifest::load_video_data(&dir), &names);
        let replaces = |n: &str| plan.replaces(n);
        let moves = names_in(&dir).unwrap().iter().filter(|n| is_output(n) && replaces(n)).count() + 6;
        assert_eq!(moves, 5 + 6);
        for steps in 0..=moves {
            for write_data in [false, true] {
                let (_t, dir, staging, _) = regenerate_fixture("thumbnail_option_2.jpg");
                let before = contents(&dir);
                crash_after_where(&dir, &staging, steps, &replaces);
                if write_data {
                    manifest::save_video_data(&dir, &plan.data).unwrap();
                }
                recover_commit(&dir);
                assert_eq!(contents(&dir), before, "after {steps} moves");
            }
        }

        // A crash during the clean-up, once the commit was marked done: the
        // new set stays, and the custom thumbnail with it.
        crash_after_where(&dir, &staging, moves, &replaces);
        manifest::save_video_data(&dir, &plan.data).unwrap();
        let backup = dir.join(COMMIT_BACKUP);
        let mut journal: CommitJournal = fsutil::read_json(&backup.join(COMMIT_JOURNAL), 1 << 20).unwrap().unwrap();
        journal.phase = CommitPhase::Done;
        write_journal(&backup, &journal).unwrap();
        recover_commit(&dir);
        assert!(!backup.exists());
        let d = manifest::load_video_data(&dir);
        assert_eq!(d.thumbnail_options.len(), 4);
        assert_eq!(std::fs::read_to_string(dir.join("custom_thumbnail_1.jpg")).unwrap(), "old custom_thumbnail_1.jpg");
        assert_eq!(std::fs::read_to_string(dir.join("thumbnail_option_3.jpg")).unwrap(), "new thumbnail_option_3.jpg");
    }

    #[test]
    fn raw_variants() {
        assert_eq!(raw_variant("thumbnail_option_3.jpg").as_deref(), Some("thumbnail_option_3_raw.jpg"));
        assert_eq!(raw_variant("thumbnail_option_3_raw.jpg"), None);
        assert_eq!(raw_variant("custom_thumbnail_1.jpg"), None);
    }

    #[test]
    fn custom_thumbnails_keep_three_newest_and_the_selected_one() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut options = names(&[
            "custom_thumbnail_5.jpg",
            "custom_thumbnail_4.jpg",
            "custom_thumbnail_3.jpg",
            "custom_thumbnail_2.jpg",
            "custom_thumbnail_1.jpg",
            "thumbnail_option_1.jpg",
        ]);
        let pruned = prune_custom_thumbnails(&mut options, Some("custom_thumbnail_1.jpg"));
        assert_eq!(pruned, names(&["custom_thumbnail_2.jpg"]));
        assert_eq!(
            options,
            names(&[
                "custom_thumbnail_5.jpg",
                "custom_thumbnail_4.jpg",
                "custom_thumbnail_3.jpg",
                "custom_thumbnail_1.jpg",
                "thumbnail_option_1.jpg",
            ])
        );
        // Generated options are never pruned.
        let mut options = names(&["thumbnail_option_1.jpg", "thumbnail_option_2.jpg"]);
        assert!(prune_custom_thumbnails(&mut options, None).is_empty());
        assert_eq!(options.len(), 2);
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
