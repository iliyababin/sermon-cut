use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::Emitter;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const YOUTUBE_UPLOAD_URL: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
const YOUTUBE_THUMBNAILS_URL: &str = "https://www.googleapis.com/upload/youtube/v3/thumbnails/set";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouTubeCredentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct YouTubeTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Serialize)]
struct VideoSnippet {
    title: String,
    description: String,
    #[serde(rename = "categoryId")]
    category_id: String,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct VideoStatus {
    #[serde(rename = "privacyStatus")]
    privacy_status: String,
    #[serde(rename = "selfDeclaredMadeForKids")]
    self_declared_made_for_kids: bool,
}

#[derive(Debug, Serialize)]
struct VideoMetadata {
    snippet: VideoSnippet,
    status: VideoStatus,
}

#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    pub id: String,
    pub snippet: Option<UploadSnippet>,
}

#[derive(Debug, Deserialize)]
pub struct UploadSnippet {
    pub title: String,
}

impl YouTubeCredentials {
    pub fn load_from_resource(app_handle: &tauri::AppHandle) -> Result<Self, String> {
        use tauri::Manager;

        // Try resource path first (production), then fall back to dev path
        let resource_path = app_handle
            .path()
            .resolve(
                "resources/youtube_client_secrets.json",
                tauri::path::BaseDirectory::Resource,
            )
            .ok();

        let contents = if let Some(ref path) = resource_path {
            std::fs::read_to_string(path).ok()
        } else {
            None
        };

        // Fall back to development path
        let contents = contents.or_else(|| {
            let dev_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .join("youtube_client_secrets.json");
            println!("[youtube] Trying dev path: {:?}", dev_path);
            std::fs::read_to_string(&dev_path).ok()
        });

        let contents = contents.ok_or_else(|| {
            "Failed to read credentials file: not found in resources".to_string()
        })?;

        let json: serde_json::Value = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse credentials: {}", e))?;

        let installed = json
            .get("installed")
            .ok_or("Invalid credentials format: missing 'installed' key")?;

        Ok(YouTubeCredentials {
            client_id: installed["client_id"]
                .as_str()
                .ok_or("Missing client_id")?
                .to_string(),
            client_secret: installed["client_secret"]
                .as_str()
                .ok_or("Missing client_secret")?
                .to_string(),
        })
    }
}

pub async fn start_oauth_flow(credentials: &YouTubeCredentials) -> Result<YouTubeTokens, String> {
    // Find an available port
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind to port: {}", e))?;
    let port = listener.local_addr().unwrap().port();
    let redirect_uri = format!("http://127.0.0.1:{}", port);

    // Generate CSRF state parameter
    let oauth_state = uuid::Uuid::new_v4().to_string();

    // Build authorization URL
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        AUTH_URL,
        urlencoding::encode(&credentials.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode("https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube"),
        urlencoding::encode(&oauth_state)
    );

    println!("[youtube] Opening browser for OAuth: {}", auth_url);

    // Open browser - use platform-specific commands for reliability
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&auth_url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&auth_url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &auth_url])
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    println!("[youtube] Waiting for OAuth callback on port {}...", port);

    // Wait for the callback (async)
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| format!("Failed to accept connection: {}", e))?;

    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| format!("Failed to read request: {}", e))?;

    // Extract authorization code from the request and validate state
    let code = extract_code_from_request(&request_line, &oauth_state)?;

    // Send success response to browser
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Authentication successful!</h1><p>You can close this window and return to the app.</p><script>window.close();</script></body></html>";
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| format!("Failed to send response: {}", e))?;

    // Exchange code for tokens
    exchange_code_for_tokens(credentials, &code, &redirect_uri).await
}

fn extract_code_from_request(request_line: &str, expected_state: &str) -> Result<String, String> {
    // Parse "GET /?code=xxx&state=yyy&scope=... HTTP/1.1"
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("Invalid request format".to_string());
    }

    let path = parts[1];
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;

    if let Some(query_start) = path.find('?') {
        let query = &path[query_start + 1..];
        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                if key == "code" {
                    code = Some(urlencoding::decode(value)
                        .map_err(|e| format!("Failed to decode code: {}", e))?
                        .to_string());
                }
                if key == "state" {
                    state = Some(urlencoding::decode(value)
                        .map_err(|e| format!("Failed to decode state: {}", e))?
                        .to_string());
                }
                if key == "error" {
                    return Err(format!("OAuth error: {}", value));
                }
            }
        }
    }

    // Validate CSRF state parameter
    match state {
        Some(ref s) if s == expected_state => {}
        Some(_) => return Err("OAuth state mismatch — possible CSRF attack".to_string()),
        None => return Err("Missing OAuth state parameter in callback".to_string()),
    }

    code.ok_or_else(|| "No authorization code in callback".to_string())
}

