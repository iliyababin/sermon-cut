use crate::upload::UploadError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

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

/// Google's sign-in endpoints; tests use a local mock.
#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub auth: String,
    pub token: String,
}

impl OAuthEndpoints {
    pub fn google() -> Self {
        OAuthEndpoints { auth: AUTH_URL.into(), token: TOKEN_URL.into() }
    }
}

pub const SIGN_IN_CANCELLED: &str = "Sign-in was cancelled.";
pub const SIGN_IN_TIMED_OUT: &str = "Sign-in took too long. Click Sign in to try again.";
pub const SIGN_IN_REPLACED: &str = "A newer sign-in was started.";

/// How one sign-in runs.
pub struct OAuthFlow {
    pub endpoints: OAuthEndpoints,
    /// Give up when the browser hasn't come back by then.
    pub timeout: std::time::Duration,
    /// Opens the authorization URL (the browser in the app, a fake in tests).
    pub open_browser: Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    /// Cancelled when a newer sign-in starts.
    pub cancel: CancellationToken,
}

impl OAuthFlow {
    pub fn new(cancel: CancellationToken) -> Self {
        OAuthFlow {
            endpoints: OAuthEndpoints::google(),
            timeout: std::time::Duration::from_secs(5 * 60),
            open_browser: Box::new(open_in_browser),
            cancel,
        }
    }
}

fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    result.map(|_| ()).map_err(|e| format!("Failed to open browser: {}", e))
}

/// Only one sign-in listens at a time: starting one cancels the one before
/// (whose browser tab may have been closed).
#[derive(Default)]
pub struct SignInSlot {
    current: std::sync::Mutex<Option<(u64, CancellationToken)>>,
    next: std::sync::atomic::AtomicU64,
}

impl SignInSlot {
    pub fn begin(&self) -> (u64, CancellationToken) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let token = CancellationToken::new();
        let previous = self.current.lock().unwrap().replace((id, token.clone()));
        if let Some((_, previous)) = previous {
            previous.cancel();
        }
        (id, token)
    }

    pub fn end(&self, id: u64) {
        let mut current = self.current.lock().unwrap();
        if current.as_ref().is_some_and(|(c, _)| *c == id) {
            *current = None;
        }
    }
}

/// What a request to the loopback listener was.
#[derive(Debug, PartialEq)]
enum Callback {
    Code(String),
    /// Google sent the user back with `error=` (they clicked Cancel).
    Denied(String),
    /// Not the callback: a favicon, a preconnect, a wrong state.
    Other,
}

fn parse_callback(request_line: &str, expected_state: &str) -> Callback {
    let Some(target) = request_line.split_whitespace().nth(1) else { return Callback::Other };
    let Some((_, query)) = target.split_once('?') else { return Callback::Other };
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for param in query.split('&') {
        let Some((key, value)) = param.split_once('=') else { continue };
        let Ok(value) = urlencoding::decode(&value.replace('+', " ")).map(|v| v.to_string()) else { continue };
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            _ => {}
        }
    }
    // Anything without our state could come from any page in the browser.
    if state.as_deref() != Some(expected_state) {
        return Callback::Other;
    }
    match (error, code) {
        (Some(e), _) => Callback::Denied(e),
        (None, Some(c)) if !c.is_empty() => Callback::Code(c),
        _ => Callback::Other,
    }
}

const PAGE_DONE: &str = "<html><body style=\"font-family:-apple-system,sans-serif;text-align:center;padding:60px\"><h1>Signed in to YouTube</h1><p>You can close this window and return to Sermon Cut.</p><script>window.close();</script></body></html>";

