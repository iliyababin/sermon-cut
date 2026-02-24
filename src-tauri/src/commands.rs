use crate::ffmpeg;
use crate::modal;
use crate::state::{AppState, ProcessingStage, Settings, TranscriptionResult, VideoInfo, VideoStatus};
use crate::thumbnail::{self, CropRect};
use crate::youtube_dl;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

type AppStateHandle = Arc<Mutex<AppState>>;

#[tauri::command]
pub async fn get_video_info(url: String) -> Result<VideoInfo, String> {
    youtube_dl::get_video_info(&url).await
}

#[tauri::command]
pub async fn download_video(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    url: String,
    quality: Option<String>,
) -> Result<VideoInfo, String> {
    let quality = quality.unwrap_or_else(|| "best".to_string());

    // Get video info first
    let mut video_info = youtube_dl::get_video_info(&url).await?;
    let video_id = video_info.id.clone();

    // Create cancel channel
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let output_dir = {
        let mut state = state.lock().await;
        video_info.status = VideoStatus::Downloading;
        state.videos.insert(video_id.clone(), video_info.clone());
        state.active_downloads.insert(video_id.clone(), cancel_tx);
        state.settings.output_folder.clone()
    };

    // Start download
    let result = youtube_dl::download_video(&url, &quality, &video_id, &output_dir, cancel_rx, app_handle).await;

    // Clean up and update state
    let mut state = state.lock().await;
    state.active_downloads.remove(&video_id);

    match result {
        Ok(file_path) => {
            if let Some(video) = state.videos.get_mut(&video_id) {
                video.file_path = Some(file_path);
                video.status = VideoStatus::Downloaded;
                video.download_progress = 100.0;
            }
            if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }
            Ok(state.videos.get(&video_id).cloned().unwrap())
        }
        Err(e) => {
            if let Some(video) = state.videos.get_mut(&video_id) {
                video.status = VideoStatus::Error;
            }
            if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn cancel_download(
    state: State<'_, AppStateHandle>,
    video_id: String,
) -> Result<(), String> {
    let state = state.lock().await;
    if let Some(cancel_tx) = state.active_downloads.get(&video_id) {
        cancel_tx.send(true).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_download_progress(
    state: State<'_, AppStateHandle>,
    video_id: String,
) -> Result<f32, String> {
    let state = state.lock().await;
    state
        .videos
        .get(&video_id)
        .map(|v| v.download_progress)
        .ok_or_else(|| "Video not found".to_string())
}

#[tauri::command]
pub async fn extract_audio(
    state: State<'_, AppStateHandle>,
    video_id: String,
) -> Result<String, String> {
    let (video_path, output_dir) = {
        let mut state = state.lock().await;
        let video = state
            .videos
            .get_mut(&video_id)
            .ok_or("Video not found")?;

        let video_path = video
            .file_path
            .clone()
            .ok_or("Video file not found")?;

        video.status = VideoStatus::ExtractingAudio;
        (video_path, state.settings.output_folder.clone())
    };

    let audio_path = ffmpeg::extract_audio(&video_path, &output_dir).await?;

    let mut state = state.lock().await;
    if let Some(video) = state.videos.get_mut(&video_id) {
        video.audio_path = Some(audio_path.clone());
        video.status = VideoStatus::AudioReady;
    }
    if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }

    Ok(audio_path)
}

#[tauri::command]
pub async fn transcribe_audio(
    state: State<'_, AppStateHandle>,
    video_id: String,
    force_refresh: Option<bool>,
) -> Result<TranscriptionResult, String> {
    // Check for cached transcription first (unless force_refresh is true)
    let force = force_refresh.unwrap_or(false);
    {
        let state = state.lock().await;
        if let Some(video) = state.videos.get(&video_id) {
            if !force {
                if let Some(ref cached) = video.transcription {
                    println!("[transcribe] Using cached transcription for video {}", video_id);
                    return Ok(cached.clone());
                }
            }
        }
    }

    let (audio_path, api_url, api_key) = {
        let mut state = state.lock().await;
        let video = state
            .videos
            .get_mut(&video_id)
            .ok_or("Video not found")?;

        let audio_path = video
            .audio_path
            .clone()
            .ok_or("Audio file not found")?;

        video.status = VideoStatus::Transcribing;
        (
            audio_path,
            state.settings.modal_api_url.clone(),
            state.settings.modal_api_key.clone(),
        )
    };

    let result = modal::transcribe_audio(&audio_path, &api_url, api_key.as_deref()).await?;

    // Cache the transcription result
    let mut state = state.lock().await;
    if let Some(video) = state.videos.get_mut(&video_id) {
        video.status = VideoStatus::Transcribed;
        video.transcription = Some(result.clone());
    }
    if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }

    Ok(result)
}

#[tauri::command]
pub async fn trim_video(
    state: State<'_, AppStateHandle>,
    video_id: String,
    start_time: f64,
    end_time: f64,
) -> Result<String, String> {
    let (video_path, output_dir) = {
        let state = state.lock().await;
        let video = state
            .videos
            .get(&video_id)
            .ok_or("Video not found")?;

        let video_path = video
            .file_path
            .clone()
            .ok_or("Video file not found")?;

        (video_path, state.settings.output_folder.clone())
    };

    let trimmed_path = ffmpeg::trim_video(&video_path, start_time, end_time, &output_dir).await?;

    // Update video status to ready
    {
        let mut state = state.lock().await;
        if let Some(video) = state.videos.get_mut(&video_id) {
            video.status = VideoStatus::Ready;
        }
        if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(trimmed_path)
}

#[tauri::command]
pub async fn get_videos(state: State<'_, AppStateHandle>) -> Result<Vec<VideoInfo>, String> {
    let state = state.lock().await;
    Ok(state.videos.values().cloned().collect())
}

#[tauri::command]
pub async fn delete_video(
    state: State<'_, AppStateHandle>,
    video_id: String,
) -> Result<(), String> {
    let mut state = state.lock().await;

    if let Some(video) = state.videos.remove(&video_id) {
        // Delete files
        if let Some(path) = video.file_path {
            let _ = std::fs::remove_file(&path);
        }
        if let Some(path) = video.audio_path {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }

    Ok(())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppStateHandle>) -> Result<Settings, String> {
    let state = state.lock().await;
    Ok(state.settings.clone())
}

#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppStateHandle>,
    settings: Settings,
) -> Result<(), String> {
    let mut state = state.lock().await;
    state.settings = settings;
    state.save()
}

#[tauri::command]
pub async fn reset_state(state: State<'_, AppStateHandle>) -> Result<(), String> {
    let mut state = state.lock().await;

    // Delete everything inside the output folder
    let output_folder = std::path::Path::new(&state.settings.output_folder);
    if output_folder.exists() {
        if let Ok(entries) = std::fs::read_dir(output_folder) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    state.videos.clear();
    state.active_downloads.clear();
    state.settings = Settings::default();
    state.save()
}

#[tauri::command]
pub async fn get_app_data_dir() -> Result<String, String> {
    dirs::data_dir()
        .map(|p| p.join("sermon-cut").to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine app data directory".to_string())
}

#[tauri::command]
pub async fn add_local_video(
    state: State<'_, AppStateHandle>,
    file_path: String,
) -> Result<VideoInfo, String> {
    let path = std::path::Path::new(&file_path);

    if !path.exists() {
        return Err("File does not exist".to_string());
    }

    // Get file name as title
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();

    // Generate a unique ID from the file path
    let id = format!("local_{:x}", md5::compute(&file_path).0.iter().take(8).fold(0u64, |acc, &b| acc << 8 | b as u64));

    // Try to get duration using ffprobe
    let duration = ffmpeg::get_duration(&file_path).await.ok();

    let video_info = VideoInfo {
        id: id.clone(),
        title,
        url: format!("file://{}", file_path),
        thumbnail: None,
        duration,
        channel: None,
        upload_date: None,
        file_path: Some(file_path),
        audio_path: None,
        thumbnail_path: None,
        transcription: None,
        status: VideoStatus::Downloaded,
        download_progress: 100.0,
        created_at: chrono::Utc::now().to_rfc3339(),
        trimmed_path: None,
        thumbnail_options: None,
        processing_stage: None,
        processing_progress: None,
    };

    let mut state = state.lock().await;
    state.videos.insert(id, video_info.clone());
    if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }

    Ok(video_info)
}

#[tauri::command]
pub async fn generate_thumbnail(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    start_time: f64,
    end_time: f64,
    title: String,
    logo_path: Option<String>,
) -> Result<String, String> {
    let (video_path, output_dir) = {
        let state = state.lock().await;
        let video = state
            .videos
            .get(&video_id)
            .ok_or("Video not found")?;

        let video_path = video
            .file_path
            .clone()
            .ok_or("Video file not found")?;

        (video_path, state.settings.output_folder.clone())
    };

    // Use provided logo or fall back to bundled default
    let effective_logo_path = match logo_path {
        Some(path) if !path.is_empty() => Some(path),
        _ => {
            // Try to get bundled default logo
            app_handle
                .path()
                .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
                .ok()
                .map(|p: std::path::PathBuf| p.to_string_lossy().to_string())
        }
    };

    let thumbnail_path = thumbnail::generate_thumbnail(
        &video_path,
        start_time,
        end_time,
        &title,
        &output_dir,
        effective_logo_path.as_deref(),
    )
    .await?;

    // Update video with thumbnail path
    {
        let mut state = state.lock().await;
        if let Some(video) = state.videos.get_mut(&video_id) {
            video.thumbnail_path = Some(thumbnail_path.clone());
        }
        if let Err(e) = state.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(thumbnail_path)
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

#[tauri::command]
pub async fn generate_thumbnail_options(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    start_time: f64,
    end_time: f64,
    title: String,
    count: Option<u32>,
    logo_path: Option<String>,
) -> Result<Vec<String>, String> {
    let count = count.unwrap_or(10).min(50);

    let (video_path, output_dir) = {
        let state = state.lock().await;
        let video = state
            .videos
            .get(&video_id)
            .ok_or("Video not found")?;

        let video_path = video
            .file_path
            .clone()
            .ok_or("Video file not found")?;

        (video_path, state.settings.output_folder.clone())
    };

    // Use provided logo or fall back to bundled default
    let effective_logo_path = match logo_path {
        Some(path) if !path.is_empty() => Some(path),
        _ => {
            // Try to get bundled default logo
            app_handle
                .path()
                .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
                .ok()
                .map(|p: std::path::PathBuf| p.to_string_lossy().to_string())
        }
    };

    let thumbnail_paths = thumbnail::generate_thumbnail_options(
        &video_path,
        start_time,
        end_time,
        &title,
        &output_dir,
        count,
        effective_logo_path.as_deref(),
    )
    .await?;

    Ok(thumbnail_paths)
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

#[tauri::command]
pub async fn youtube_list_playlists(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
) -> Result<Vec<crate::youtube::PlaylistInfo>, String> {
    use crate::youtube::{self, YouTubeCredentials};

    // Get current tokens and check if refresh is needed
    let (mut access_token, refresh_token, needs_refresh) = {
        let state_guard = state.lock().await;
        let auth = state_guard.settings.youtube_auth.as_ref()
            .ok_or("Not authenticated with YouTube")?;

        let access_token = auth.access_token.clone();
        let refresh_token = auth.refresh_token.clone();
        let needs_refresh = access_token.is_none() || auth.expires_at
            .map(|exp| exp - chrono::Utc::now().timestamp() < 300)
            .unwrap_or(true);

        (access_token.unwrap_or_default(), refresh_token, needs_refresh)
    };

    // Refresh token if needed
    if needs_refresh {
        if let Some(ref refresh) = refresh_token {
            let credentials = YouTubeCredentials::load_from_resource(&app_handle)?;
            let new_tokens = youtube::refresh_access_token(&credentials, refresh).await?;

            {
                let mut state_guard = state.lock().await;
                if let Some(ref mut auth) = state_guard.settings.youtube_auth {
                    auth.access_token = Some(new_tokens.access_token.clone());
                    auth.expires_at = new_tokens.expires_at;
                }
                if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
            }

            access_token = new_tokens.access_token;
        }
    }

    youtube::list_playlists(&access_token).await
}

#[tauri::command]
pub async fn youtube_upload_video(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_path: String,
    title: String,
    description: String,
    thumbnail_path: Option<String>,
    privacy: Option<String>,
    source_video_id: Option<String>,
    start_time: Option<f64>,
    end_time: Option<f64>,
) -> Result<String, String> {
    use crate::youtube::{self, YouTubeCredentials};

    let privacy = privacy.unwrap_or_else(|| "unlisted".to_string());

    // Get transcription segments if source video ID is provided
    let segments = if let Some(ref src_id) = source_video_id {
        let state_guard = state.lock().await;
        state_guard.videos.get(src_id)
            .and_then(|v| v.transcription.as_ref())
            .map(|t| t.segments.clone())
    } else {
        None
    };

    // Get current tokens and check if refresh is needed
    let (mut access_token, refresh_token, needs_refresh) = {
        let state_guard = state.lock().await;
        let auth = state_guard.settings.youtube_auth.as_ref()
            .ok_or("Not authenticated with YouTube")?;

        let access_token = auth.access_token.clone();
        let refresh_token = auth.refresh_token.clone();
        let needs_refresh = access_token.is_none() || auth.expires_at
            .map(|exp| exp - chrono::Utc::now().timestamp() < 300)
            .unwrap_or(true);

        (access_token.unwrap_or_default(), refresh_token, needs_refresh)
    };

    // Refresh token if needed
    if needs_refresh {
        if let Some(ref refresh) = refresh_token {
            let credentials = YouTubeCredentials::load_from_resource(&app_handle)?;
            let new_tokens = youtube::refresh_access_token(&credentials, refresh).await?;

            // Update stored tokens
            {
                let mut state_guard = state.lock().await;
                if let Some(ref mut auth) = state_guard.settings.youtube_auth {
                    auth.access_token = Some(new_tokens.access_token.clone());
                    auth.expires_at = new_tokens.expires_at;
                }
                if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
            }

            access_token = new_tokens.access_token;
        }
    }

    // Upload video
    let upload_result = youtube::upload_video(
        &access_token,
        &video_path,
        &title,
        &description,
        &privacy,
        Some(&app_handle),
    ).await?;

    let video_id = upload_result.id.clone();

    // Set thumbnail if provided
    if let Some(thumb_path) = thumbnail_path {
        if let Err(e) = youtube::set_thumbnail(&access_token, &video_id, &thumb_path).await {
            println!("[youtube] Warning: Thumbnail upload failed: {}", e);
        }
    }

    // Upload captions if we have segments
    if let Some(segs) = segments {
        let start = start_time.unwrap_or(0.0);
        let end = end_time.unwrap_or(f64::MAX);
        let srt_content = youtube::generate_srt(&segs, start, end);

        if !srt_content.trim().is_empty() {
            if let Err(e) = youtube::upload_captions(&access_token, &video_id, &srt_content).await {
                println!("[youtube] Warning: Caption upload failed: {}", e);
            }
        }
    }

    // Add to playlist if configured
    {
        let state_guard = state.lock().await;
        if let Some(ref playlist_id) = state_guard.settings.youtube_playlist_id {
            if !playlist_id.is_empty() {
                if let Err(e) = youtube::add_to_playlist(&access_token, playlist_id, &video_id).await {
                    println!("[youtube] Warning: Failed to add to playlist: {}", e);
                }
            }
        }
    }

    // Return video URL
    Ok(format!("https://youtube.com/watch?v={}", video_id))
}

// Processing progress event payload
#[derive(Debug, Clone, serde::Serialize)]
struct ProcessingProgressEvent {
    video_id: String,
    stage: ProcessingStage,
    progress: f32,
    message: String,
}

fn emit_processing_progress(
    app_handle: &AppHandle,
    video_id: &str,
    stage: ProcessingStage,
    progress: f32,
    message: &str,
) {
    let _ = app_handle.emit(
        "processing-progress",
        ProcessingProgressEvent {
            video_id: video_id.to_string(),
            stage,
            progress,
            message: message.to_string(),
        },
    );
}

#[tauri::command]
pub async fn process_video_full(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
) -> Result<VideoInfo, String> {
    println!("[process_video_full] Starting full processing for video: {}", video_id);

    // Update status to processing
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.status = VideoStatus::Processing;
            video.processing_stage = Some(ProcessingStage::ExtractingAudio);
            video.processing_progress = Some(0.0);
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    // Step 1: Extract audio
    emit_processing_progress(&app_handle, &video_id, ProcessingStage::ExtractingAudio, 0.0, "Extracting audio from video...");

    let (video_path, output_dir, api_url, api_key, logo_path) = {
        let state_guard = state.lock().await;
        let video = state_guard.videos.get(&video_id).ok_or("Video not found")?;
        let video_path = video.file_path.clone().ok_or("Video file not found")?;
        (
            video_path,
            state_guard.settings.output_folder.clone(),
            state_guard.settings.modal_api_url.clone(),
            state_guard.settings.modal_api_key.clone(),
            state_guard.settings.logo_path.clone(),
        )
    };

    let audio_path = ffmpeg::extract_audio(&video_path, &output_dir).await?;

    // Update state with audio path
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.audio_path = Some(audio_path.clone());
            video.processing_stage = Some(ProcessingStage::Transcribing);
            video.processing_progress = Some(25.0);
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    // Step 2: Transcribe audio
    emit_processing_progress(&app_handle, &video_id, ProcessingStage::Transcribing, 25.0, "Transcribing audio with AI...");

    let transcription = modal::transcribe_audio(&audio_path, &api_url, api_key.as_deref()).await?;

    // Get sermon boundaries
    let sermon_start = transcription.sermon_start.unwrap_or(0.0);
    let sermon_end = {
        let state_guard = state.lock().await;
        let video = state_guard.videos.get(&video_id).ok_or("Video not found")?;
        transcription.sermon_end.unwrap_or(video.duration.unwrap_or(0.0))
    };

    // Update state with transcription
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.transcription = Some(transcription.clone());
            video.processing_stage = Some(ProcessingStage::Trimming);
            video.processing_progress = Some(50.0);
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    // Step 3: Trim video
    emit_processing_progress(&app_handle, &video_id, ProcessingStage::Trimming, 50.0, "Trimming video to sermon boundaries...");

    let trimmed_path = ffmpeg::trim_video(&video_path, sermon_start, sermon_end, &output_dir).await?;

    // Update state with trimmed path
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.trimmed_path = Some(trimmed_path.clone());
            video.processing_stage = Some(ProcessingStage::GeneratingThumbnails);
            video.processing_progress = Some(75.0);
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    // Step 4: Generate thumbnail options
    emit_processing_progress(&app_handle, &video_id, ProcessingStage::GeneratingThumbnails, 75.0, "Generating thumbnail options...");

    let title = match transcription.suggested_title.clone() {
        Some(t) => t,
        None => {
            let state_guard = state.lock().await;
            state_guard.videos.get(&video_id).map(|v| v.title.clone()).unwrap_or_default()
        }
    };

    // Use provided logo or fall back to bundled default
    let effective_logo_path = match logo_path {
        Some(path) if !path.is_empty() => Some(path),
        _ => {
            app_handle
                .path()
                .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
                .ok()
                .map(|p: std::path::PathBuf| p.to_string_lossy().to_string())
        }
    };

    let thumbnail_options = thumbnail::generate_thumbnail_options(
        &video_path,
        sermon_start,
        sermon_end,
        &title,
        &output_dir,
        10, // Generate 10 thumbnail options
        effective_logo_path.as_deref(),
    )
    .await?;

    // Step 5: Complete - update final state
    emit_processing_progress(&app_handle, &video_id, ProcessingStage::Complete, 100.0, "Processing complete!");

    let final_video = {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.thumbnail_options = Some(thumbnail_options);
            video.status = VideoStatus::ReadyForReview;
            video.processing_stage = Some(ProcessingStage::Complete);
            video.processing_progress = Some(100.0);
            // Set first thumbnail as default
            if let Some(ref options) = video.thumbnail_options {
                if let Some(first) = options.first() {
                    video.thumbnail_path = Some(first.clone());
                }
            }
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
        state_guard.videos.get(&video_id).cloned()
    };

    final_video.ok_or_else(|| "Failed to get final video state".to_string())
}

#[tauri::command]
pub async fn update_video_metadata(
    state: State<'_, AppStateHandle>,
    video_id: String,
    title: Option<String>,
    description: Option<String>,
    thumbnail_path: Option<String>,
) -> Result<VideoInfo, String> {
    let mut state_guard = state.lock().await;

    {
        let video = state_guard.videos.get_mut(&video_id).ok_or("Video not found")?;

        if let Some(title) = title {
            if let Some(ref mut transcription) = video.transcription {
                transcription.suggested_title = Some(title);
            }
        }

        if let Some(description) = description {
            if let Some(ref mut transcription) = video.transcription {
                transcription.suggested_description = Some(description);
            }
        }

        if let Some(thumb_path) = thumbnail_path {
            video.thumbnail_path = Some(thumb_path);
        }
    }

    if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    state_guard.videos.get(&video_id).cloned().ok_or_else(|| "Video not found".to_string())
}

#[tauri::command]
pub async fn retrim_video(
    state: State<'_, AppStateHandle>,
    video_id: String,
    start_time: f64,
    end_time: f64,
) -> Result<String, String> {
    let (video_path, output_dir) = {
        let state_guard = state.lock().await;
        let video = state_guard.videos.get(&video_id).ok_or("Video not found")?;
        let video_path = video.file_path.clone().ok_or("Video file not found")?;
        (video_path, state_guard.settings.output_folder.clone())
    };

    let trimmed_path = ffmpeg::trim_video(&video_path, start_time, end_time, &output_dir).await?;

    // Update state
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.trimmed_path = Some(trimmed_path.clone());
            if let Some(ref mut transcription) = video.transcription {
                transcription.sermon_start = Some(start_time);
                transcription.sermon_end = Some(end_time);
            }
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(trimmed_path)
}

#[tauri::command]
pub async fn regenerate_thumbnails(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
) -> Result<Vec<String>, String> {
    let (video_path, output_dir, sermon_start, sermon_end, title, logo_path) = {
        let state_guard = state.lock().await;
        let video = state_guard.videos.get(&video_id).ok_or("Video not found")?;
        let video_path = video.file_path.clone().ok_or("Video file not found")?;
        let transcription = video.transcription.as_ref();
        let sermon_start = transcription.and_then(|t| t.sermon_start).unwrap_or(0.0);
        let sermon_end = transcription.and_then(|t| t.sermon_end).unwrap_or(video.duration.unwrap_or(0.0));
        let title = transcription.and_then(|t| t.suggested_title.clone()).unwrap_or_else(|| video.title.clone());
        (
            video_path,
            state_guard.settings.output_folder.clone(),
            sermon_start,
            sermon_end,
            title,
            state_guard.settings.logo_path.clone(),
        )
    };

    // Use provided logo or fall back to bundled default
    let effective_logo_path = match logo_path {
        Some(path) if !path.is_empty() => Some(path),
        _ => {
            app_handle
                .path()
                .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
                .ok()
                .map(|p: std::path::PathBuf| p.to_string_lossy().to_string())
        }
    };

    let thumbnail_options = thumbnail::generate_thumbnail_options(
        &video_path,
        sermon_start,
        sermon_end,
        &title,
        &output_dir,
        10,
        effective_logo_path.as_deref(),
    )
    .await?;

    // Update state
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            video.thumbnail_options = Some(thumbnail_options.clone());
            if let Some(first) = thumbnail_options.first() {
                video.thumbnail_path = Some(first.clone());
            }
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(thumbnail_options)
}

#[tauri::command]
pub async fn process_custom_thumbnail(
    state: State<'_, AppStateHandle>,
    app_handle: AppHandle,
    video_id: String,
    source_image_path: String,
    crop_rect: CropRect,
    apply_color_grading: bool,
    apply_logo_overlay: bool,
) -> Result<String, String> {
    let (output_dir, logo_path) = {
        let state_guard = state.lock().await;
        (
            state_guard.settings.output_folder.clone(),
            state_guard.settings.logo_path.clone(),
        )
    };

    // Determine logo path based on settings
    let effective_logo_path = if apply_logo_overlay {
        match logo_path {
            Some(path) if !path.is_empty() => Some(path),
            _ => {
                // Try to get bundled default logo
                app_handle
                    .path()
                    .resolve("resources/default_logo.png", tauri::path::BaseDirectory::Resource)
                    .ok()
                    .map(|p: std::path::PathBuf| p.to_string_lossy().to_string())
            }
        }
    } else {
        None
    };

    let thumbnail_path = thumbnail::process_custom_thumbnail(
        &source_image_path,
        &output_dir,
        &crop_rect,
        apply_color_grading,
        effective_logo_path.as_deref(),
    )
    .await?;

    // Add to thumbnail options and set as selected
    {
        let mut state_guard = state.lock().await;
        if let Some(video) = state_guard.videos.get_mut(&video_id) {
            // Add to thumbnail options if not already present
            if let Some(ref mut options) = video.thumbnail_options {
                if !options.contains(&thumbnail_path) {
                    options.insert(0, thumbnail_path.clone());
                }
            } else {
                video.thumbnail_options = Some(vec![thumbnail_path.clone()]);
            }
            // Set as selected thumbnail
            video.thumbnail_path = Some(thumbnail_path.clone());
        }
        if let Err(e) = state_guard.save() { eprintln!("[warn] Failed to save state: {}", e); }
    }

    Ok(thumbnail_path)
}