async fn exchange_code_for_tokens(
    credentials: &YouTubeCredentials,
    code: &str,
    redirect_uri: &str,
) -> Result<YouTubeTokens, String> {
    let client = Client::new();

    let mut params = HashMap::new();
    params.insert("client_id", credentials.client_id.as_str());
    params.insert("client_secret", credentials.client_secret.as_str());
    params.insert("code", code);
    params.insert("grant_type", "authorization_code");
    params.insert("redirect_uri", redirect_uri);

    let response = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to exchange code: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Token exchange failed: {}", error_text));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let expires_at = token_response.expires_in.map(|expires_in| {
        chrono::Utc::now().timestamp() + expires_in
    });

    Ok(YouTubeTokens {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        expires_at,
    })
}

pub async fn refresh_access_token(
    credentials: &YouTubeCredentials,
    refresh_token: &str,
) -> Result<YouTubeTokens, String> {
    let client = Client::new();

    let mut params = HashMap::new();
    params.insert("client_id", credentials.client_id.as_str());
    params.insert("client_secret", credentials.client_secret.as_str());
    params.insert("refresh_token", refresh_token);
    params.insert("grant_type", "refresh_token");

    let response = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Token refresh failed: {}", error_text));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let expires_at = token_response.expires_in.map(|expires_in| {
        chrono::Utc::now().timestamp() + expires_in
    });

    Ok(YouTubeTokens {
        access_token: token_response.access_token,
        refresh_token: Some(refresh_token.to_string()), // Keep the original refresh token
        expires_at,
    })
}