fn page(title: &str, text: &str) -> String {
    format!(
        "<html><body style=\"font-family:-apple-system,sans-serif;text-align:center;padding:60px\"><h1>{}</h1><p>{}</p></body></html>",
        title,
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    )
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The request line of one connection (headers are read and dropped).
async fn read_request_line(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = async {
        reader.read_line(&mut line).await.ok()?;
        loop {
            let mut header = String::new();
            let n = reader.read_line(&mut header).await.ok()?;
            if n == 0 || header == "\r\n" || header == "\n" {
                break;
            }
        }
        Some(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), read).await.ok()??;
    Some(line)
}

/// Sign in with the system browser. Waits up to five minutes; a newer
/// sign-in cancels this one.
pub async fn start_oauth_flow(credentials: &YouTubeCredentials, cancel: CancellationToken) -> Result<YouTubeTokens, String> {
    run_oauth_flow(credentials, &OAuthFlow::new(cancel)).await
}

pub async fn run_oauth_flow(credentials: &YouTubeCredentials, flow: &OAuthFlow) -> Result<YouTubeTokens, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind to port: {}", e))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{}", port);

    // CSRF protection: only a callback carrying this state counts.
    let oauth_state = uuid::Uuid::new_v4().to_string();
    // PKCE: the code is only worth anything with this verifier.
    let verifier = pkce_verifier();

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}&code_challenge={}&code_challenge_method=S256",
        flow.endpoints.auth,
        urlencoding::encode(&credentials.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode("https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube https://www.googleapis.com/auth/youtube.force-ssl"),
        urlencoding::encode(&oauth_state),
        pkce_challenge(&verifier)
    );

    println!("[youtube] Opening browser for sign-in; waiting on port {}", port);
    (flow.open_browser)(&auth_url)?;

    // Connections are read side by side, so an idle preconnect can't hold
    // up the real callback.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(tokio::net::TcpStream, String)>(16);
    let accept_loop = async {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.map_err(|e| format!("Failed to accept connection: {}", e))?;
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Some(line) = read_request_line(&mut stream).await {
                            let _ = tx.send((stream, line)).await;
                        }
                    });
                }
                Some((mut stream, line)) = rx.recv() => match parse_callback(&line, &oauth_state) {
                    Callback::Other => respond(&mut stream, "404 Not Found", "").await,
                    Callback::Denied(error) => {
                        println!("[youtube] Sign-in ended in the browser: {}", error);
                        respond(&mut stream, "200 OK", &page("Sign-in cancelled", "You can close this window.")).await;
                        return Err(SIGN_IN_CANCELLED.to_string());
                    }
                    Callback::Code(code) => {
                        // Exchange first, so the page tells the truth.
                        let result = exchange_code_for_tokens(credentials, &code, &verifier, &redirect_uri, &flow.endpoints.token).await;
                        match &result {
                            Ok(_) => respond(&mut stream, "200 OK", PAGE_DONE).await,
                            Err(e) => respond(&mut stream, "200 OK", &page("Sign-in failed", e)).await,
                        }
                        return result;
                    }
                },
            }
        }
    };

    tokio::select! {
        r = accept_loop => r,
        _ = tokio::time::sleep(flow.timeout) => Err(SIGN_IN_TIMED_OUT.to_string()),
        _ = flow.cancel.cancelled() => Err(SIGN_IN_REPLACED.to_string()),
    }
}

/// A PKCE code verifier: 64 random URL-safe characters (RFC 7636 §4.1).
fn pkce_verifier() -> String {
    use base64::Engine;
    let bytes: Vec<u8> = (0..3).flat_map(|_| uuid::Uuid::new_v4().into_bytes()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes[..48])
}

/// The S256 challenge for a verifier: base64url(SHA-256(verifier)).
fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

async fn exchange_code_for_tokens(
    credentials: &YouTubeCredentials,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    token_url: &str,
) -> Result<YouTubeTokens, String> {
    let mut params = HashMap::new();
    params.insert("client_id", credentials.client_id.as_str());
    params.insert("client_secret", credentials.client_secret.as_str());
    params.insert("code", code);
    params.insert("code_verifier", code_verifier);
    params.insert("grant_type", "authorization_code");
    params.insert("redirect_uri", redirect_uri);

    let response = crate::upload::http_client()
        .post(token_url)
        .timeout(std::time::Duration::from_secs(60))
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Google to finish signing in: {}", e))?;

    if !response.status().is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("Signing in failed: {}", error_text));
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

