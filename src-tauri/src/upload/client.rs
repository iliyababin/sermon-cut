//! YouTube's resumable upload protocol, made to survive real networks.
//!
//! - A session is created once and kept in a `SessionStore`, so a later
//!   attempt (even after a restart) continues it.
//! - Before resuming, the server is asked how much it has
//!   (`Content-Range: bytes */N`): 200/201 means it's already complete,
//!   308 gives the next offset, 404/410 means the session is gone.
//! - Chunks are retried with backoff (honouring `Retry-After`); the count
//!   of failures resets whenever a chunk makes progress.
//! - A request is abandoned when no bytes moved for the stall timeout,
//!   not after a fixed total time, so slow uplinks still work.
//! - A 401 refreshes the token once and carries on.

use super::error::{parse_retry_after, UploadError};
use super::session::{metadata_hash, SessionStore, UploadSession};
use super::{content_type, TokenSource, UploadConfig};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::Client;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;

/// Pieces the chunk body is streamed in; each one counts as progress.
const PIECE: usize = 64 * 1024;

/// What to upload.
pub struct VideoUpload<'a> {
    pub path: &'a Path,
    /// The library entry's fingerprint hash.
    pub file_hash: &'a str,
    /// `{snippet, status}` for the new video.
    pub metadata: serde_json::Value,
}

/// The new video's snippet and status.
pub fn video_metadata(title: &str, description: &str, privacy: &str) -> serde_json::Value {
    serde_json::json!({
        "snippet": {
            "title": title,
            "description": description,
            "categoryId": "22", // People & Blogs
            "tags": ["sermon", "church"],
        },
        "status": {
            "privacyStatus": privacy,
            "selfDeclaredMadeForKids": false,
        },
    })
}

/// `(bytes sent, total)`.
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

enum Step {
    Created(UploadSession),
    /// The server has everything; the new video's id.
    Done(String),
    /// The server has this many bytes.
    Resume(u64),
}

/// Bytes moved during one request, for the stall check and progress.
struct Activity {
    start: Instant,
    last_ms: AtomicU64,
    sent: AtomicU64,
}

impl Activity {
    fn new() -> Arc<Self> {
        Arc::new(Activity { start: Instant::now(), last_ms: AtomicU64::new(0), sent: AtomicU64::new(0) })
    }
    fn touch(&self) {
        self.last_ms.store(self.start.elapsed().as_millis() as u64, Ordering::SeqCst);
    }
    fn idle(&self) -> Duration {
        self.start.elapsed().saturating_sub(Duration::from_millis(self.last_ms.load(Ordering::SeqCst)))
    }
}

/// The next byte the server wants, from a 308's `Range: bytes=0-N`.
pub fn parse_range(value: Option<&str>) -> Result<u64, UploadError> {
    let Some(value) = value else { return Ok(0) };
    let bad = || UploadError::Protocol(format!("unexpected Range header: {}", value));
    let range = value.trim().strip_prefix("bytes=").ok_or_else(bad)?;
    let (start, end) = range.split_once('-').ok_or_else(bad)?;
    let start: u64 = start.trim().parse().map_err(|_| bad())?;
    let end: u64 = end.trim().parse().map_err(|_| bad())?;
    if start != 0 || end < start {
        return Err(bad());
    }
    Ok(end + 1)
}

pub struct Uploader<'a> {
    pub http: &'a Client,
    pub cfg: &'a UploadConfig,
    pub tokens: &'a dyn TokenSource,
    pub cancel: &'a CancellationToken,
}

