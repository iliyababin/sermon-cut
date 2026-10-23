use crate::library::commands::LibraryHandle;
use crate::state::{AppState, Settings, SettingsPatch, SettingsView, VideoInfo};
use crate::upload::{TokenSource, UploadError};
use crate::youtube_dl;
use futures::future::BoxFuture;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

type AppStateHandle = Arc<Mutex<AppState>>;

/// Let the webview load thumbnails from the library and the logo. The
/// library is chosen at runtime, so its scope is added here rather than in
/// tauri.conf.json. `.sermon-cut` needs its own rule: scope globs don't
/// match hidden folders.
pub fn allow_user_paths(app_handle: &AppHandle, library_root: Option<&Path>, settings: &Settings) {
    allow_library_paths(app_handle, library_root);
    let scope = app_handle.asset_protocol_scope();
    if let Some(logo) = settings.logo_path.as_deref().filter(|p| !p.is_empty()) {
        let _ = scope.allow_file(logo);
    }
}

#[tauri::command]
pub async fn get_video_info(url: String) -> Result<VideoInfo, String> {
    youtube_dl::get_video_info(&url).await
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppStateHandle>) -> Result<SettingsView, String> {
    Ok(state.lock().await.settings.view())
}

/// Apply the edited settings. `seq` increases with every save the webview
/// makes; a save older than one already applied is ignored. Returns the
/// settings as they now are.
#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppStateHandle>,
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    patch: SettingsPatch,
    seq: u64,
) -> Result<SettingsView, String> {
    let library = lib.inner().clone();
    let result = apply_settings_patch(
        &state,
        patch,
        seq,
        |folder| lib.check_switch(folder),
        |folder| async move { library.switch(folder).await },
    )
    .await;
    let settings = state.lock().await.settings.clone();
    allow_user_paths(&app_handle, lib.root().as_deref(), &settings);
    let _ = tauri::Emitter::emit(&app_handle, "library-changed", ());
    result
}

/// The settings save, apart from Tauri. A new library folder is checked
/// first, saved, and only then switched to; if the switch fails the old
/// folder is saved back, so state.json never points at a folder the app
/// isn't using.
pub async fn apply_settings_patch<C, S, F>(
    state: &AppStateHandle,
    patch: SettingsPatch,
    seq: u64,
    check_switch: C,
    switch: S,
) -> Result<SettingsView, String>
where
    C: FnOnce(&str) -> Result<(), String>,
    S: FnOnce(String) -> F,
    F: std::future::Future<Output = Result<(), String>>,
{
    let (old_folder, new_folder) = {
        let mut s = state.lock().await;
        if seq <= s.settings_seq {
            return Ok(s.settings.view());
        }
        s.settings_seq = seq;

        let mut next = s.settings.clone();
        next.apply(&patch);
        let new_folder = patch
            .output_folder
            .as_ref()
            .map(|f| f.trim().to_string())
            .filter(|f| *f != s.settings.output_folder);
        if let Some(folder) = &new_folder {
            next.output_folder = folder.clone();
            check_switch(&next.library_folder())?;
        }
        let previous = std::mem::replace(&mut s.settings, next);
        if let Err(e) = s.save() {
            s.settings = previous;
            return Err(e);
        }
        (previous.output_folder, new_folder.map(|_| s.settings.library_folder()))
    };

    if let Some(target) = new_folder {
        if let Err(e) = switch(target).await {
            let mut s = state.lock().await;
            s.settings.output_folder = old_folder;
            if let Err(save_err) = s.save() {
                eprintln!("[settings] Failed to restore the library folder: {}", save_err);
            }
            return Err(e);
        }
    }
    Ok(state.lock().await.settings.view())
}

/// Reset settings to their defaults (signs out of YouTube). Never touches
/// any videos or the library's data.
#[tauri::command]
pub async fn reset_state(
    state: State<'_, AppStateHandle>,
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = Settings::default();
    lib.switch(settings.library_folder()).await?;
    allow_user_paths(&app_handle, lib.root().as_deref(), &settings);
    let mut state = state.lock().await;
    state.settings = settings;
    let result = state.save();
    drop(state);
    let _ = tauri::Emitter::emit(&app_handle, "library-changed", ());
    result
}

#[tauri::command]
pub async fn get_app_data_dir() -> Result<String, String> {
    dirs::data_dir()
        .map(|p| p.join("sermon-cut").to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine app data directory".to_string())
}

#[tauri::command]
pub async fn read_image_base64(path: String) -> Result<String, String> {
    use base64::Engine;

    let bytes = std::fs::read(&path)
        .map_err(|e| format!("Failed to read image: {}", e))?;

    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    // Detect mime type from extension
    let mime = if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    };

    Ok(format!("data:{};base64,{}", mime, base64))
}

