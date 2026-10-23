//! Tauri commands for the library screen, review screen and downloads.

use super::manifest::Review;
use super::runtime::{AppStateHandle, EntrySession, Library, LibraryView, UploadOptions, VideoDetail};
use crate::upload::client::{video_metadata, VideoUpload};
use crate::upload::{self, AfterUpload, TokenSource, UploadConfig, UploadError, UploadOutcome, UploadWarning};
use std::sync::atomic::{AtomicU32, Ordering};
use crate::thumbnail::CropRect;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

pub type LibraryHandle = Arc<Library>;

async fn view(lib: &Library, state: &AppStateHandle) -> LibraryView {
    let notices = state.lock().await.notices.clone();
    lib.view(notices)
}

fn changed(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
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
    lib.reset(&video_id)?;
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
    lib.forget(&video_id)?;
    changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub async fn save_review(
    lib: State<'_, LibraryHandle>,
    video_id: String,
    title: Option<String>,
    description: Option<String>,
    preacher: Option<String>,
    selected_source: Option<String>,
) -> Result<(), String> {
    lib.save_review(&video_id, Review { title, description, preacher }, selected_source)
}

#[tauri::command]
pub async fn regenerate_thumbnails(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
) -> Result<Vec<String>, String> {
    lib.regenerate_thumbnails(&app_handle, &state, &video_id).await
}

#[tauri::command]
pub async fn process_custom_thumbnail(
    lib: State<'_, LibraryHandle>,
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    source_image_path: String,
    crop_rect: CropRect,
    apply_color_grading: bool,
    apply_logo_overlay: bool,
) -> Result<String, String> {
    lib.custom_thumbnail(
        &app_handle,
        &state,
        &video_id,
        source_image_path,
        crop_rect,
        apply_color_grading,
        apply_logo_overlay,
    )
    .await
}

/// Save a template render (PNG bytes) of one thumbnail source. Resolves
/// with the saved file's path.
#[tauri::command]
pub async fn save_rendered_thumbnail(
    lib: State<'_, LibraryHandle>,
    video_id: String,
    source: String,
    render_key: String,
    image_data: Vec<u8>,
) -> Result<String, String> {
    lib.save_rendered_thumbnail(&video_id, &source, &render_key, image_data).await
}

/// Show the trimmed video (or the original) in Finder.
#[tauri::command]
pub async fn reveal_video(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    let path = lib.reveal_path(&video_id)?;
    reveal(&path)
}

#[tauri::command]
pub async fn open_library_folder(lib: State<'_, LibraryHandle>) -> Result<(), String> {
    let root = lib.root().ok_or("The library folder isn't available")?;
    crate::commands::open_folder(root.to_string_lossy().to_string()).await
}

fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    result.map(|_| ()).map_err(|e| format!("Failed to open Finder: {}", e))
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
    // Privacy and playlist as chosen next to Upload; the settings fill in
    // anything not sent.
    let options = UploadOptions::resolve(&state.lock().await.settings, privacy, playlist_id)?;
    let job = lib.begin_upload(&video_id, thumbnail_source, render_key)?;
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
        let after = AfterUpload {
            thumbnail: job.thumbnail_path.clone(),
            captions: job.segments.as_ref().map(|segs| {
                upload::post::generate_srt(segs, start_time.unwrap_or(0.0), end_time.unwrap_or(f64::MAX))
            }),
            playlist_id: options.playlist_id.clone(),
        };
        let store = EntrySession { lib: library, id: &video_id };
        let last_percent = AtomicU32::new(u32::MAX);
        let on_progress = |sent: u64, total: u64| {
            let percent = (sent.saturating_mul(95) / total.max(1)) as u32;
            if last_percent.swap(percent, Ordering::SeqCst) != percent {
                library.set_upload_progress(&video_id, percent);
                emit_upload(&app_handle, &video_id, percent, "uploading", None, None, None);
            }
        };
        let cfg = UploadConfig::default();
        upload::run(&upload::http_client(), &cfg, &tokens, &store, &job.cancel, &video, &after, &on_progress).await
    }
    .await;

    library.finish_upload(&video_id, &job, result.as_ref().ok(), &options);
    // Final word for any screen showing this upload, including a review
    // screen reopened after the one that started it went away.
    match &result {
        Ok(o) => emit_upload(&app_handle, &video_id, 100, "done", Some(o.url.as_str()), None, Some(&o.warnings)),
        Err(UploadError::Cancelled) => emit_upload(&app_handle, &video_id, 0, "cancelled", None, None, None),
        Err(e) => {
            eprintln!("[upload] Failed: {}", e);
            emit_upload(&app_handle, &video_id, 0, "error", None, Some(e.user_message().as_str()), None)
        }
    }
    changed(&app_handle);
    result.map_err(|e| e.user_message())
}

/// Stop a running upload; the next Upload continues where it stopped.
#[tauri::command]
pub async fn cancel_upload(lib: State<'_, LibraryHandle>, video_id: String) -> Result<(), String> {
    lib.cancel_upload(&video_id)
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
    let (youtube_id, thumbnail) = lib.begin_thumbnail_retry(&video_id)?;
    let result = async {
        let tokens = crate::commands::AppTokens { state: &state, app: &app_handle };
        let endpoints = upload::Endpoints::youtube();
        let http = upload::http_client();
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
    let remaining = lib.finish_thumbnail_retry(&video_id, &youtube_id, result.is_ok());
    changed(&app_handle);
    result.map(|_| remaining).map_err(|e| UploadWarning::thumbnail(&e).message)
}

/// `youtube-upload-progress` event, tagged with the library video.
fn emit_upload(
    app: &AppHandle,
    video_id: &str,
    progress: u32,
    status: &str,
    url: Option<&str>,
    error: Option<&str>,
    warnings: Option<&[UploadWarning]>,
) {
    let _ = app.emit(
        "youtube-upload-progress",
        serde_json::json!({
            "video_id": video_id,
            "progress": progress,
            "status": status,
            "url": url,
            "error": error,
            "warnings": warnings,
        }),
    );
}

/// Warnings about where the library lives, for Settings.
#[tauri::command]
pub async fn check_library_folder(path: String) -> Result<Vec<String>, String> {
    Ok(folder_warnings(Path::new(&path), dirs::home_dir().as_deref()))
}

pub fn folder_warnings(path: &Path, home: Option<&Path>) -> Vec<String> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut warnings = Vec::new();
    let under = |rel: &str| home.is_some_and(|h| path.starts_with(h.join(rel)));

    if under("Library/Mobile Documents") {
        warnings.push("This is in iCloud Drive. iCloud can swap videos for online-only placeholders and create conflicting copies of the library's tracking file. A local folder such as Movies is safer.".into());
    } else if under("Library/CloudStorage") || under("Dropbox") || under("Google Drive") || under("OneDrive") {
        warnings.push("This is a cloud folder (Dropbox, Google Drive or OneDrive). Videos may be online-only placeholders until they download, and two computers using it at once can create conflicting copies.".into());
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
    use super::*;

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