impl Uploader<'_> {
    /// Upload the video, continuing a stored session when it still fits.
    /// Returns the new video's id. The session is kept on failure or
    /// cancel, and completed on success, cleared when YouTube dropped it.
    pub async fn upload(&self, store: &dyn SessionStore, video: &VideoUpload<'_>, on_progress: Progress<'_>) -> Result<String, UploadError> {
        let meta = std::fs::metadata(video.path).map_err(|e| UploadError::File(e.to_string()))?;
        let size = meta.len();
        if size == 0 {
            return Err(UploadError::File("the video file is empty".into()));
        }
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let ctype = content_type(video.path);
        let metadata_json = video.metadata.to_string();
        let file_name = video.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mhash = metadata_hash(&metadata_json, ctype, &file_name, mtime_ms);

        let mut session = match store.load() {
            Some(s) if s.reusable_for(video.file_hash, size, &mhash, chrono::Utc::now()) => {
                println!("[upload] Continuing the upload session from {}", s.created);
                Some(s)
            }
            Some(_) => {
                store.save(None);
                None
            }
            None => None,
        };
        let mut file = tokio::fs::File::open(video.path).await.map_err(|e| UploadError::File(e.to_string()))?;
        let mut token = self.tokens.token().await?;
        // None: ask the server where to continue.
        let mut offset: Option<u64> = None;
        let mut confirmed: u64 = 0;
        let mut failures = 0u32;
        let mut refreshed = false;
        on_progress(0, size);

        loop {
            if self.cancel.is_cancelled() {
                return Err(UploadError::Cancelled);
            }
            let step = match (&session, offset) {
                (None, _) => self.create(&token, &metadata_json, size, ctype).await.map(|uri| {
                    Step::Created(UploadSession::new(uri, video.file_hash, size, &mhash))
                }),
                (Some(s), None) => self.query(&token, &s.uri, size).await,
                (Some(s), Some(off)) => {
                    self.put_chunk(&token, &s.uri, &mut file, off, size, &|sent| on_progress(off + sent, size)).await
                }
            };
            match step {
                Ok(Step::Created(s)) => {
                    store.save(Some(&s));
                    session = Some(s);
                    offset = Some(0);
                    confirmed = 0;
                }
                Ok(Step::Done(id)) => {
                    store.complete(&id);
                    on_progress(size, size);
                    return Ok(id);
                }
                Ok(Step::Resume(n)) if n >= size => {
                    return Err(UploadError::Protocol("YouTube has the whole file but didn't finish the upload".into()));
                }
                Ok(Step::Resume(n)) => {
                    if n > confirmed {
                        failures = 0;
                        refreshed = false;
                    }
                    confirmed = n;
                    offset = Some(n);
                    on_progress(n, size);
                }
                Err(UploadError::SessionGone) => {
                    println!("[upload] The upload session is gone; starting a new one");
                    store.save(None);
                    session = None;
                    offset = None;
                    failures += 1;
                    if failures >= self.cfg.max_attempts {
                        return Err(UploadError::SessionGone);
                    }
                }
                Err(UploadError::Auth(e)) => {
                    if refreshed {
                        return Err(UploadError::Auth(e));
                    }
                    refreshed = true;
                    token = self.tokens.refresh().await?;
                    offset = None;
                }
                Err(e) if e.is_retryable() => {
                    failures += 1;
                    if failures >= self.cfg.max_attempts {
                        return Err(e);
                    }
                    let delay = self.cfg.backoff.delay(failures, e.retry_after());
                    println!("[upload] {} (try {}); retrying in {:?}", e, failures, delay);
                    offset = None;
                    tokio::select! {
                        _ = (self.cfg.sleep)(delay) => {}
                        _ = self.cancel.cancelled() => return Err(UploadError::Cancelled),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Run a request, giving up when nothing moved for the stall timeout
    /// or the upload is cancelled. `tick` runs every half second.
    async fn guarded<T>(
        &self,
        activity: &Activity,
        fut: impl Future<Output = Result<T, UploadError>>,
        tick: &(dyn Fn() + Send + Sync),
    ) -> Result<T, UploadError> {
        let stall = self.cfg.stall_timeout;
        tokio::pin!(fut);
        loop {
            let idle = activity.idle();
            if idle >= stall {
                return Err(UploadError::Stalled);
            }
            let wait = (stall - idle).min(Duration::from_millis(500));
            tokio::select! {
                r = &mut fut => return r,
                _ = self.cancel.cancelled() => return Err(UploadError::Cancelled),
                _ = tokio::time::sleep(wait) => tick(),
            }
        }
    }

    /// Start a session; returns its URI.
    async fn create(&self, token: &str, metadata_json: &str, size: u64, ctype: &str) -> Result<String, UploadError> {
        let activity = Activity::new();
        let request = async {
            let resp = self
                .http
                .post(format!("{}?uploadType=resumable&part=snippet,status", self.cfg.endpoints.upload))
                .bearer_auth(token)
                .header(CONTENT_TYPE, "application/json; charset=UTF-8")
                .header("X-Upload-Content-Length", size.to_string())
                .header("X-Upload-Content-Type", ctype)
                .body(metadata_json.to_string())
                .send()
                .await
                .map_err(UploadError::from_reqwest)?;
            activity.touch();
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let retry_after = parse_retry_after(resp.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok()));
                let body = resp.text().await.unwrap_or_default();
                return Err(UploadError::from_response(status, body, retry_after, false));
            }
            resp.headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| UploadError::Protocol("no upload address in the reply".into()))
        };
        self.guarded(&activity, request, &|| {}).await
    }

    /// Ask how much of the file the session has.
    async fn query(&self, token: &str, uri: &str, size: u64) -> Result<Step, UploadError> {
        let activity = Activity::new();
        let request = async {
            let resp = self
                .http
                .put(uri)
                .bearer_auth(token)
                .header(CONTENT_RANGE, format!("bytes */{}", size))
                .header(CONTENT_LENGTH, "0")
                .send()
                .await
                .map_err(UploadError::from_reqwest)?;
            activity.touch();
            interpret(resp).await
        };
        self.guarded(&activity, request, &|| {}).await
    }

    /// Send the chunk starting at `offset`.
    async fn put_chunk(
        &self,
        token: &str,
        uri: &str,
        file: &mut tokio::fs::File,
        offset: u64,
        size: u64,
        on_sent: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<Step, UploadError> {
        let len = self.cfg.chunk_size.min(size - offset) as usize;
        let mut chunk = vec![0u8; len];
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| UploadError::File(e.to_string()))?;
        file.read_exact(&mut chunk).await.map_err(|e| UploadError::File(e.to_string()))?;

        let activity = Activity::new();
        let pieces: Vec<Vec<u8>> = chunk.chunks(PIECE).map(<[u8]>::to_vec).collect();
        let streamed = activity.clone();
        let body = futures::stream::iter(pieces.into_iter().map(move |p| {
            streamed.touch();
            streamed.sent.fetch_add(p.len() as u64, Ordering::SeqCst);
            Ok::<_, std::io::Error>(p)
        }));
        let request = async {
            let resp = self
                .http
                .put(uri)
                .bearer_auth(token)
                .header(CONTENT_LENGTH, len.to_string())
                .header(CONTENT_RANGE, format!("bytes {}-{}/{}", offset, offset + len as u64 - 1, size))
                .body(reqwest::Body::wrap_stream(body))
                .send()
                .await
                .map_err(UploadError::from_reqwest)?;
            activity.touch();
            interpret(resp).await
        };
        self.guarded(&activity, request, &|| on_sent(activity.sent.load(Ordering::SeqCst))).await
    }
}

/// A reply on the session URI.
async fn interpret(resp: reqwest::Response) -> Result<Step, UploadError> {
    let status = resp.status().as_u16();
    match status {
        200 | 201 => {
            let body: serde_json::Value = resp.json().await.map_err(UploadError::from_reqwest)?;
            body["id"]
                .as_str()
                .map(|id| Step::Done(id.to_string()))
                .ok_or_else(|| UploadError::Protocol("the finished upload has no video id".into()))
        }
        308 => parse_range(resp.headers().get(RANGE).and_then(|v| v.to_str().ok())).map(Step::Resume),
        _ => {
            let retry_after = parse_retry_after(resp.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok()));
            let body = resp.text().await.unwrap_or_default();
            Err(UploadError::from_response(status, body, retry_after, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_range_headers() {
        assert_eq!(parse_range(None).unwrap(), 0);
        assert_eq!(parse_range(Some("bytes=0-0")).unwrap(), 1);
        assert_eq!(parse_range(Some("bytes=0-262143")).unwrap(), 262144);
        assert!(parse_range(Some("bytes=5-10")).is_err());
        assert!(parse_range(Some("0-10")).is_err());
        assert!(parse_range(Some("bytes=0-x")).is_err());
        assert!(parse_range(Some("bytes=0-")).is_err());
    }
}
