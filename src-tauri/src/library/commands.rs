//! Tauri commands for the library screen, review screen and downloads.

use super::manifest::Review;
use super::runtime::{AppStateHandle, Library, LibraryView, VideoDetail};
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
    selected_thumbnail: Option<String>,
) -> Result<(), String> {
    lib.save_review(&video_id, Review { title, description, preacher }, selected_thumbnail)
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

#[tauri::command]
pub async fn save_rendered_thumbnail(
    lib: State<'_, LibraryHandle>,
    video_id: String,
    image_data: Vec<u8>,
    filename: String,
) -> Result<String, String> {
    lib.save_rendered_thumbnail(&video_id, image_data, filename).await
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
    thumbnail_path: Option<String>,
    privacy: Option<String>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    playlist_id: Option<String>,
) -> Result<String, String> {
    use crate::youtube;

    let job = lib.begin_upload(&video_id, thumbnail_path)?;
    changed(&app_handle);

    let result = async {
        let privacy = privacy.unwrap_or_else(|| "unlisted".to_string());
        let access_token = crate::commands::fresh_access_token(&state, &app_handle).await?;

        let upload_result = youtube::upload_video(
            &access_token,
            &job.video_path.to_string_lossy(),
            &title,
            &description,
            &privacy,
            Some(&app_handle),
        )
        .await?;
        let uploaded_id = upload_result.id.clone();

        if let Some(thumb) = &job.thumbnail_path {
            if let Err(e) = youtube::set_thumbnail(&access_token, &uploaded_id, &thumb.to_string_lossy()).await {
                println!("[youtube] Warning: Thumbnail upload failed: {}", e);
            }
        }

        if let Some(segs) = &job.segments {
            let start = start_time.unwrap_or(0.0);
            let end = end_time.unwrap_or(f64::MAX);
            let srt_content = youtube::generate_srt(segs, start, end);
            if !srt_content.trim().is_empty() {
                if let Err(e) = youtube::upload_captions(&access_token, &uploaded_id, &srt_content).await {
                    println!("[youtube] Warning: Caption upload failed: {}", e);
                }
            }
        }

        // An explicit playlist from the review screen wins over the setting.
        let playlist = match playlist_id.filter(|p| !p.is_empty()) {
            Some(explicit) => Some(explicit),
            None => state.lock().await.settings.youtube_playlist_id.clone(),
        };
        if let Some(playlist_id) = playlist.filter(|p| !p.is_empty()) {
            if let Err(e) = youtube::add_to_playlist(&access_token, &playlist_id, &uploaded_id).await {
                println!("[youtube] Warning: Failed to add to playlist: {}", e);
            }
        }

        Ok(format!("https://youtube.com/watch?v={}", uploaded_id))
    }
    .await;

    lib.finish_upload(&video_id, &job.file_hash, result.as_deref().ok());
    changed(&app_handle);
    result
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
