use crate::library::commands::LibraryHandle;
use crate::state::{AppState, Settings, SettingsPatch, SettingsView, VideoInfo};
use crate::upload::{TokenSource, UploadError};
use crate::youtube_dl;
use futures::future::BoxFuture;
use crate::library::fsutil::{resolve, resolve_within};
use std::path::{Path, PathBuf};
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

/// Whether `path` is inside one of `roots` (see `fsutil::resolve_within`).
pub fn is_within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| resolve_within(path, root).is_some())
}

/// What a file command does with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Shown in Finder or opened in its own app.
    Open,
    /// Read or written for the webview.
    Data,
}

/// The folders the file commands below may touch: the library, the
/// standalone Thumbnail tab's folder and the temp folder. Opening in Finder
/// may also reach the app's data, settings and tools folders (Settings
/// shows them); reading and writing may not.
pub fn allowed_roots(library_root: Option<PathBuf>, app_data: Option<PathBuf>, access: Access) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(library_root);
    roots.extend(app_data.as_ref().map(|d| d.join(STANDALONE_DIR)));
    roots.push(std::env::temp_dir());
    if access == Access::Open {
        roots.extend(app_data);
        roots.push(crate::state::state_dir());
        roots.extend(crate::tools::tools_dir());
    }
    roots
}

/// state.json, its backup, its damaged and pre-library copies and its
/// temp files: never read or written for the webview, wherever they are.
fn is_state_file(path: &Path) -> bool {
    let named = |p: &Path| {
        p.file_name().map(|n| n.to_string_lossy().to_string()).is_some_and(|n| {
            n == "state.json" || n.starts_with("state.json.") || n == "state.v0.json" || n.starts_with(".state.json")
        })
    };
    named(path) || resolve(path).is_some_and(|p| named(&p))
}

/// The resolved path (see `fsutil::resolve`) a file command may use for
/// `path`: inside `roots`, or picked by the user in a dialog; never one of
/// state.json's files when reading or writing.
pub fn allowed_path(path: &Path, roots: &[PathBuf], picked: bool, access: Access) -> Option<PathBuf> {
    if access == Access::Data && is_state_file(path) {
        return None;
    }
    roots
        .iter()
        .find_map(|root| resolve_within(path, root))
        .or_else(|| picked.then(|| resolve(path)).flatten())
}

pub fn path_allowed(path: &Path, roots: &[PathBuf], picked: bool, access: Access) -> bool {
    allowed_path(path, roots, picked, access).is_some()
}

/// Refuse a path outside the app's folders, unless the user picked it in an
/// open/save dialog this session (the dialog adds picks to the fs scope).
/// Returns the resolved path, which callers use instead of the string they
/// were given (docs/INVARIANTS.md #11).
pub fn check_path(app_handle: &AppHandle, lib: &LibraryHandle, path: &str, access: Access) -> Result<PathBuf, String> {
    use tauri_plugin_fs::FsExt;
    let p = Path::new(path);
    let picked = app_handle
        .try_fs_scope()
        .is_some_and(|scope| scope.is_allowed(p));
    let roots = allowed_roots(lib.root(), app_handle.path().app_data_dir().ok(), access);
    allowed_path(p, &roots, picked, access)
        .ok_or_else(|| format!("Sermon Cut can only use files in its own folders or ones you chose: {}", path))
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
/// makes in one session (`epoch`); a field's value older than one already
/// applied to that field is ignored. Returns the settings as they now are.
#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppStateHandle>,
    lib: State<'_, LibraryHandle>,
    app_handle: AppHandle,
    patch: SettingsPatch,
    seq: u64,
    epoch: String,
) -> Result<SettingsView, String> {
    let library = lib.inner().clone();
    let result = apply_settings_patch(
        &state,
        patch,
        &epoch,
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
    mut patch: SettingsPatch,
    epoch: &str,
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
        if !patch.drop_stale(&mut s.settings_seq, epoch, seq) {
            return Ok(s.settings.view());
        }

        let mut next = s.settings.clone();
        next.apply(&patch);
        let new_folder = patch
            .output_folder
            .as_ref()
            .map(|f| f.trim().to_string())
            .filter(|f| *f != s.settings.output_folder);
        if let Some(folder) = &new_folder {
            next.output_folder = folder.clone();
            // Only a newly chosen folder is checked: one already in use
            // keeps opening whatever the rules say now.
            crate::library::commands::check_new_library_folder(&next.library_folder())?;
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

/// Where the app keeps its own files, for the Settings screen.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppFolders {
    /// state.json: settings and the YouTube sign-in.
    pub settings: String,
    /// ffmpeg, ffprobe and yt-dlp, as install.sh puts them.
    pub tools: Option<String>,
}

#[tauri::command]
pub async fn get_app_folders() -> Result<AppFolders, String> {
    Ok(AppFolders {
        settings: crate::state::state_dir().to_string_lossy().to_string(),
        tools: crate::tools::tools_dir().map(|p| p.to_string_lossy().to_string()),
    })
}

#[tauri::command]
pub async fn read_image_base64(
    app_handle: AppHandle,
    lib: State<'_, LibraryHandle>,
    path: String,
) -> Result<String, String> {
    use base64::Engine;
    let path = check_path(&app_handle, &lib, &path, Access::Data)?;

    let bytes = crate::library::fsutil::read_limited(&path, MAX_IMAGE_BYTES).map_err(|e| match e {
        crate::library::fsutil::ReadJsonError::TooLarge => "The image is too large (over 50 MB)".to_string(),
        e => format!("Failed to read image: {}", e),
    })?;
    let mime = image_mime(&bytes).ok_or("That file isn't a JPEG, PNG, WebP, GIF or HEIC image")?;
    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, base64))
}

