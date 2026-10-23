//! Tauri commands for the library screen, review screen and downloads.

use super::manifest::Review;
use super::runtime::{AppStateHandle, EntrySession, Library, LibraryView, UploadOptions, VideoDetail};
use crate::upload::client::{video_metadata, VideoUpload};
use crate::upload::{self, AfterUpload, Thumbnail, TokenSource, UploadConfig, UploadError, UploadOutcome, UploadWarning};
use std::sync::atomic::{AtomicU32, Ordering};
use crate::thumbnail::CropRect;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

pub type LibraryHandle = Arc<Library>;

async fn view(lib: &Library, state: &AppStateHandle) -> LibraryView {
    let notices = state.lock().await.notices.clone();
    lib.view(notices)
}

/// Run a blocking library action (one that touches files) off the async
/// workers.
async fn off_thread<T: Send + 'static>(
    lib: &LibraryHandle,
    f: impl FnOnce(&Library) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let lib = lib.clone();
    tauri::async_runtime::spawn_blocking(move || f(&lib))
        .await
        .map_err(|e| format!("Background task failed: {}", e))?
}

fn changed(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
    crate::notify::refresh_badge(app);
}

#[tauri::command]
pub async fn get_library(lib: State<'_, LibraryHandle>, state: State<'_, AppStateHandle>) -> Result<LibraryView, String> {
    Ok(view(&lib, &state).await)
}

