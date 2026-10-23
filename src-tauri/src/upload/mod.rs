//! YouTube uploads: the resumable protocol (`client`), sessions that
//! survive a restart (`session`) and the steps after the video is up
//! (`post`).
//!
//! Everything that touches the network takes an `UploadConfig`, so tests
//! point it at a local mock server (`test_server`) with millisecond
//! timeouts and an instant sleep.

pub mod client;
pub mod error;
pub mod post;
pub mod session;
#[cfg(test)]
pub mod test_server;
#[cfg(test)]
mod mock_tests;

pub use error::UploadError;
pub use post::{AfterUpload, UploadWarning, WarningKind};
pub use session::{SessionStore, UploadSession};

use futures::future::BoxFuture;
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Where requests go. Production uses `youtube()`; tests a mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub upload: String,
    pub thumbnails: String,
    pub captions: String,
    pub playlist_items: String,
}

impl Endpoints {
    pub fn youtube() -> Self {
        Endpoints {
            upload: "https://www.googleapis.com/upload/youtube/v3/videos".into(),
            thumbnails: "https://www.googleapis.com/upload/youtube/v3/thumbnails/set".into(),
            captions: "https://www.googleapis.com/upload/youtube/v3/captions".into(),
            playlist_items: "https://www.googleapis.com/youtube/v3/playlistItems".into(),
        }
    }

    /// Every endpoint under one base URL (the mock server).
    #[cfg(test)]
    pub fn at(base: &str) -> Self {
        Endpoints {
            upload: format!("{}/upload", base),
            thumbnails: format!("{}/thumbnails", base),
            captions: format!("{}/captions", base),
            playlist_items: format!("{}/playlistItems", base),
        }
    }
}

/// Exponential backoff with jitter: `initial`, doubling up to `max`, each
/// delay drawn from [½, 1] of that. A `Retry-After` from the server wins
/// (capped at `max_retry_after`).
#[derive(Debug, Clone)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub max_retry_after: Duration,
}

impl Backoff {
    /// The pause before retry number `attempt` (1-based).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(r) = retry_after {
            return r.min(self.max_retry_after);
        }
        let exp = self.initial.saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
        let ceiling = exp.min(self.max);
        ceiling.mul_f64(0.5 + 0.5 * jitter())
    }
}

/// A number in [0, 1), random enough for spreading retries.
fn jitter() -> f64 {
    (uuid::Uuid::new_v4().as_u128() as u64 >> 11) as f64 / (1u64 << 53) as f64
}

pub type SleepFn = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct UploadConfig {
    pub endpoints: Endpoints,
    /// Bytes per PUT; a multiple of 256 KiB, as YouTube requires.
    pub chunk_size: u64,
    /// Give up on a request when nothing moved for this long.
    pub stall_timeout: Duration,
    pub backoff: Backoff,
    /// Failed tries in a row (without progress in between) before giving up.
    pub max_attempts: u32,
    /// How retries wait; tests make it instant and record the delays.
    pub sleep: SleepFn,
}

impl Default for UploadConfig {
    fn default() -> Self {
        UploadConfig {
            endpoints: Endpoints::youtube(),
            chunk_size: 8 * 1024 * 1024,
            stall_timeout: Duration::from_secs(90),
            backoff: Backoff {
                initial: Duration::from_secs(2),
                max: Duration::from_secs(60),
                max_retry_after: Duration::from_secs(10 * 60),
            },
            max_attempts: 8,
            sleep: Arc::new(|d| Box::pin(tokio::time::sleep(d))),
        }
    }
}

/// Access tokens for the requests. `refresh` is called after a 401.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> BoxFuture<'_, Result<String, UploadError>>;
    fn refresh(&self) -> BoxFuture<'_, Result<String, UploadError>>;
}

/// The one HTTP client for YouTube: a connect timeout but no total timeout
/// (a chunk on a slow uplink can take minutes; stalls are caught instead),
/// and no redirects (a 308 is the resumable protocol's "keep going").
pub fn http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(build_client).clone()
}

pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// A finished upload, as stored on the library entry and shown to the user.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UploadOutcome {
    pub url: String,
    pub video_id: String,
    pub warnings: Vec<UploadWarning>,
}

/// YouTube video ids are short and URL-safe.
pub fn is_valid_video_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn watch_url(video_id: &str) -> String {
    format!("https://youtube.com/watch?v={}", video_id)
}

/// Upload the video, then set its thumbnail, captions and playlist with a
/// fresh token.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    http: &reqwest::Client,
    cfg: &UploadConfig,
    tokens: &dyn TokenSource,
    store: &dyn SessionStore,
    cancel: &CancellationToken,
    video: &client::VideoUpload<'_>,
    after: &AfterUpload,
    on_progress: client::Progress<'_>,
) -> Result<UploadOutcome, UploadError> {
    let uploader = client::Uploader { http, cfg, tokens, cancel };
    let video_id = uploader.upload(store, video, on_progress).await?;
    println!("[upload] Video uploaded: {}", video_id);
    let warnings = post::after_upload(http, &cfg.endpoints, tokens, &video_id, after).await;
    Ok(UploadOutcome { url: watch_url(&video_id), video_id, warnings })
}

/// MIME type YouTube is told about, from the file's extension.
pub fn content_type(path: &Path) -> &'static str {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mts" | "m2ts" => "video/mp2t",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_follows_the_extension() {
        assert_eq!(content_type(Path::new("/a/b.MOV")), "video/quicktime");
        assert_eq!(content_type(Path::new("x.mkv")), "video/x-matroska");
        assert_eq!(content_type(Path::new("trimmed.mp4")), "video/mp4");
        assert_eq!(content_type(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn backoff_grows_with_jitter_and_honours_retry_after() {
        let b = UploadConfig::default().backoff;
        for attempt in 1..10 {
            let d = b.delay(attempt, None);
            let ceiling = (Duration::from_secs(2) * (1 << (attempt - 1))).min(Duration::from_secs(60));
            assert!(d <= ceiling && d >= ceiling / 2, "attempt {}: {:?}", attempt, d);
        }
        assert_eq!(b.delay(1, Some(Duration::from_secs(7))), Duration::from_secs(7));
        assert_eq!(b.delay(1, Some(Duration::from_secs(99999))), Duration::from_secs(600));
    }
}