/// Largest image read for the webview.
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

/// An image's MIME type from its first bytes (never its name): JPEG, PNG,
/// WebP, GIF or HEIC/HEIF. None for anything else.
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        const HEIC: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
        const HEIF: [&[u8; 4]; 2] = [b"mif1", b"msf1"];
        let brand: &[u8; 4] = bytes[8..12].try_into().ok()?;
        if HEIC.contains(&brand) {
            return Some("image/heic");
        }
        if HEIF.contains(&brand) {
            return Some("image/heif");
        }
    }
    None
}

#[tauri::command]
pub async fn open_file(app_handle: AppHandle, lib: State<'_, LibraryHandle>, path: String) -> Result<(), String> {
    let path = check_path(&app_handle, &lib, &path, Access::Open)?;
    println!("[open_file] Opening file: {}", path.display());

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
            .args(["/C", "start", ""])
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub async fn open_folder(app_handle: AppHandle, lib: State<'_, LibraryHandle>, path: String) -> Result<(), String> {
    let path = check_path(&app_handle, &lib, &path, Access::Open)?;
    open_folder_path(path.to_string_lossy().to_string())
}

/// Open a folder (or a file's folder) in the file manager, unchecked: for
/// paths the backend chose itself.
pub fn open_folder_path(path: String) -> Result<(), String> {
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
    sign_in: State<'_, crate::youtube::SignInSlot>,
    app_handle: AppHandle,
) -> Result<YouTubeAuthStatus, String> {
    use crate::youtube::{self, YouTubeCredentials};
    use crate::state::YouTubeAuth;

    // Load credentials
    let credentials = YouTubeCredentials::load_from_resource(&app_handle)?;

    // Wait for the browser (up to five minutes). Starting another sign-in
    // cancels this one.
    let (slot, cancel) = sign_in.begin();
    let tokens = youtube::start_oauth_flow(&credentials, cancel).await;
    sign_in.end(slot);
    let tokens = tokens?;

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

/// One token refresh at a time, app-wide: concurrent callers (the upload,
/// a playlist list, a thumbnail retry) wait for it and use its result.
/// Taken before AppState's lock, never while holding it.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub const ACCOUNT_CHANGED: &str = "The YouTube account changed while Sermon Cut was renewing the sign-in. Try again.";

/// The access token, refreshed when it's about to expire or `force` is set
/// (after a 401). With no refresh token to renew it, the user has to sign
/// in again.
pub async fn access_token(state: &AppStateHandle, app_handle: &AppHandle, force: bool) -> Result<String, UploadError> {
    let endpoints = crate::youtube::OAuthEndpoints::google();
    let credentials = || crate::youtube::YouTubeCredentials::load_from_resource(app_handle);
    access_token_with(state, force, credentials, &endpoints.token, &REFRESH).await
}

/// What the signed-in account looks like at one moment.
#[derive(PartialEq)]
struct AuthSnapshot {
    access: Option<String>,
    refresh: Option<String>,
    channel_id: Option<String>,
    expiring: bool,
}

async fn snapshot(state: &AppStateHandle) -> Result<AuthSnapshot, UploadError> {
    let g = state.lock().await;
    let auth = g.settings.youtube_auth.as_ref().ok_or_else(|| UploadError::Auth("not signed in".into()))?;
    Ok(AuthSnapshot {
        access: auth.access_token.clone(),
        refresh: auth.refresh_token.clone(),
        channel_id: auth.channel_id.clone(),
        expiring: auth.expires_at.map(|exp| exp - chrono::Utc::now().timestamp() < 300).unwrap_or(true),
    })
}

/// `access_token` with the pieces injected (tests use a mock token
/// endpoint and their own lock).
///
/// - Single flight: refreshes take `refresh_lock`; a caller that waited
///   finds the token another one fetched meanwhile and uses it.
/// - Compare and swap: the new token is written back only if the account
///   (channel and refresh token) is still the one it was fetched for;
///   after a sign-in to another account it is dropped.
pub(crate) async fn access_token_with(
    state: &AppStateHandle,
    force: bool,
    credentials: impl FnOnce() -> Result<crate::youtube::YouTubeCredentials, String>,
    token_url: &str,
    refresh_lock: &tokio::sync::Mutex<()>,
) -> Result<String, UploadError> {
    let seen = snapshot(state).await?;
    if !force && !seen.expiring {
        if let Some(access) = seen.access {
            return Ok(access);
        }
    }

    let _one = refresh_lock.lock().await;
    let current = snapshot(state).await?;
    if current.access.is_some() && current.access != seen.access && !current.expiring {
        // Refreshed (or signed in again) while this caller waited.
        return Ok(current.access.unwrap_or_default());
    }
    let Some(refresh) = current.refresh.clone() else {
        return Err(UploadError::Auth("no refresh token".into()));
    };

    let credentials = credentials().map_err(UploadError::Other)?;
    let new_tokens = crate::youtube::refresh_access_token_at(&credentials, &refresh, token_url).await?;
    let mut g = state.lock().await;
    let Some(auth) = g.settings.youtube_auth.as_mut() else {
        return Err(UploadError::Auth("signed out".into()));
    };
    if auth.channel_id != current.channel_id || auth.refresh_token.as_ref() != Some(&refresh) {
        println!("[youtube] The account changed during a token refresh; not saving it");
        return Err(UploadError::Other(ACCOUNT_CHANGED.into()));
    }
    auth.access_token = Some(new_tokens.access_token.clone());
    auth.expires_at = new_tokens.expires_at;
    if let Err(e) = g.save() {
        eprintln!("[warn] Failed to save state: {}", e);
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

const STANDALONE_DIR: &str = "standalone-thumbnails";

/// Working folder for the standalone Thumbnail tab, in the app's data folder
/// (covered by the `$APPDATA/**` asset scope), so it works without a library.
fn standalone_dir(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Can't find the app data folder: {}", e))?
        .join(STANDALONE_DIR);
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
    lib: State<'_, LibraryHandle>,
    source_image_path: String,
    crop_rect: crate::thumbnail::CropRect,
) -> Result<String, String> {
    // Usually a picked image; the dialog adds picks to the fs scope.
    let source = check_path(&app_handle, &lib, &source_image_path, Access::Data)?;
    let dir = standalone_dir(&app_handle)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    crate::thumbnail::process_custom_thumbnail(&source, &dir, &crop_rect, false)
        .await
}

/// Save a thumbnail rendered in the Thumbnail tab, as a JPEG within
/// YouTube's limits so the exported file can be uploaded as is.
#[tauri::command]
pub async fn save_standalone_thumbnail(
    app_handle: AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<String, String> {
    // Raw invoke body, like save_rendered_thumbnail.
    let image_data = crate::library::commands::raw_image(request.body())?;
    let path = standalone_dir(&app_handle)?.join(format!(
        "thumbnail_{}.jpg",
        chrono::Utc::now().timestamp_millis()
    ));
    let out = path.clone();
    tokio::task::spawn_blocking(move || crate::thumbnail::save_for_youtube(&image_data, &out))
        .await
        .map_err(|e| format!("Background task failed: {}", e))?
        .map_err(|e| format!("Failed to save thumbnail: {}", e))?;
    Ok(path.to_string_lossy().to_string())
}

/// Copy a file, creating the destination's parent directory if needed. Both
/// ends must be in the app's folders or picked in a dialog (the Thumbnail
/// tab's Save As).
#[tauri::command]
pub async fn copy_file(
    app_handle: AppHandle,
    lib: State<'_, LibraryHandle>,
    source: String,
    destination: String,
) -> Result<(), String> {
    let from = check_path(&app_handle, &lib, &source, Access::Data)?;
    let to = check_path(&app_handle, &lib, &destination, Access::Data)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create destination directory: {}", e))?;
    }
    std::fs::copy(&from, &to)
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

    /// Signed in as channel UC1 with an access token that has expired.
    fn state_with_expired_token() -> AppStateHandle {
        let mut s = AppState::default();
        s.settings.youtube_auth = Some(YouTubeAuth {
            access_token: Some("old".into()),
            refresh_token: Some("r1".into()),
            expires_at: Some(0),
            channel_name: Some("Youth".into()),
            channel_id: Some("UC1".into()),
        });
        Arc::new(Mutex::new(s))
    }

    fn credentials() -> Result<crate::youtube::YouTubeCredentials, String> {
        Ok(crate::youtube::YouTubeCredentials { client_id: "c".into(), client_secret: "s".into() })
    }

    #[tokio::test]
    async fn concurrent_refreshes_make_one_request() {
        let server = crate::upload::test_server::MockServer::start().await;
        server.with(|s| s.token_delay = std::time::Duration::from_millis(200));
        let url = format!("{}/token", server.base);
        let state = state_with_expired_token();
        let lock = tokio::sync::Mutex::new(());
        let (a, b, c) = tokio::join!(
            access_token_with(&state, false, credentials, &url, &lock),
            access_token_with(&state, false, credentials, &url, &lock),
            // After a 401 with the old token: the one being fetched will do.
            access_token_with(&state, true, credentials, &url, &lock),
        );
        assert_eq!((a.unwrap(), b.unwrap(), c.unwrap()), ("new-access".into(), "new-access".into(), "new-access".into()));
        assert_eq!(server.with(|s| s.token_requests.len()), 1);
        let auth = state.lock().await.settings.youtube_auth.clone().unwrap();
        assert_eq!(auth.access_token.as_deref(), Some("new-access"));
        assert!(auth.expires_at.unwrap() > chrono::Utc::now().timestamp());
    }

    #[tokio::test]
    async fn a_refresh_for_an_account_switched_away_from_isnt_saved() {
        let server = crate::upload::test_server::MockServer::start().await;
        server.with(|s| s.token_delay = std::time::Duration::from_millis(300));
        let url = format!("{}/token", server.base);
        let state = state_with_expired_token();
        let lock = tokio::sync::Mutex::new(());
        let refresh = access_token_with(&state, true, credentials, &url, &lock);
        let switch = async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            state.lock().await.settings.youtube_auth = Some(YouTubeAuth {
                access_token: Some("other-access".into()),
                refresh_token: Some("r2".into()),
                expires_at: Some(chrono::Utc::now().timestamp() + 3600),
                channel_name: Some("Other".into()),
                channel_id: Some("UC2".into()),
            });
        };
        let (result, _) = tokio::join!(refresh, switch);
        assert_eq!(result.unwrap_err(), UploadError::Other(ACCOUNT_CHANGED.into()));
        let auth = state.lock().await.settings.youtube_auth.clone().unwrap();
        assert_eq!((auth.channel_id.as_deref(), auth.access_token.as_deref()), (Some("UC2"), Some("other-access")));
    }

    #[tokio::test]
    async fn a_fresh_token_is_used_without_a_request() {
        let server = crate::upload::test_server::MockServer::start().await;
        let url = format!("{}/token", server.base);
        let state = state_with_expired_token();
        state.lock().await.settings.youtube_auth.as_mut().unwrap().expires_at = Some(chrono::Utc::now().timestamp() + 3600);
        let lock = tokio::sync::Mutex::new(());
        assert_eq!(access_token_with(&state, false, credentials, &url, &lock).await.unwrap(), "old");
        assert!(server.with(|s| s.token_requests.is_empty()));
    }

    fn patch(json: serde_json::Value) -> SettingsPatch {
        serde_json::from_value(json).unwrap()
    }

    async fn ok_switch(_: String) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn is_within_resolves_dots_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("sub/a.jpg"), b"x").unwrap();
        std::fs::write(outside.join("secret"), b"x").unwrap();
        let roots = vec![root.clone()];

        assert!(is_within(&root.join("sub/a.jpg"), &roots));
        assert!(is_within(&root, &roots));
        // Not there yet (a copy's destination), in a folder not there yet.
        assert!(is_within(&root.join("new/dir/b.jpg"), &roots));
        assert!(!is_within(&outside.join("new.jpg"), &roots));
        // `..` out of the root, existing or not.
        assert!(!is_within(&root.join("sub/../../outside/secret"), &roots));
        assert!(!is_within(&root.join("missing/../../outside/x"), &roots));
        assert!(is_within(&root.join("sub/../sub/a.jpg"), &roots));
        // Relative paths and look-alike prefixes.
        assert!(!is_within(Path::new("lib/sub/a.jpg"), &roots));
        std::fs::create_dir_all(tmp.path().join("lib2")).unwrap();
        assert!(!is_within(&tmp.path().join("lib2/x"), &roots));
        // No roots, nothing allowed.
        assert!(!is_within(&root.join("sub/a.jpg"), &[]));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A symlink inside the root that points out of it.
            symlink(&outside, root.join("escape")).unwrap();
            assert!(!is_within(&root.join("escape/secret"), &roots));
            assert!(!is_within(&root.join("escape/new.jpg"), &roots));
            // A dangling symlink as the destination itself.
            symlink(outside.join("nope"), root.join("dangling")).unwrap();
            assert!(!is_within(&root.join("dangling"), &roots));
            // A root reached through a symlink still counts.
            symlink(&root, tmp.path().join("alias")).unwrap();
            assert!(is_within(&tmp.path().join("alias/sub/a.jpg"), &roots));
            assert!(is_within(&root.join("sub/a.jpg"), &[tmp.path().join("alias")]));
        }
    }

    #[test]
    fn image_types_come_from_their_bytes() {
        assert_eq!(image_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Some("image/png"));
        assert_eq!(image_mime(b"GIF89a\x01\0"), Some("image/gif"));
        assert_eq!(image_mime(b"RIFF\x24\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_mime(b"\0\0\0\x18ftypheic\0\0\0\0"), Some("image/heic"));
        assert_eq!(image_mime(b"\0\0\0\x18ftypmif1\0\0\0\0"), Some("image/heif"));
        // A video, a text file or nothing at all isn't an image.
        assert_eq!(image_mime(b"\0\0\0\x18ftypisom\0\0\0\0"), None);
        assert_eq!(image_mime(b"{\"refresh_token\": \"x\"}"), None);
        assert_eq!(image_mime(b"RIFF\x24\0\0\0WAVEfmt "), None);
        assert_eq!(image_mime(b""), None);
    }

    #[test]
    fn the_checked_path_is_the_resolved_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.jpg"), b"x").unwrap();
        let real = root.canonicalize().unwrap();
        let roots = vec![root.clone()];
        let got = allowed_path(&root.join("sub/../sub/./a.jpg"), &roots, false, Access::Data);
        assert_eq!(got, Some(real.join("sub/a.jpg")));
        // A pick outside the roots comes back resolved too.
        let picked = tmp.path().join("pick.png");
        std::fs::write(&picked, b"x").unwrap();
        assert_eq!(
            allowed_path(&tmp.path().join("lib/../pick.png"), &roots, true, Access::Data),
            Some(picked.canonicalize().unwrap())
        );
        assert_eq!(allowed_path(&picked, &roots, false, Access::Data), None);
    }

    #[test]
    fn reading_and_copying_stay_out_of_the_app_settings_and_tools() {
        // Outside the temp folder, which is itself allowed.
        let tmp = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        let lib = tmp.path().join("lib");
        let app_data = tmp.path().join("appdata");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(app_data.join("bin")).unwrap();
        std::fs::create_dir_all(app_data.join(STANDALONE_DIR)).unwrap();
        let data = allowed_roots(Some(lib.clone()), Some(app_data.clone()), Access::Data);
        let open = allowed_roots(Some(lib.clone()), Some(app_data.clone()), Access::Open);
        let state_json = crate::state::state_dir().join("state.json");
        let tool = crate::tools::tools_dir().unwrap().join("ffmpeg");

        for p in [&state_json, &tool, &app_data.join("bin/ffmpeg")] {
            assert!(!path_allowed(p, &data, false, Access::Data), "{}", p.display());
            assert!(path_allowed(p, &open, false, Access::Open), "{}", p.display());
        }
        assert!(path_allowed(&lib.join("a.jpg"), &data, false, Access::Data));
        assert!(path_allowed(&app_data.join(STANDALONE_DIR).join("t.jpg"), &data, false, Access::Data));

        // state.json and its copies, even where files may otherwise be used
        // (the temp folder) or the user picked them.
        let tmpdir = std::env::temp_dir();
        for name in ["state.json", "state.json.bak", "state.json.corrupt-20260101-000000", "state.v0.json"] {
            assert!(!path_allowed(&tmpdir.join(name), &data, true, Access::Data), "{}", name);
            assert!(!path_allowed(&lib.join(name), &data, false, Access::Data), "{}", name);
        }
        #[cfg(unix)]
        {
            // Nor through a link with an innocent name.
            std::fs::write(lib.join("state.json"), b"{}").unwrap();
            std::os::unix::fs::symlink(lib.join("state.json"), lib.join("logo.png")).unwrap();
            assert!(!path_allowed(&lib.join("logo.png"), &data, false, Access::Data));
        }
    }

    #[test]
    fn thumbnail_sources_are_the_apps_files_or_picked_ones() {
        let tmp = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        let lib = tmp.path().join("lib");
        let photos = tmp.path().join("Pictures");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&photos).unwrap();
        let roots = allowed_roots(Some(lib.clone()), None, Access::Data);
        let photo = photos.join("pastor.jpg");
        // Picked in the open dialog: accepted. Not picked: refused.
        assert!(path_allowed(&photo, &roots, true, Access::Data));
        assert!(!path_allowed(&photo, &roots, false, Access::Data));
        assert!(!path_allowed(Path::new("/etc/passwd"), &roots, false, Access::Data));
        // A generated option to crop again, in the library.
        assert!(path_allowed(&lib.join(".sermon-cut/videos/x/thumbnail_option_1_raw.jpg"), &roots, false, Access::Data));
    }

    #[tokio::test]
    async fn patch_keeps_sign_in() {
        let state = state_with_auth();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"download_quality": "720p"})), "e", 1, |_| Ok(()), ok_switch)
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
        apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "new"})), "e", 5, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "old"})), "e", 4, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        assert_eq!(view.modal_api_url, "new");
        assert_eq!(state.lock().await.settings.modal_api_url, "new");
    }

    #[tokio::test]
    async fn different_fields_out_of_order_both_apply() {
        let state = state_with_auth();
        apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "new"})), "e", 2, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"download_quality": "720p"})), "e", 1, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        assert_eq!((view.modal_api_url.as_str(), view.download_quality.as_str()), ("new", "720p"));

        // In one save, only the field already newer is dropped.
        let view = apply_settings_patch(
            &state,
            patch(serde_json::json!({"modal_api_url": "older", "logo_path": "/l.png"})),
            "e",
            1,
            |_| Ok(()),
            ok_switch,
        )
        .await
        .unwrap();
        assert_eq!(view.modal_api_url, "new");
        assert_eq!(view.logo_path.as_deref(), Some("/l.png"));
    }

    #[tokio::test]
    async fn a_new_epoch_numbers_saves_from_the_start() {
        let state = state_with_auth();
        apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "a"})), "first", 9, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        // The webview reloaded: its counter starts again.
        let view = apply_settings_patch(&state, patch(serde_json::json!({"modal_api_url": "b"})), "second", 1, |_| Ok(()), ok_switch)
            .await
            .unwrap();
        assert_eq!(view.modal_api_url, "b");
    }

    #[tokio::test]
    async fn failed_switch_restores_the_folder() {
        let state = state_with_auth();
        let seen = std::sync::Mutex::new(None::<String>);
        let err = apply_settings_patch(
            &state,
            patch(serde_json::json!({"output_folder": "/new", "download_quality": "480p"})),
            "e",
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
            "e",
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

    #[tokio::test]
    async fn a_refused_new_folder_is_not_saved_but_a_refused_current_one_is_kept() {
        let state = state_with_auth();
        let err = apply_settings_patch(
            &state,
            patch(serde_json::json!({"output_folder": "/"})),
            "e",
            1,
            |_| Ok(()),
            |_| async { panic!("must not switch") },
        )
        .await
        .unwrap_err();
        assert!(err.contains("can't be the library folder"), "{err}");
        assert_eq!(state.lock().await.settings.output_folder, "/old");

        // Already set to a folder the rules now refuse: other edits still save.
        state.lock().await.settings.output_folder = "/".into();
        let view = apply_settings_patch(&state, patch(serde_json::json!({"output_folder": "/", "download_quality": "720p"})), "e", 2, |_| Ok(()), |_| async { panic!("must not switch") })
            .await
            .unwrap();
        assert_eq!(view.download_quality, "720p");
        assert_eq!(state.lock().await.settings.output_folder, "/");
    }
}