/// List the folder again and return the updated library.
#[tauri::command]
pub async fn rescan_library(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<LibraryView, String> {
    lib.rescan(&state).await?;
    crate::commands::allow_library_paths(&app_handle, lib.root().as_deref());
    crate::notify::refresh_badge(&app_handle);
    Ok(view(&lib, &state).await)
}

#[tauri::command]
pub async fn get_video(lib: State<'_, LibraryHandle>, video_id: String) -> Result<VideoDetail, String> {
    lib.detail(&video_id)
}

#[tauri::command]
pub async fn process_video(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    lib.enqueue(&video_id).await?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn cancel_processing(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    lib.cancel(&video_id)?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn reset_video(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    off_thread(&lib, move |lib| lib.reset(&video_id)).await?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn hide_video(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String, hidden: bool) -> Result<(), String> {
    lib.set_hidden(&video_id, hidden)?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn trash_video(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    let result = lib.trash(&video_id).await;
    changed(&app_handle);
    result
}

#[tauri::command]
pub async fn forget_video(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    off_thread(&lib, move |lib| lib.forget(&video_id)).await?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn save_review(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    video_id: String,
    title: Option<String>,
    description: Option<String>,
    preacher: Option<String>,
    single_line: Option<bool>,
    selected_source: Option<String>,
) -> Result<(), String> {
    let named = preacher.clone().unwrap_or_default();
    let review = Review { title, description, preacher, single_line: single_line.unwrap_or(false) };
    let id = video_id.clone();
    off_thread(&lib, move |lib| lib.save_review(&id, review, selected_source)).await?;
    // Offered on the next video's review screen.
    let mut s = state.lock().await;
    if s.note_preacher(&video_id, &named) {
        if let Err(e) = s.save() {
            eprintln!("[warn] Failed to save state: {}", e);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn regenerate_thumbnails(
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    video_id: String,
) -> Result<Vec<String>, String> {
    let result = lib.regenerate_thumbnails(&video_id).await;
    changed(&app_handle);
    result
}

#[tauri::command]
pub async fn process_custom_thumbnail(
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    video_id: String,
    source_image_path: String,
    crop_rect: CropRect,
    apply_color_grading: bool,
) -> Result<String, String> {
    // A picked image (the dialog adds picks to the fs scope) or one of the
    // video's own.
    let check = |p: &str| crate::commands::check_path(&app_handle, &lib, p, crate::commands::Access::Data);
    let result = custom_thumbnail_checked(&lib, check, &video_id, &source_image_path, crop_rect, apply_color_grading).await;
    changed(&app_handle);
    result
}

/// `process_custom_thumbnail`, with `check` deciding whether the source may
/// be read (and where it really is).
pub(crate) async fn custom_thumbnail_checked(
    lib: &Arc<Library>,
    check: impl FnOnce(&str) -> Result<PathBuf, String>,
    video_id: &str,
    source_image_path: &str,
    crop_rect: CropRect,
    apply_color_grading: bool,
) -> Result<String, String> {
    let source = check(source_image_path)?;
    lib.custom_thumbnail(video_id, source, crop_rect, apply_color_grading).await
}

/// Cut the video again at new points; resolves with where the cut really
/// starts and ends. Progress comes as `trim-progress` events.
#[tauri::command]
pub async fn set_trim(
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    video_id: String,
    start: f64,
    end: f64,
) -> Result<TrimResult, String> {
    let (start, end) = lib.set_trim(&app_handle, &video_id, start, end).await?;
    Ok(TrimResult { start, end })
}

#[derive(serde::Serialize)]
pub struct TrimResult {
    pub start: f64,
    pub end: f64,
}

/// A still of the original at `time` (seconds), for when it can't play.
#[tauri::command]
pub async fn frame_still(
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    video_id: String,
    time: f64,
) -> Result<String, String> {
    let path = lib.frame_still(&video_id, time).await?;
    let _ = tauri::Manager::asset_protocol_scope(&app_handle).allow_file(&path);
    Ok(path)
}

/// Save a template render of one thumbnail source. The JPEG comes as the
/// raw invoke body (no JSON number array); the video, source and render key
/// come as percent-encoded headers. Resolves with the saved file's path.
#[tauri::command]
pub async fn save_rendered_thumbnail(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<String, String> {
    let video_id = header(request.headers(), "x-video-id")?;
    let source = header(request.headers(), "x-source")?;
    let render_key = header(request.headers(), "x-render-key")?;
    let template_version = header(request.headers(), "x-template-version")?
        .parse::<u32>()
        .map_err(|_| "Invalid x-template-version header".to_string())?;
    let bytes = raw_image(request.body())?;
    // The logo is drawn too, so it is part of the key.
    let logo = state.lock().await.settings.logo_path.clone().unwrap_or_default();
    let result = lib.save_rendered_thumbnail(&video_id, &source, &render_key, template_version, &logo, bytes).await;
    // The card shows the selected render.
    changed(&app_handle);
    result
}

/// Largest image accepted over IPC.
const MAX_RAW_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// A raw invoke body holding an image.
pub fn raw_image(body: &tauri::ipc::InvokeBody) -> Result<Vec<u8>, String> {
    match body {
        tauri::ipc::InvokeBody::Raw(bytes) if bytes.is_empty() => Err("The image is empty".into()),
        tauri::ipc::InvokeBody::Raw(bytes) if bytes.len() > MAX_RAW_IMAGE_BYTES => Err("The image is too large".into()),
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        tauri::ipc::InvokeBody::Json(_) => Err("Expected the image as raw bytes".into()),
    }
}

/// A required header: visible ASCII only, at most 512 bytes, then
/// percent-decoded to UTF-8. Anything else is refused, never guessed at.
pub fn header(headers: &tauri::http::HeaderMap, name: &str) -> Result<String, String> {
    let value = headers.get(name).ok_or_else(|| format!("Missing {} header", name))?;
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 512 || !bytes.iter().all(|b| b.is_ascii_graphic()) {
        return Err(format!("Invalid {} header", name));
    }
    let ascii = std::str::from_utf8(bytes).map_err(|_| format!("Invalid {} header", name))?;
    let decoded = urlencoding::decode(ascii).map_err(|_| format!("Invalid {} header", name))?;
    if decoded.is_empty() || decoded.chars().any(char::is_control) {
        return Err(format!("Invalid {} header", name));
    }
    Ok(decoded.into_owned())
}

/// Show the trimmed video (or the original) in Finder.
#[tauri::command]
pub async fn reveal_video(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    let path = lib.reveal_path(&video_id)?;
    reveal(&path)
}

/// Forget an earlier upload attempt that can't be checked with YouTube, so
/// the next upload starts afresh (the webview confirms first: it may make a
/// duplicate).
#[tauri::command]
pub async fn forget_upload_session(lib: State<'_, LibraryHandle>, app_handle: AppHandle, video_id: String) -> Result<(), String> {
    let result = off_thread(&lib, move |lib| lib.forget_upload_session(&video_id)).await;
    changed(&app_handle);
    result
}

/// Show a video's unfinished commit backup in Finder (see `STUCK_COMMIT`).
#[tauri::command]
pub async fn reveal_commit_backup(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    let path = lib.commit_backup_path(&video_id)?;
    reveal(&path)
}

#[tauri::command]
pub async fn open_library_folder(lib: State<'_, LibraryHandle>) -> Result<(), String> {
    let root = lib.root().ok_or("The library folder isn't available")?;
    crate::commands::open_folder_path(root.to_string_lossy().to_string())
}

fn reveal(path: &Path) -> Result<(), String> {
    crate::tools::os_open(crate::tools::OsOpen::Reveal(path))
}

#[tauri::command]
pub async fn dismiss_notices(state: State<'_, AppStateHandle>) -> Result<(), String> {
    let mut s = state.lock().await;
    s.notices.clear();
    s.save()
}

#[tauri::command]
pub async fn take_over_library(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<LibraryView, String> {
    lib.take_over().await?;
    lib.rescan(&state).await?;
    changed(&app_handle);
    Ok(view(&lib, &state).await)
}

#[tauri::command]
pub async fn clear_library_data(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<(), String> {
    lib.clear_data().await?;
    lib.rescan(&state).await?;
    changed(&app_handle);
    Ok(())
}

/// Download a YouTube video into the library. With `process`, queue it
/// right away (the old "Download & Process").
#[tauri::command]
pub async fn download_video(
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    url: String,
    quality: Option<String>,
    process: Option<bool>,
) -> Result<String, String> {
    let quality = quality.unwrap_or_else(|| "best".to_string());
    let id = lib.download(&app_handle, &url, &quality).await?;
    if process.unwrap_or(false) {
        lib.enqueue(&id)
            .await
            .map_err(|e| format!("The video was downloaded to the library, but processing couldn't start: {}", e))?;
        changed(&app_handle);
    }
    Ok(id)
}

/// Called before an app update installs. Refused while anything is running;
/// otherwise nothing new starts until the relaunch (or `end_update`).
#[tauri::command]
pub async fn begin_update(lib: State<'_, LibraryHandle>) -> Result<(), String> {
    lib.begin_update()
}

/// The update didn't install: allow work again.
#[tauri::command]
pub async fn end_update(lib: State<'_, LibraryHandle>) -> Result<(), String> {
    lib.end_update();
    Ok(())
}

#[tauri::command]
pub async fn cancel_download(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    lib.cancel_download(&video_id);
    Ok(())
}

#[tauri::command]
pub async fn youtube_upload_video(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    title: String,
    description: String,
    thumbnail_source: Option<String>,
    render_key: Option<String>,
    privacy: Option<String>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    playlist_id: Option<String>,
) -> Result<UploadOutcome, String> {
    check_title(&title)?;
    // Privacy and playlist as chosen next to Upload; the settings fill in
    // anything not sent.
    let options = UploadOptions::resolve(&state.lock().await.settings, privacy, playlist_id)?;
    let mut job = lib.begin_upload(&video_id, thumbnail_source, render_key)?;
    changed(&app_handle);
    let library: &Library = &lib;

    let result: Result<UploadOutcome, UploadError> = async {
        let tokens = crate::commands::AppTokens { state: &state, app: &app_handle };
        // Signed out or revoked: say so before anything else.
        tokens.token().await?;

        // The upload is starting: offer the same privacy next time.
        {
            let mut s = state.lock().await;
            if s.settings.upload_privacy != options.privacy {
                s.settings.upload_privacy = options.privacy.clone();
                if let Err(e) = s.save() {
                    eprintln!("[warn] Failed to save state: {}", e);
                }
            }
        }

        let video = VideoUpload {
            path: &job.video_path,
            file_hash: &job.file_hash,
            metadata: video_metadata(&title, &description, &options.privacy),
        };
        let thumbnail = match (job.thumbnail_path.clone(), job.thumbnail_data.take()) {
            (Some(path), Some(data)) => Some(
                tokio::task::spawn_blocking(move || Thumbnail::prepare(data, &path))
                    .await
                    .map_err(|e| UploadError::File(e.to_string()))?,
            ),
            _ => None,
        };
        let after = AfterUpload {
            thumbnail,
            captions: job.segments.as_ref().map(|segs| {
                upload::post::generate_srt(segs, start_time.unwrap_or(0.0), end_time.unwrap_or(f64::MAX))
            }),
            playlist_id: options.playlist_id.clone(),
        };
        let store = EntrySession { lib: library, id: &video_id, options: &options };
        let last_percent = AtomicU32::new(u32::MAX);
        let on_progress = |sent: u64, total: u64| {
            let percent = (sent.saturating_mul(95) / total.max(1)) as u32;
            if last_percent.swap(percent, Ordering::SeqCst) != percent {
                library.set_upload_progress(&video_id, percent);
                emit_upload(&app_handle, &video_id, percent, "uploading", None, None);
            }
        };
        let cfg = UploadConfig::default();
        upload::run(&upload::http_client(), &cfg, &tokens, &store, &job.cancel, &video, &after, &on_progress).await
    }
    .await;

    library.finish_upload(&video_id, job, result.as_ref().ok(), &options);
    let shown_title = library.title_of(&video_id);
    match &result {
        Ok(_) => crate::notify::notify(&app_handle, crate::notify::Event::Uploaded { title: &shown_title }),
        Err(UploadError::Cancelled | UploadError::CancelledMaybeFinished) => {}
        Err(e) => crate::notify::notify(
            &app_handle,
            crate::notify::Event::UploadFailed { title: &shown_title, error: &e.user_message() },
        ),
    }
    // Final word for any screen showing this upload, including a review
    // screen reopened after the one that started it went away.
    match &result {
        Ok(o) => emit_upload(&app_handle, &video_id, 100, "done", Some(o), None),
        Err(e @ (UploadError::Cancelled | UploadError::CancelledMaybeFinished)) => {
            emit_upload(&app_handle, &video_id, 0, "cancelled", None, Some(e.user_message().as_str()))
        }
        Err(e) => {
            eprintln!("[upload] Failed: {}", e);
            emit_upload(&app_handle, &video_id, 0, "error", None, Some(e.user_message().as_str()))
        }
    }
    changed(&app_handle);
    result.map_err(|e| e.user_message())
}

/// YouTube refuses a video without a title (and the webview disables
/// Upload then); refused here too, before anything starts.
fn check_title(title: &str) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("Add a title before uploading.".into());
    }
    Ok(())
}

/// Stop a running upload; the next Upload continues where it stopped.
#[tauri::command]
pub async fn cancel_upload(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    lib.cancel_upload(&video_id)
}

/// Give a finished upload of this video (an earlier one, found when a new
/// upload began) the current title and description, via `videos.update`.
#[tauri::command]
pub async fn youtube_update_metadata(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    youtube_id: String,
    title: String,
    description: String,
) -> Result<(), String> {
    check_title(&title)?;
    if !lib.has_upload(&video_id, &youtube_id) {
        return Err("That YouTube video isn't one of this video's uploads.".into());
    }
    let tokens = crate::commands::AppTokens { state: &state, app: &app_handle };
    upload::post::update_metadata_authed(&upload::http_client(), &upload::Endpoints::youtube(), &tokens, &youtube_id, &title, &description)
        .await
        .map_err(|e| e.user_message())
}

/// "Set thumbnail again" for the last upload, by its stored YouTube id.
/// Returns that upload's remaining warnings.
#[tauri::command]
pub async fn retry_thumbnail(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
) -> Result<Vec<UploadWarning>, String> {
    let (youtube_id, thumbnail, busy) = lib.begin_thumbnail_retry(&video_id)?;
    let result = async {
        let tokens = crate::commands::AppTokens { state: &state, app: &app_handle };
        let endpoints = upload::Endpoints::youtube();
        let http = upload::http_client();
        let path = thumbnail.clone();
        let thumbnail = tokio::task::spawn_blocking(move || Thumbnail::read(&path))
            .await
            .map_err(|e| UploadError::File(e.to_string()))??;
        let token = tokens.token().await?;
        match upload::post::set_thumbnail(&http, &endpoints, &token, &youtube_id, &thumbnail).await {
            Err(UploadError::Auth(_)) => {
                let token = tokens.refresh().await?;
                upload::post::set_thumbnail(&http, &endpoints, &token, &youtube_id, &thumbnail).await
            }
            r => r,
        }
    }
    .await;
    let remaining = lib.finish_thumbnail_retry(busy, &video_id, &youtube_id, result.is_ok());
    changed(&app_handle);
    result.map(|_| remaining).map_err(|e| UploadWarning::thumbnail(&e).message)
}

/// `youtube-upload-progress` event, tagged with the library video.
fn emit_upload(
    app: &AppHandle,
    video_id: &str,
    progress: u32,
    status: &str,
    outcome: Option<&UploadOutcome>,
    error: Option<&str>,
) {
    let _ = app.emit(
        "youtube-upload-progress",
        serde_json::json!({
            "video_id": video_id,
            "progress": progress,
            "status": status,
            "url": outcome.map(|o| &o.url),
            "youtube_id": outcome.map(|o| &o.video_id),
            "error": error,
            "warnings": outcome.map(|o| &o.warnings),
            "earlier": outcome.and_then(|o| o.earlier.as_ref()),
            "before_cancel": outcome.is_some_and(|o| o.before_cancel),
        }),
    );
}

/// Warnings about where the library lives, for Settings.
#[tauri::command]
pub async fn check_library_folder(path: String) -> Result<Vec<String>, String> {
    Ok(folder_warnings(Path::new(&path), dirs::home_dir().as_deref()))
}

/// Shown in Settings for iCloud Drive, Dropbox, Google Drive and OneDrive.
pub const CLOUD_WARNING: &str = "Cloud folders can offload videos to save space — keep this folder downloaded (in Finder, choose “Keep Downloaded”). Two computers using it at once can also create conflicting copies of the library's tracking file.";

/// Why a folder can't become the library, if it can't. Only for a newly
/// chosen folder: a library already set up somewhere refused here keeps
/// opening. `app_dirs` are the app's own folders (settings, tools), which
/// the library may neither be inside nor contain.
pub fn library_folder_refusal(folder: &Path, home: Option<&Path>, app_dirs: &[PathBuf]) -> Option<String> {
    let Some(path) = super::fsutil::resolve(folder) else {
        return Some("Choose the library folder with Change…, or type its full path (such as /Users/you/Movies/Sermon Cut).".into());
    };
    let whole = |what: &str| {
        Some(format!(
            "{} can't be the library folder: Sermon Cut would treat everything in it as its own. Choose or make a folder inside it, such as Movies/Sermon Cut.",
            what
        ))
    };
    let system = |what: &str| Some(format!("{} belongs to macOS or its apps and can't hold the library. Choose a folder such as Movies/Sermon Cut.", what));
    let is = |p: &str| path == Path::new(p);
    if is("/") {
        return whole("The whole disk");
    }
    if is("/Volumes") {
        return whole("/Volumes (every connected drive)");
    }
    if is("/Users") {
        return whole("/Users (every account's home folder)");
    }
    if path.starts_with("/System") || path.starts_with("/Applications") {
        return system(&path.display().to_string());
    }
    for dir in app_dirs.iter().filter_map(|d| super::fsutil::resolve(d)) {
        if path.starts_with(&dir) || dir.starts_with(&path) {
            return Some(format!(
                "{} holds Sermon Cut's own settings or tools, so it can't be the library folder. Choose a folder such as Movies/Sermon Cut.",
                path.display()
            ));
        }
    }
    if let Some(home) = home.and_then(super::fsutil::resolve) {
        if path == home {
            return whole("Your home folder");
        }
        let library = home.join("Library");
        // iCloud Drive, Dropbox, Google Drive…: a folder inside one is fine
        // (with a warning), the cloud folder list itself isn't.
        let in_cloud = ["CloudStorage", "Mobile Documents"]
            .iter()
            .any(|c| path.strip_prefix(library.join(c)).is_ok_and(|rest| rest.components().next().is_some()));
        if path.starts_with(&library) && !in_cloud {
            return system("Your Library folder");
        }
    }
    None
}

/// `library_folder_refusal` with this Mac's folders, as an error.
pub fn check_new_library_folder(folder: &str) -> Result<(), String> {
    let mut app_dirs = vec![crate::state::state_dir()];
    app_dirs.extend(crate::tools::tools_dir());
    match library_folder_refusal(Path::new(folder), dirs::home_dir().as_deref(), &app_dirs) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

pub fn folder_warnings(path: &Path, home: Option<&Path>) -> Vec<String> {
    let path = super::fsutil::resolve(path).unwrap_or_else(|| path.to_path_buf());
    let home = home.map(|h| super::fsutil::resolve(h).unwrap_or_else(|| h.to_path_buf()));
    let mut warnings = Vec::new();
    let under = |rel: &str| home.as_ref().is_some_and(|h| path.starts_with(h.join(rel)));

    if under("Library/Mobile Documents") || under("Library/CloudStorage") || under("Dropbox") || under("Google Drive") || under("OneDrive") {
        warnings.push(CLOUD_WARNING.into());
    }
    if cfg!(target_os = "macos") && (under("Desktop") || under("Documents") || under("Downloads")) {
        warnings.push("macOS asks permission before apps can use Desktop, Documents or Downloads, and may ask again after each Sermon Cut update. Movies doesn't have this problem.".into());
    }
    if cfg!(target_os = "macos") && path.starts_with("/Volumes") {
        warnings.push("This is on an external or network drive. The library is unavailable while it's disconnected, and macOS may ask for permission again after updates.".into());
    }
    warnings
}

#[cfg(test)]
mod tests {
    use tauri::http::{HeaderMap, HeaderValue};

    #[test]
    fn an_upload_needs_a_title() {
        assert!(super::check_title("").is_err());
        assert!(super::check_title(" \n\t").is_err());
        assert!(super::check_title("Hope").is_ok());
    }
    use tauri::ipc::InvokeBody;

    fn headers(pairs: &[(&'static str, &[u8])]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_bytes(v).unwrap());
        }
        h
    }

    #[test]
    fn render_headers_are_ascii_and_percent_decoded() {
        let h = headers(&[("x-source", b"custom_thumbnail_1.jpg"), ("x-video-id", b"Caf%C3%A9%20%27x%27")]);
        assert_eq!(header(&h, "x-source").unwrap(), "custom_thumbnail_1.jpg");
        assert_eq!(header(&h, "x-video-id").unwrap(), "Caf\u{e9} 'x'");
    }

    #[test]
    fn bad_render_headers_are_refused() {
        // Missing, empty, raw non-ASCII, spaces, control characters (raw
        // or encoded), bad UTF-8 once decoded, and overlong.
        assert!(header(&headers(&[]), "x-source").is_err());
        let long = vec![b'a'; 513];
        for bad in [
            &b""[..],
            "caf\u{e9}".as_bytes(),
            b"a b",
            b"a\tb",
            b"a%0Ab",
            b"%FF%FE",
            &long[..],
        ] {
            let h = headers(&[("x-source", bad)]);
            assert!(header(&h, "x-source").is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn render_body_must_be_raw_bytes() {
        assert_eq!(raw_image(&InvokeBody::Raw(vec![1, 2, 3])).unwrap(), vec![1, 2, 3]);
        assert!(raw_image(&InvokeBody::Raw(vec![])).is_err());
        assert!(raw_image(&InvokeBody::Json(serde_json::json!({"imageData": [1, 2]}))).is_err());
    }

    use super::*;

    #[test]
    fn new_library_folders_follow_the_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let lib = home.join("Library");
        let state = lib.join("Application Support/sermon-cut");
        let tools = lib.join("Application Support/com.youthscc.sermon-cut/bin");
        for d in [&state, &tools, &home.join("Movies"), &lib.join("CloudStorage/Dropbox"), &lib.join("Mobile Documents/com~apple~CloudDocs")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let app_dirs = vec![state.clone(), tools.clone()];
        let refused = |p: &Path| library_folder_refusal(p, Some(&home), &app_dirs);

        for bad in [
            PathBuf::from("/"),
            PathBuf::from("/Volumes"),
            PathBuf::from("/Users"),
            PathBuf::from("/System/Library"),
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Sermon Cut.app"),
            home.clone(),
            lib.clone(),
            lib.join("Caches/x"),
            lib.join("CloudStorage"),
            lib.join("Mobile Documents"),
            state.clone(),
            state.join("library"),
            tools.parent().unwrap().to_path_buf(),
            PathBuf::from("relative/folder"),
        ] {
            assert!(refused(&bad).is_some(), "{} should be refused", bad.display());
        }
        for good in [
            home.join("Movies/Sermon Cut"),
            home.join("Movies"),
            PathBuf::from("/Volumes/USB"),
            PathBuf::from("/Volumes/USB/Sermons"),
            lib.join("CloudStorage/Dropbox/Sermons"),
            lib.join("CloudStorage/Dropbox"),
            lib.join("Mobile Documents/com~apple~CloudDocs/Sermons"),
        ] {
            assert_eq!(refused(&good), None, "{} should be allowed", good.display());
        }
        // Allowed cloud folders come with the warning.
        let warnings = folder_warnings(&lib.join("CloudStorage/Dropbox/Sermons"), Some(&home));
        assert_eq!(warnings, vec![CLOUD_WARNING.to_string()]);
    }

    #[test]
    fn a_library_set_up_in_a_now_refused_folder_still_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        assert!(library_folder_refusal(&home, Some(&home), &[]).is_some());
        let lib = Library::new(home.to_string_lossy().to_string());
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(lib.reopen()).unwrap();
        let view = lib.view(vec![]);
        assert!(view.available && view.writable, "{:?}", view.message);
    }

    #[test]
    fn warns_about_risky_folders() {
        let home = Path::new("/Users/test");
        assert!(folder_warnings(Path::new("/Users/test/Movies/sermon-cut"), Some(home)).is_empty());
        assert_eq!(folder_warnings(Path::new("/Users/test/Library/Mobile Documents/com~apple~CloudDocs/x"), Some(home)).len(), 1);
        assert_eq!(folder_warnings(Path::new("/Users/test/Library/CloudStorage/Dropbox/x"), Some(home)).len(), 1);
        if cfg!(target_os = "macos") {
            assert_eq!(folder_warnings(Path::new("/Users/test/Desktop/x"), Some(home)).len(), 1);
            assert_eq!(folder_warnings(Path::new("/Volumes/USB/x"), Some(home)).len(), 1);
        }
    }
}