#[tauri::command]
pub async fn open_file(path: String) -> Result<(), String> {
    println!("[open_file] Opening file: {}", path);

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path])
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub async fn open_folder(path: String) -> Result<(), String> {
    println!("[open_folder] Requested path: {}", path);

    let folder_path = std::path::Path::new(&path);

    // Get parent directory if path is a file
    let folder = if folder_path.is_file() {
        folder_path.parent().unwrap_or(folder_path)
    } else {
        folder_path
    };

    println!("[open_folder] Opening folder: {}", folder.display());

    #[cfg(target_os = "linux")]
    {
        let result = std::process::Command::new("xdg-open")
            .arg(folder)
            .spawn();

        match result {
            Ok(_) => println!("[open_folder] xdg-open spawned successfully"),
            Err(e) => {
                println!("[open_folder] xdg-open failed: {}", e);
                return Err(format!("Failed to open folder: {}", e));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(folder)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(folder)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

/// Safe to call repeatedly (e.g. after every scan, in case the library's
/// drive was plugged in after launch).
pub fn allow_library_paths(app_handle: &AppHandle, library_root: Option<&Path>) {
    let scope = app_handle.asset_protocol_scope();
    if let Some(root) = library_root {
        let _ = scope.allow_directory(root, false);
        let _ = scope.allow_directory(root.join(crate::library::DATA_DIR), true);
    }
}

// YouTube commands

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct YouTubeAuthStatus {
    pub is_authenticated: bool,
    pub channel_name: Option<String>,
    pub channel_id: Option<String>,
}

#[tauri::command]
pub async fn youtube_get_auth_status(
    state: State<'_, AppStateHandle>,
) -> Result<YouTubeAuthStatus, String> {
    let state = state.lock().await;

    if let Some(ref auth) = state.settings.youtube_auth {
        if auth.access_token.is_some() || auth.refresh_token.is_some() {
            return Ok(YouTubeAuthStatus {
                is_authenticated: true,
                channel_name: auth.channel_name.clone(),
                channel_id: auth.channel_id.clone(),
            });
        }
    }

    Ok(YouTubeAuthStatus {
        is_authenticated: false,
        channel_name: None,
        channel_id: None,
    })
}

#[tauri::command]
pub async fn youtube_sign_in(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<YouTubeAuthStatus, String> {
    use crate::youtube::{self, YouTubeCredentials};
    use crate::state::YouTubeAuth;

    // Load credentials
    let credentials = YouTubeCredentials::load_from_resource(&app_handle)?;

    // Start OAuth flow (blocks until user completes auth)
    let tokens = youtube::start_oauth_flow(&credentials).await?;

    // Get channel info
    let channel_info = youtube::get_channel_info(&tokens.access_token).await?;

    // Save tokens to state
    let auth = YouTubeAuth {
        access_token: Some(tokens.access_token),
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
        channel_name: Some(channel_info.title.clone()),
        channel_id: Some(channel_info.id.clone()),
    };

    {
        let mut state = state.lock().await;
        state.settings.youtube_auth = Some(auth);
        if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(YouTubeAuthStatus {
        is_authenticated: true,
        channel_name: Some(channel_info.title),
        channel_id: Some(channel_info.id),
    })
}

#[tauri::command]
pub async fn youtube_sign_out(
    state: State<'_, AppStateHandle>,
) -> Result<(), String> {
    let mut state = state.lock().await;
    state.settings.youtube_auth = None;
    state.save()
}

/// A valid YouTube access token, refreshing (and saving) it if needed.
pub async fn fresh_access_token(state: &AppStateHandle, app_handle: &AppHandle) -> Result<String, String> {
    access_token(state, app_handle, false).await.map_err(|e| e.user_message())
}

/// The access token, refreshed when it's about to expire or `force` is set
/// (after a 401). With no refresh token to renew it, the user has to sign
/// in again.
pub async fn access_token(state: &AppStateHandle, app_handle: &AppHandle, force: bool) -> Result<String, UploadError> {
    use crate::youtube::{self, YouTubeCredentials};

    let (access_token, refresh_token, needs_refresh) = {
        let state_guard = state.lock().await;
        let auth = state_guard
            .settings
            .youtube_auth
            .as_ref()
            .ok_or_else(|| UploadError::Auth("not signed in".into()))?;

        let access_token = auth.access_token.clone();
        let refresh_token = auth.refresh_token.clone();
        let needs_refresh = force
            || access_token.is_none()
            || auth.expires_at
                .map(|exp| exp - chrono::Utc::now().timestamp() < 300)
                .unwrap_or(true);

        (access_token.unwrap_or_default(), refresh_token, needs_refresh)
    };

    if !needs_refresh {
        return Ok(access_token);
    }
    let Some(refresh) = refresh_token else {
        return Err(UploadError::Auth("no refresh token".into()));
    };

    let credentials = YouTubeCredentials::load_from_resource(app_handle).map_err(UploadError::Other)?;
    let new_tokens = youtube::refresh_access_token(&credentials, &refresh).await?;
    {
        let mut state_guard = state.lock().await;
        if let Some(ref mut auth) = state_guard.settings.youtube_auth {
            auth.access_token = Some(new_tokens.access_token.clone());
            auth.expires_at = new_tokens.expires_at;
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }
    Ok(new_tokens.access_token)
}

/// Tokens for the upload code, from the signed-in account.
pub struct AppTokens<'a> {
    pub state: &'a AppStateHandle,
    pub app: &'a AppHandle,
}

impl TokenSource for AppTokens<'_> {
    fn token(&self) -> BoxFuture<'_, Result<String, UploadError>> {
        Box::pin(access_token(self.state, self.app, false))
    }
    fn refresh(&self) -> BoxFuture<'_, Result<String, UploadError>> {
        Box::pin(access_token(self.state, self.app, true))
    }
}

#[tauri::command]
pub async fn youtube_list_playlists(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<Vec<crate::youtube::PlaylistInfo>, String> {
    let access_token = fresh_access_token(&state, &app_handle).await?;
    crate::youtube::list_playlists(&access_token).await
}

/// Working folder for the standalone Thumbnail tab, in the app's data folder
/// (covered by the `$APPDATA/**` asset scope), so it works without a library.
fn standalone_dir(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Can't find the app data folder: {}", e))?
        .join("standalone-thumbnails");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// Crop an arbitrary image for the standalone Thumbnail tab.
///
/// The library-backed `process_custom_thumbnail` needs a real video id; this
/// one works on any file the user picks. Grading and the logo overlay are
/// skipped on purpose — the template system draws those in the webview.
/// Only the latest result matters ("Save As" copies it out), so earlier
/// files are cleared first.
#[tauri::command]
pub async fn crop_standalone_thumbnail(
    app_handle: AppHandle,
    source_image_path: String,
    crop_rect: crate::thumbnail::CropRect,
) -> Result<String, String> {
    let dir = standalone_dir(&app_handle)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    crate::thumbnail::process_custom_thumbnail(&source_image_path, &dir, &crop_rect, false, None)
        .await
}

/// Save a thumbnail rendered in the Thumbnail tab, as the PNG it is.
#[tauri::command]
pub async fn save_standalone_thumbnail(
    app_handle: AppHandle,
    image_data: Vec<u8>,
) -> Result<String, String> {
    let path = standalone_dir(&app_handle)?.join(format!(
        "thumbnail_{}.png",
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::write(&path, &image_data).map_err(|e| format!("Failed to save thumbnail: {}", e))?;
    Ok(path.to_string_lossy().to_string())
}

/// Copy a file, creating the destination's parent directory if needed.
#[tauri::command]
pub async fn copy_file(source: String, destination: String) -> Result<(), String> {
    if let Some(parent) = Path::new(&destination).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create destination directory: {}", e))?;
    }
    std::fs::copy(&source, &destination)
        .map_err(|e| format!("Failed to copy {} to {}: {}", source, destination, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::YouTubeAuth;

    fn state_with_auth() -> AppStateHandle {
        let mut s = AppState::default();
        s.settings.output_folder = "/old".into();
        s.settings.upload_privacy = "private".into();
        s.settings.youtube_auth = Some(YouTubeAuth {
            refresh_token: Some("r".into()),
            ..Default::default()
        });
        Arc::new(Mutex::new(s))
    }

    fn patch(json: serde_json::Value) -> SettingsPatch {
        serde_json::from_value(json).unwrap()
    }

    async fn ok_switch(_: String) -> Result<(), String> {
        Ok(())
    }

    #[tokio::test]
    async fn patch_keeps_sign_in() {
        let state = state_with_auth();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"download_quality": "720p"})), 1, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        assert_eq!(view.download_quality, "720p");
        assert!(view.youtube_signed_in);
        let s = state.lock().await;
        assert!(s.settings.youtube_auth.is_some());
        assert_eq!(s.settings.upload_privacy, "private");
    }

    #[tokio::test]
    async fn stale_seq_is_ignored() {
        let state = state_with_auth();
        apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "new"})), 5, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "old"})), 4, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        assert_eq!(view.modal_api_url, "new");
        assert_eq!(state.lock().await.settings.modal_api_url, "new");
    }

    #[tokio::test]
    async fn failed_switch_restores_the_folder() {
        let state = state_with_auth();
        let seen = std::sync::Mutex::new(None::<String>);
        let err = apply_settings_patch(
            &state,
            patch(serde_json::json!({"output_folder": "/new", "download_quality": "480p"})),
            1,
            |_| Ok(()),
            |folder| {
                // The new folder was saved before switching.
                *seen.lock().unwrap() = Some(folder);
                async { Err("busy".to_string()) }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, "busy");
        assert_eq!(seen.lock().unwrap().as_deref(), Some("/new"));
        let s = state.lock().await;
        assert_eq!(s.settings.output_folder, "/old");
        // Other edits in the same save still apply.
        assert_eq!(s.settings.download_quality, "480p");
    }

    #[tokio::test]
    async fn refused_switch_saves_nothing() {
        let state = state_with_auth();
        let err = apply_settings_patch(
            &state,
            patch(serde_json::json!({"output_folder": "/new", "download_quality": "480p"})),
            1,
            |_| Err("wait".to_string()),
            |_| async { panic!("must not switch") },
        )
        .await
        .unwrap_err();
        assert_eq!(err, "wait");
        let s = state.lock().await;
        assert_eq!(s.settings.output_folder, "/old");
        assert_eq!(s.settings.download_quality, "best");
    }
}