pub async fn upload_video(
    access_token: &str,
    video_path: &str,
    title: &str,
    description: &str,
    privacy: &str,
    app_handle: Option<&tauri::AppHandle>,
) -> Result<UploadResponse, String> {
    let client = Client::new();

    // Get file size
    let file_metadata = std::fs::metadata(video_path)
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;
    let file_size = file_metadata.len();

    println!("[youtube] Starting upload: {} ({:.1} MB)", video_path, file_size as f64 / 1024.0 / 1024.0);

    // Emit initial progress
    if let Some(handle) = app_handle {
        let _ = handle.emit("youtube-upload-progress", serde_json::json!({
            "progress": 0,
            "status": "preparing"
        }));
    }

    // Create metadata for resumable upload
    let metadata = VideoMetadata {
        snippet: VideoSnippet {
            title: title.to_string(),
            description: description.to_string(),
            category_id: "22".to_string(), // People & Blogs
            tags: vec!["sermon".to_string(), "church".to_string()],
        },
        status: VideoStatus {
            privacy_status: privacy.to_string(),
            self_declared_made_for_kids: false,
        },
    };

    let metadata_json = serde_json::to_string(&metadata)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

    // Step 1: Initiate resumable upload session
    println!("[youtube] Initiating resumable upload session...");
    let init_response = client
        .post(format!(
            "{}?uploadType=resumable&part=snippet,status",
            YOUTUBE_UPLOAD_URL
        ))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Length", file_size.to_string())
        .header("X-Upload-Content-Type", "video/mp4")
        .body(metadata_json)
        .send()
        .await
        .map_err(|e| format!("Failed to initiate upload: {}", e))?;

    if !init_response.status().is_success() {
        let error_text = init_response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Failed to initiate upload: {}", error_text));
    }

    // Get the upload URI from the Location header
    let upload_uri = init_response
        .headers()
        .get("location")
        .ok_or("No upload URI in response")?
        .to_str()
        .map_err(|e| format!("Invalid upload URI: {}", e))?
        .to_string();


    // Step 2: Upload in chunks (8MB each) - never loads entire file into memory
    const CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8MB chunks
    let mut uploaded: u64 = 0;
    let mut file = File::open(video_path)
        .await
        .map_err(|e| format!("Failed to open video file: {}", e))?;

    while uploaded < file_size {
        let chunk_end = std::cmp::min(uploaded + CHUNK_SIZE, file_size);
        let chunk_len = (chunk_end - uploaded) as usize;

        // Read only this chunk into memory
        let mut chunk = vec![0u8; chunk_len];
        file.read_exact(&mut chunk)
            .await
            .map_err(|e| format!("Failed to read chunk: {}", e))?;

        // Upload chunk with retry logic
        let content_range = format!("bytes {}-{}/{}", uploaded, chunk_end - 1, file_size);

        let mut last_error = String::new();
        let mut chunk_response = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(2u64.pow(attempt));
                println!("[youtube] Retry attempt {} after {:?}...", attempt, delay);
                tokio::time::sleep(delay).await;
            }

            match client
                .put(&upload_uri)
                .header("Content-Length", chunk_len.to_string())
                .header("Content-Range", &content_range)
                .body(chunk.clone())
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_server_error() && attempt < 2 {
                        last_error = format!("Server error {}", resp.status());
                        println!("[youtube] Chunk upload got {}, will retry", resp.status());
                        continue;
                    }
                    chunk_response = Some(resp);
                    break;
                }
                Err(e) => {
                    last_error = format!("Failed to upload chunk: {}", e);
                    if attempt == 2 {
                        return Err(last_error);
                    }
                    println!("[youtube] Chunk upload failed: {}, will retry", e);
                }
            }
        }

        let chunk_response = chunk_response.ok_or(last_error)?;
        let status = chunk_response.status();

        // 308 Resume Incomplete = chunk uploaded, continue
        // 200/201 = upload complete
        if status.as_u16() == 308 {
            uploaded = chunk_end;
            let progress = ((uploaded as f64 / file_size as f64) * 95.0) as u32;

            if let Some(handle) = app_handle {
                let _ = handle.emit("youtube-upload-progress", serde_json::json!({
                    "progress": progress,
                    "status": "uploading"
                }));
            }
        } else if status.is_success() {
            // Upload complete
            if let Some(handle) = app_handle {
                let _ = handle.emit("youtube-upload-progress", serde_json::json!({
                    "progress": 98,
                    "status": "processing"
                }));
            }

            let upload_response: UploadResponse = chunk_response
                .json()
                .await
                .map_err(|e| format!("Failed to parse upload response: {}", e))?;

            if let Some(handle) = app_handle {
                let _ = handle.emit("youtube-upload-progress", serde_json::json!({
                    "progress": 100,
                    "status": "complete"
                }));
            }

            println!("[youtube] Video uploaded successfully: {}", upload_response.id);
            return Ok(upload_response);
        } else {
            let error_text = chunk_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!("Chunk upload failed ({}): {}", status, error_text));
        }
    }

    Err("Upload completed but no response received".to_string())
}

pub async fn set_thumbnail(
    access_token: &str,
    video_id: &str,
    thumbnail_path: &str,
) -> Result<(), String> {
    let client = Client::new();

    // Read thumbnail file
    let thumbnail_data = std::fs::read(thumbnail_path)
        .map_err(|e| format!("Failed to read thumbnail: {}", e))?;

    let mime_type = if thumbnail_path.ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    };

    println!("[youtube] Uploading thumbnail: {} ({} bytes, {})", thumbnail_path, thumbnail_data.len(), mime_type);

    let response = client
        .post(format!("{}?videoId={}&uploadType=media", YOUTUBE_THUMBNAILS_URL, video_id))
        .bearer_auth(access_token)
        .header("Content-Type", mime_type)
        .header("Content-Length", thumbnail_data.len().to_string())
        .body(thumbnail_data)
        .send()
        .await
        .map_err(|e| format!("Failed to upload thumbnail: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Thumbnail upload failed: {}", error_text));
    }

    println!("[youtube] Thumbnail set successfully for video {}", video_id);

    Ok(())
}

const YOUTUBE_CAPTIONS_URL: &str = "https://www.googleapis.com/upload/youtube/v3/captions";

