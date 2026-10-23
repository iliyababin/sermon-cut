use crate::upload::UploadError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

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
        urlencoding::encode("https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube https://www.googleapis.com/auth/youtube.force-ssl"),
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
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &auth_url])
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

/// A new access token from the refresh token. An `invalid_grant` (revoked
/// or expired sign-in) comes back as `UploadError::Auth`.
pub async fn refresh_access_token(
    credentials: &YouTubeCredentials,
    refresh_token: &str,
) -> Result<YouTubeTokens, UploadError> {
    let mut params = HashMap::new();
    params.insert("client_id", credentials.client_id.as_str());
    params.insert("client_secret", credentials.client_secret.as_str());
    params.insert("refresh_token", refresh_token);
    params.insert("grant_type", "refresh_token");

    let response = crate::upload::http_client()
        .post(TOKEN_URL)
        .timeout(std::time::Duration::from_secs(60))
        .form(&params)
        .send()
        .await
        .map_err(UploadError::from_reqwest)?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(UploadError::from_response(status, body, None, false));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|e| UploadError::Protocol(format!("token response: {}", e)))?;

    let expires_at = token_response.expires_in.map(|expires_in| {
        chrono::Utc::now().timestamp() + expires_in
    });

    Ok(YouTubeTokens {
        access_token: token_response.access_token,
        refresh_token: Some(refresh_token.to_string()), // Keep the original refresh token
        expires_at,
    })
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
