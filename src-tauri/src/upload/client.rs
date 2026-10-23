//! YouTube's resumable upload protocol, made to survive real networks.
//!
//! - A session is created once and kept in a `SessionStore`, so a later
//!   attempt (even after a restart) continues it.
//! - A stored session is settled with the server before anything else
//!   (`Content-Range: bytes */N`): 200/201 means it's already complete,
//!   308 gives the next offset, 404/410 means the session is gone. It is
//!   dropped only on such a definite answer (docs/INVARIANTS.md #8, #9).
//! - Chunks are retried with backoff (honouring `Retry-After`); the count
//!   of failures resets whenever the confirmed offset advances, and a 308
//!   that confirms nothing new counts as a failure.
//! - A request is abandoned when no bytes moved for the stall timeout,
//!   not after a fixed total time, so slow uplinks still work.
//! - A 401 refreshes the token once and carries on.

use super::error::{parse_retry_after, UploadError};
use super::session::{file_key, metadata_hash, SessionStore, UploadSession};
use super::{content_type, TokenSource, UploadConfig};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::Client;
use serde::Serialize;
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

/// A video YouTube has.
#[derive(Debug, Clone, PartialEq)]
pub struct Uploaded {
    pub video_id: String,
    /// Set when a stored session for something else (an earlier cut, or the
    /// video with other metadata) turned out finished: that video is what
    /// YouTube has, and this upload didn't happen.
    pub earlier: Option<EarlierUpload>,
    /// Cancel came while the last chunk was on its way, and it arrived.
    pub before_cancel: bool,
}

/// How a finished earlier upload differs from the one asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EarlierUpload {
    /// It was another file: the cut before a re-trim, or a replaced source.
    pub new_cut: bool,
    /// Same file; the title, description or privacy have changed since.
    pub metadata_changed: bool,
}

/// What the upload is, for matching it against a stored session.
struct Current<'a> {
    file_hash: &'a str,
    size: u64,
    metadata_hash: &'a str,
    file_key: &'a str,
}

/// A stored session, settled with YouTube.
enum Settled {
    /// Nothing to continue: start a new session.
    Fresh,
    /// Continue this session from this byte.
    Resume(UploadSession, u64),
    /// YouTube has the whole video already.
    Finished(Uploaded),
}