/// Generate SRT content from transcript segments, adjusted for trim offset
pub fn generate_srt(segments: &[crate::state::TranscriptSegment], start_offset: f64, end_time: f64) -> String {
    let mut srt_lines = Vec::new();
    let mut counter = 1;

    for seg in segments {
        // Skip segments outside the trim range
        if seg.end < start_offset || seg.start > end_time {
            continue;
        }

        // Adjust timestamps relative to trimmed video
        let mut start = seg.start - start_offset;
        let end = seg.end - start_offset;
        if start < 0.0 {
            start = 0.0;
        }

        // Format as SRT timestamp: HH:MM:SS,mmm
        let start_ts = format_srt_timestamp(start);
        let end_ts = format_srt_timestamp(end);

        srt_lines.push(counter.to_string());
        srt_lines.push(format!("{} --> {}", start_ts, end_ts));
        srt_lines.push(seg.text.trim().to_string());
        srt_lines.push(String::new()); // Empty line between entries

        counter += 1;
    }

    println!("[youtube] Generated {} caption segments", counter - 1);
    srt_lines.join("\n")
}

fn format_srt_timestamp(seconds: f64) -> String {
    let hours = (seconds / 3600.0).floor() as u32;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as u32;
    let secs = (seconds % 60.0).floor() as u32;
    let millis = ((seconds % 1.0) * 1000.0).floor() as u32;
    format!("{:02}:{:02}:{:02},{:03}", hours, minutes, secs, millis)
}

pub async fn upload_captions(
    access_token: &str,
    video_id: &str,
    srt_content: &str,
) -> Result<(), String> {
    let client = Client::new();

    println!("[youtube] Uploading captions for video {} ({} bytes)", video_id, srt_content.len());

    // Create multipart form
    let metadata = serde_json::json!({
        "snippet": {
            "videoId": video_id,
            "language": "en",
            "name": "English",
            "isDraft": false
        }
    });

    let boundary = format!("caption_boundary_{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();

    // Add metadata part
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(metadata.to_string().as_bytes());
    body.extend_from_slice(b"\r\n");

    // Add SRT file part
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: text/srt\r\n\r\n");
    body.extend_from_slice(srt_content.as_bytes());
    body.extend_from_slice(b"\r\n");

    // End boundary
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let response = client
        .post(format!("{}?part=snippet", YOUTUBE_CAPTIONS_URL))
        .bearer_auth(access_token)
        .header("Content-Type", format!("multipart/related; boundary={}", boundary))
        .header("Content-Length", body.len().to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Failed to upload captions: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Caption upload failed: {}", error_text));
    }

    println!("[youtube] Captions uploaded successfully for video {}", video_id);
    Ok(())
}

pub async fn get_channel_info(access_token: &str) -> Result<ChannelInfo, String> {
    let client = Client::new();

    let response = client
        .get("https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Failed to get channel info: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Failed to get channel info: {}", error_text));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse channel response: {}", e))?;

    let items = json["items"]
        .as_array()
        .ok_or("No channel found")?;

    if items.is_empty() {
        return Err("No channel found for this account".to_string());
    }

    let channel = &items[0];
    let snippet = &channel["snippet"];

    Ok(ChannelInfo {
        id: channel["id"].as_str().unwrap_or("").to_string(),
        title: snippet["title"].as_str().unwrap_or("").to_string(),
        thumbnail_url: snippet["thumbnails"]["default"]["url"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: String,
    pub title: String,
    pub thumbnail_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistInfo {
    pub id: String,
    pub title: String,
}

pub async fn list_playlists(access_token: &str) -> Result<Vec<PlaylistInfo>, String> {
    let client = Client::new();

    let response = client
        .get("https://www.googleapis.com/youtube/v3/playlists?part=snippet&mine=true&maxResults=50")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Failed to list playlists: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Failed to list playlists: {}", error_text));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse playlists response: {}", e))?;

    let items = json["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            let title = item["snippet"]["title"].as_str()?;
            Some(PlaylistInfo {
                id: id.to_string(),
                title: title.to_string(),
            })
        })
        .collect();

    Ok(items)
}

pub async fn add_to_playlist(
    access_token: &str,
    playlist_id: &str,
    video_id: &str,
) -> Result<(), String> {
    let client = Client::new();

    let body = serde_json::json!({
        "snippet": {
            "playlistId": playlist_id,
            "resourceId": {
                "kind": "youtube#video",
                "videoId": video_id
            }
        }
    });

    let response = client
        .post("https://www.googleapis.com/youtube/v3/playlistItems?part=snippet")
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("Failed to add to playlist: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Failed to add to playlist: {}", error_text));
    }

    println!("[youtube] Video {} added to playlist {}", video_id, playlist_id);
    Ok(())
}