/// A new access token from the refresh token, from `token_url`. An
/// `invalid_grant` (revoked or expired sign-in) comes back as
/// `UploadError::Auth`.
pub async fn refresh_access_token_at(
    credentials: &YouTubeCredentials,
    refresh_token: &str,
    token_url: &str,
) -> Result<YouTubeTokens, UploadError> {
    let mut params = HashMap::new();
    params.insert("client_id", credentials.client_id.as_str());
    params.insert("client_secret", credentials.client_secret.as_str());
    params.insert("refresh_token", refresh_token);
    params.insert("grant_type", "refresh_token");

    let response = crate::upload::http_client()
        .post(token_url)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::test_server::MockServer;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn credentials() -> YouTubeCredentials {
        YouTubeCredentials { client_id: "client".into(), client_secret: "secret".into() }
    }

    /// A sign-in against the mock token endpoint. Returns the running flow
    /// and, once the "browser" opened, the redirect URI and state.
    struct Running {
        handle: tokio::task::JoinHandle<Result<YouTubeTokens, String>>,
        redirect: String,
        state: String,
        challenge: String,
        challenge_method: String,
    }

    async fn start(server: &MockServer, timeout: Duration, cancel: CancellationToken) -> Running {
        let (seen, mut opened) = tokio::sync::mpsc::unbounded_channel::<String>();
        let flow = OAuthFlow {
            endpoints: OAuthEndpoints { auth: format!("{}/auth", server.base), token: format!("{}/token", server.base) },
            timeout,
            open_browser: Box::new(move |url| {
                let _ = seen.send(url.to_string());
                Ok(())
            }),
            cancel,
        };
        let handle = tokio::spawn(async move { run_oauth_flow(&credentials(), &flow).await });
        let url = tokio::time::timeout(Duration::from_secs(10), opened.recv()).await.unwrap().unwrap();
        let param = |name: &str| {
            let (_, query) = url.split_once('?').unwrap();
            let value = query.split('&').find_map(|p| p.strip_prefix(&format!("{}=", name))).unwrap();
            urlencoding::decode(value).unwrap().to_string()
        };
        assert!(url.starts_with(&format!("{}/auth?", server.base)));
        Running {
            handle,
            redirect: param("redirect_uri"),
            state: param("state"),
            challenge: param("code_challenge"),
            challenge_method: param("code_challenge_method"),
        }
    }

    async fn get(url: &str) -> (u16, String, String) {
        let resp = reqwest::Client::new().get(url).send().await.unwrap();
        let status = resp.status().as_u16();
        let connection = resp.headers().get("connection").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        (status, connection, resp.text().await.unwrap())
    }

    #[test]
    fn recognises_the_callback() {
        let line = |q: &str| format!("GET {} HTTP/1.1\r\n", q);
        assert_eq!(parse_callback(&line("/favicon.ico"), "s1"), Callback::Other);
        assert_eq!(parse_callback(&line("/?code=abc&state=s2"), "s1"), Callback::Other);
        assert_eq!(parse_callback(&line("/?code=abc"), "s1"), Callback::Other);
        assert_eq!(parse_callback(&line("/?state=s1"), "s1"), Callback::Other);
        assert_eq!(parse_callback(&line("/?code=4%2Fabc&state=s1&scope=x"), "s1"), Callback::Code("4/abc".into()));
        assert_eq!(parse_callback(&line("/?error=access_denied&state=s1"), "s1"), Callback::Denied("access_denied".into()));
        // An error from some other page doesn't end the sign-in.
        assert_eq!(parse_callback(&line("/?error=access_denied&state=nope"), "s1"), Callback::Other);
        assert_eq!(parse_callback("garbage", "s1"), Callback::Other);
    }

    #[tokio::test]
    async fn ignores_junk_then_signs_in() {
        let server = MockServer::start().await;
        let run = start(&server, Duration::from_secs(10), CancellationToken::new()).await;
        // A browser preconnect that never sends anything.
        let _idle = tokio::net::TcpStream::connect(run.redirect.trim_start_matches("http://")).await.unwrap();
        assert_eq!(get(&format!("{}/favicon.ico", run.redirect)).await.0, 404);
        assert_eq!(get(&format!("{}/?code=abc&state=wrong", run.redirect)).await.0, 404);
        assert_eq!(get(&format!("{}/?state={}", run.redirect, run.state)).await.0, 404);

        let (status, connection, body) = get(&format!("{}/?code=abc&state={}", run.redirect, run.state)).await;
        assert_eq!(status, 200);
        assert_eq!(connection, "close");
        assert!(body.contains("Signed in"), "{}", body);
        // The page came after the exchange.
        let requests = server.with(|s| s.token_requests.clone());
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("code=abc") && requests[0].contains("grant_type=authorization_code"));

        let tokens = run.handle.await.unwrap().unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn the_code_is_exchanged_with_the_pkce_verifier() {
        use base64::Engine;
        use sha2::Digest;
        let server = MockServer::start().await;
        let run = start(&server, Duration::from_secs(10), CancellationToken::new()).await;
        assert_eq!(run.challenge_method, "S256");
        get(&format!("{}/?code=abc&state={}", run.redirect, run.state)).await;
        run.handle.await.unwrap().unwrap();

        let form = server.with(|s| s.token_requests[0].clone());
        let verifier = form
            .split('&')
            .find_map(|p| p.strip_prefix("code_verifier="))
            .map(|v| urlencoding::decode(v).unwrap().to_string())
            .expect("a code_verifier was sent");
        assert!((43..=128).contains(&verifier.len()), "{}", verifier);
        assert!(verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)));
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        assert_eq!(run.challenge, expected);

        // Each sign-in has its own.
        let again = start(&server, Duration::from_secs(10), CancellationToken::new()).await;
        assert_ne!(again.challenge, run.challenge);
        again.handle.abort();
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // Computed independently (Python's hashlib and base64).
        assert_eq!(
            pkce_challenge("sermon-cut-pkce-test-verifier-0123456789-abcdefghijklmnop"),
            "9Y03p2lF0DX5upK6YUJtnvjBvLJWsR2qtE5Ahiu359A"
        );
    }

    #[tokio::test]
    async fn access_denied_ends_the_sign_in() {
        let server = MockServer::start().await;
        let run = start(&server, Duration::from_secs(10), CancellationToken::new()).await;
        let (status, _, body) = get(&format!("{}/?error=access_denied&state={}", run.redirect, run.state)).await;
        assert_eq!(status, 200);
        assert!(body.contains("cancelled"));
        assert_eq!(run.handle.await.unwrap().unwrap_err(), SIGN_IN_CANCELLED);
        assert!(server.with(|s| s.token_requests.is_empty()));
    }

    #[tokio::test]
    async fn a_failed_exchange_says_so_in_the_browser() {
        let server = MockServer::start().await;
        server.with(|s| s.token_reply = (400, r#"{"error":"invalid_grant"}"#.into()));
        let run = start(&server, Duration::from_secs(10), CancellationToken::new()).await;
        let (_, _, body) = get(&format!("{}/?code=abc&state={}", run.redirect, run.state)).await;
        assert!(body.contains("Sign-in failed"), "{}", body);
        assert!(run.handle.await.unwrap().unwrap_err().contains("invalid_grant"));
    }

    #[tokio::test]
    async fn gives_up_after_the_timeout() {
        let server = MockServer::start().await;
        let run = start(&server, Duration::from_millis(150), CancellationToken::new()).await;
        assert_eq!(run.handle.await.unwrap().unwrap_err(), SIGN_IN_TIMED_OUT);
        // The listener is gone.
        assert!(reqwest::get(format!("{}/?code=abc&state={}", run.redirect, run.state)).await.is_err());
    }

    #[tokio::test]
    async fn a_second_sign_in_cancels_the_first() {
        let server = MockServer::start().await;
        let slot = SignInSlot::default();
        let (first_id, first) = slot.begin();
        let first_run = start(&server, Duration::from_secs(10), first).await;
        let (second_id, second) = slot.begin();
        assert_eq!(first_run.handle.await.unwrap().unwrap_err(), SIGN_IN_REPLACED);
        // The first one finishing doesn't clear the second's slot.
        slot.end(first_id);
        assert!(!second.is_cancelled());
        let second_run = start(&server, Duration::from_secs(10), second).await;
        get(&format!("{}/?code=abc&state={}", second_run.redirect, second_run.state)).await;
        assert!(second_run.handle.await.unwrap().is_ok());
        slot.end(second_id);
        assert!(slot.current.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn refresh_maps_a_revoked_sign_in_to_auth() {
        let server = MockServer::start().await;
        let url = format!("{}/token", server.base);
        let tokens = refresh_access_token_at(&credentials(), "r1", &url).await.unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("r1"));
        assert!(server.with(|s| s.token_requests[0].contains("grant_type=refresh_token")));

        server.with(|s| s.token_reply = (400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into()));
        let err = refresh_access_token_at(&credentials(), "r1", &url).await.unwrap_err();
        assert!(matches!(err, UploadError::Auth(_)));
        assert_eq!(err.user_message(), crate::upload::error::SIGN_IN_AGAIN);
    }
}