impl Uploader<'_> {
    /// Upload the video, continuing a stored session when it still fits.
    /// The session is kept on failure or cancel, completed on success, and
    /// dropped only on a definite answer from YouTube (see `settle`).
    pub async fn upload(&self, store: &dyn SessionStore, video: &VideoUpload<'_>, on_progress: Progress<'_>) -> Result<Uploaded, UploadError> {
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
        let fkey = file_key(ctype, &file_name, mtime_ms);
        let current = Current { file_hash: video.file_hash, size, metadata_hash: &mhash, file_key: &fkey };

        let mut file = tokio::fs::File::open(video.path).await.map_err(|e| UploadError::File(e.to_string()))?;
        let mut token = self.tokens.token().await?;
        on_progress(0, size);

        // Every stored session is settled first, whatever it was for.
        let mut session = None;
        let mut offset = None;
        let mut confirmed: u64 = 0;
        if let Some(stored) = store.load() {
            match self.settle(store, &mut token, stored, &current).await? {
                Settled::Fresh => {}
                Settled::Resume(s, n) => {
                    println!("[upload] Continuing the upload session from {} at byte {}", s.created, n);
                    session = Some(s);
                    offset = Some(n);
                    confirmed = n;
                    on_progress(n, size);
                }
                Settled::Finished(done) => {
                    on_progress(size, size);
                    return Ok(done);
                }
            }
        }

        let mut failures = 0u32;
        let mut refreshed = false;
        loop {
            if self.cancel.is_cancelled() {
                return Err(UploadError::Cancelled);
            }
            let sending = session.is_some() && offset.is_some();
            let step = match (&session, offset) {
                (None, _) => self.create(&token, &metadata_json, size, ctype).await.map(|uri| {
                    Step::Created(UploadSession::new(uri, video.file_hash, size, &mhash).with_file_key(&fkey))
                }),
                (Some(s), None) => self.query(&token, &s.uri, size).await,
                (Some(s), Some(off)) => {
                    let sent = self.put_chunk(&token, &s.uri, &mut file, off, size, &|sent| on_progress(off + sent, size)).await;
                    let last = off + self.cfg.chunk_size >= size;
                    if last && matches!(sent, Err(UploadError::Cancelled)) {
                        // The last chunk may have arrived just before.
                        return self.check_after_cancel(store, &token, s, size).await;
                    }
                    sent
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
                    store.complete(&id, video.file_hash);
                    on_progress(size, size);
                    return Ok(Uploaded { video_id: id, earlier: None, before_cancel: false });
                }
                Ok(Step::Resume(n)) if n >= size => {
                    return Err(UploadError::Protocol("YouTube has the whole file but didn't finish the upload".into()));
                }
                Ok(Step::Resume(n)) if sending && n <= confirmed => {
                    // A chunk went and nothing more was kept: a failure,
                    // or a server that never moves would loop forever.
                    failures += 1;
                    if failures >= self.cfg.max_attempts {
                        return Err(UploadError::Stalled);
                    }
                    self.pause(&UploadError::Stalled, failures).await?;
                    offset = Some(n);
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
                    self.pause(&e, failures).await?;
                    offset = None;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Wait before retry number `failures`, unless cancelled.
    async fn pause(&self, e: &UploadError, failures: u32) -> Result<(), UploadError> {
        let delay = self.cfg.backoff.delay(failures, e.retry_after());
        println!("[upload] {} (try {}); retrying in {:?}", e, failures, delay);
        tokio::select! {
            _ = (self.cfg.sleep)(delay) => Ok(()),
            _ = self.cancel.cancelled() => Err(UploadError::Cancelled),
        }
    }

    /// Ask YouTube about a stored session, and act only on a definite
    /// answer:
    /// - finished (200/201): record it; if it was for another file or other
    ///   metadata, say so instead of uploading;
    /// - 308: continue it if it's for this upload, otherwise drop it;
    /// - gone (404/410): drop it.
    ///
    /// Anything else (offline, server errors, a 401 after a refresh) keeps
    /// the session and fails with `CheckFailed`.
    async fn settle(
        &self,
        store: &dyn SessionStore,
        token: &mut String,
        stored: UploadSession,
        current: &Current<'_>,
    ) -> Result<Settled, UploadError> {
        let matches = stored.reusable_for(current.file_hash, current.size, current.metadata_hash, chrono::Utc::now());
        let mut failures = 0u32;
        let mut refreshed = false;
        loop {
            if self.cancel.is_cancelled() {
                return Err(UploadError::Cancelled);
            }
            match self.query(token, &stored.uri, stored.size).await {
                Ok(Step::Done(id)) => {
                    store.complete(&id, &stored.file_hash);
                    let new_cut = stored.other_file(current.file_hash, current.size, current.file_key);
                    let metadata_changed = !new_cut && stored.metadata_hash != current.metadata_hash;
                    let earlier = (new_cut || metadata_changed).then_some(EarlierUpload { new_cut, metadata_changed });
                    if earlier.is_some() {
                        println!("[upload] An earlier upload had finished: {}", id);
                    }
                    return Ok(Settled::Finished(Uploaded { video_id: id, earlier, before_cancel: false }));
                }
                Ok(Step::Resume(n)) if matches => return Ok(Settled::Resume(stored, n)),
                Ok(Step::Resume(_)) => {
                    println!("[upload] Dropping an unfinished session for another file or metadata");
                    store.save(None);
                    return Ok(Settled::Fresh);
                }
                Ok(Step::Created(_)) => unreachable!("a query never creates"),
                Err(UploadError::SessionGone) => {
                    println!("[upload] The stored upload session is gone");
                    store.save(None);
                    return Ok(Settled::Fresh);
                }
                Err(UploadError::Cancelled) => return Err(UploadError::Cancelled),
                Err(UploadError::Auth(_)) if !refreshed => {
                    refreshed = true;
                    *token = self.tokens.refresh().await?;
                }
                Err(e) if e.is_retryable() && failures + 1 < self.cfg.max_attempts => {
                    failures += 1;
                    self.pause(&e, failures).await?;
                }
                Err(e) => {
                    println!("[upload] Couldn't check the stored session: {}", e);
                    return Err(UploadError::CheckFailed(e.to_string()));
                }
            }
        }
    }

    /// Cancelled during the last chunk: ask (briefly, not cancellable)
    /// whether it arrived.
    async fn check_after_cancel(
        &self,
        store: &dyn SessionStore,
        token: &str,
        session: &UploadSession,
        size: u64,
    ) -> Result<Uploaded, UploadError> {
        let ask = async {
            let resp = self
                .http
                .put(&session.uri)
                .bearer_auth(token)
                .header(CONTENT_RANGE, format!("bytes */{}", size))
                .header(CONTENT_LENGTH, "0")
                .send()
                .await
                .map_err(UploadError::from_reqwest)?;
            interpret(resp).await
        };
        match tokio::time::timeout(self.cfg.cancel_check, ask).await {
            Ok(Ok(Step::Done(id))) => {
                println!("[upload] The upload finished before it could be cancelled: {}", id);
                store.complete(&id, &session.file_hash);
                Ok(Uploaded { video_id: id, earlier: None, before_cancel: true })
            }
            Ok(Ok(Step::Resume(_))) => Err(UploadError::Cancelled),
            _ => Err(UploadError::CancelledMaybeFinished),
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
