//! The upload against the local mock server, one injected fault at a time.

use super::client::{Uploader, VideoUpload};
use super::session::MemoryStore;
use super::test_server::{Fault, MockServer};
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const CHUNK: u64 = 256 * 1024;

struct FakeTokens {
    current: Mutex<String>,
    next: String,
    refreshes: AtomicU32,
}

impl FakeTokens {
    fn new() -> Self {
        FakeTokens { current: Mutex::new("tok1".into()), next: "tok2".into(), refreshes: AtomicU32::new(0) }
    }
}

impl TokenSource for FakeTokens {
    fn token(&self) -> BoxFuture<'_, Result<String, UploadError>> {
        Box::pin(async move { Ok(self.current.lock().unwrap().clone()) })
    }
    fn refresh(&self) -> BoxFuture<'_, Result<String, UploadError>> {
        Box::pin(async move {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            *self.current.lock().unwrap() = self.next.clone();
            Ok(self.next.clone())
        })
    }
}

struct Harness {
    server: MockServer,
    cfg: UploadConfig,
    sleeps: Arc<Mutex<Vec<Duration>>>,
    http: reqwest::Client,
    tokens: FakeTokens,
    store: MemoryStore,
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    data: Vec<u8>,
}

impl Harness {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let recorded = sleeps.clone();
        let cfg = UploadConfig {
            endpoints: Endpoints::at(&server.base),
            chunk_size: CHUNK,
            stall_timeout: Duration::from_millis(300),
            backoff: Backoff {
                initial: Duration::from_millis(1),
                max: Duration::from_millis(5),
                max_retry_after: Duration::from_secs(600),
            },
            max_attempts: 4,
            sleep: Arc::new(move |d| {
                recorded.lock().unwrap().push(d);
                Box::pin(async {})
            }),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trimmed.mp4");
        // Three full chunks and a short last one.
        let data: Vec<u8> = (0..(3 * CHUNK + 1000)).map(|i| (i * 31 % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        Harness {
            server,
            cfg,
            sleeps,
            http: build_client(),
            tokens: FakeTokens::new(),
            store: MemoryStore::default(),
            _dir: dir,
            path,
            data,
        }
    }

    fn faults(&self, faults: Vec<Fault>) {
        self.server.with(|s| s.faults = faults.into());
    }

    fn video(&self) -> VideoUpload<'_> {
        VideoUpload {
            path: &self.path,
            file_hash: "hash1",
            metadata: serde_json::json!({"snippet": {"title": "Sermon"}, "status": {"privacyStatus": "public"}}),
        }
    }

    async fn upload_with(&self, cancel: &CancellationToken) -> Result<String, UploadError> {
        let uploader = Uploader { http: &self.http, cfg: &self.cfg, tokens: &self.tokens, cancel };
        uploader.upload(&self.store, &self.video(), &|_, _| {}).await
    }

    async fn upload(&self) -> Result<String, UploadError> {
        self.upload_with(&CancellationToken::new()).await
    }

    /// The whole file arrived once, with nothing sent twice.
    fn assert_complete_once(&self) {
        assert_eq!(self.server.stored(), self.data, "stored bytes differ");
        assert_eq!(self.server.with(|s| s.bytes_stored), self.data.len() as u64, "bytes were stored twice");
    }
}

#[tokio::test]
async fn uploads_in_chunks() {
    let h = Harness::new().await;
    let progress = Mutex::new(Vec::new());
    let uploader = Uploader { http: &h.http, cfg: &h.cfg, tokens: &h.tokens, cancel: &CancellationToken::new() };
    let id = uploader.upload(&h.store, &h.video(), &|sent, total| progress.lock().unwrap().push((sent, total))).await.unwrap();
    assert_eq!(id, "vid1");
    h.assert_complete_once();
    assert_eq!(h.server.with(|s| (s.creates, s.chunk_puts, s.queries)), (1, 4, 0));
    assert_eq!(h.server.with(|s| s.content_types.clone()), vec!["video/mp4".to_string()]);
    assert!(h.store.load().is_none(), "the session is cleared on success");
    let progress = progress.into_inner().unwrap();
    assert_eq!(progress.last(), Some(&(h.data.len() as u64, h.data.len() as u64)));
}

#[tokio::test]
async fn retries_503_honouring_retry_after() {
    let h = Harness::new().await;
    h.faults(vec![Fault::Status { code: 503, retry_after: Some(7) }]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    assert_eq!(h.sleeps.lock().unwrap().as_slice(), &[Duration::from_secs(7)]);
    // Before resuming, the server was asked where to continue.
    assert_eq!(h.server.with(|s| s.queries), 1);
}

#[tokio::test]
async fn retries_429_with_backoff() {
    let h = Harness::new().await;
    h.faults(vec![Fault::Status { code: 429, retry_after: None }, Fault::Status { code: 500, retry_after: None }]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    let sleeps = h.sleeps.lock().unwrap().clone();
    assert_eq!(sleeps.len(), 2);
    assert!(sleeps.iter().all(|d| *d <= Duration::from_millis(5)));
}

#[tokio::test]
async fn dropped_connection_after_store_resumes_without_resending() {
    let h = Harness::new().await;
    h.faults(vec![Fault::DropAfterStore]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    assert_eq!(h.server.with(|s| s.chunk_puts), 4);
}

#[tokio::test]
async fn completed_but_reply_lost_is_not_uploaded_twice() {
    let h = Harness::new().await;
    h.server.with(|s| {
        s.faults = vec![
            // Three chunks go through; the last one is stored but its reply lost.
            Fault::PartialStore(CHUNK as usize),
            Fault::PartialStore(CHUNK as usize),
            Fault::PartialStore(CHUNK as usize),
            Fault::DropAfterStore,
        ]
        .into()
    });
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    // The status query found it complete: no fifth PUT, no new session.
    assert_eq!(h.server.with(|s| (s.chunk_puts, s.queries, s.creates)), (4, 1, 1));
    assert!(h.store.load().is_none());
}

#[tokio::test]
async fn partial_308_resumes_from_range() {
    let h = Harness::new().await;
    h.faults(vec![Fault::PartialStore(1000)]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    // 1000 bytes, then the rest in three chunks from there.
    assert_eq!(h.server.with(|s| (s.chunk_puts, s.queries)), (4, 0));
    assert!(h.sleeps.lock().unwrap().is_empty(), "progress isn't a failure");
}

#[tokio::test]
async fn stalled_request_is_abandoned_and_resumed() {
    let h = Harness::new().await;
    h.faults(vec![Fault::PartialStore(CHUNK as usize), Fault::Stall]);
    let started = std::time::Instant::now();
    assert_eq!(h.upload().await.unwrap(), "vid1");
    assert!(started.elapsed() < Duration::from_secs(5));
    h.assert_complete_once();
    assert_eq!(h.sleeps.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn gives_up_after_repeated_stalls() {
    let h = Harness::new().await;
    h.faults(vec![Fault::Stall; 4]);
    assert_eq!(h.upload().await, Err(UploadError::Stalled));
    // The session is kept for next time.
    assert!(h.store.load().is_some());
}

#[tokio::test]
async fn token_expiring_mid_upload_is_refreshed() {
    let h = Harness::new().await;
    h.faults(vec![Fault::PartialStore(CHUNK as usize), Fault::ExpireToken("tok2".into())]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    assert_eq!(h.tokens.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auth_failing_after_refresh_stops() {
    let h = Harness::new().await;
    // The server wants a token the refresh never produces.
    h.faults(vec![Fault::ExpireToken("other".into())]);
    let err = h.upload().await.unwrap_err();
    assert!(matches!(err, UploadError::Auth(_)), "{:?}", err);
    assert_eq!(err.user_message(), error::SIGN_IN_AGAIN);
    assert_eq!(h.tokens.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn session_gone_starts_a_new_one() {
    for code in [404, 410] {
        let h = Harness::new().await;
        h.faults(vec![Fault::PartialStore(CHUNK as usize), Fault::Gone(code)]);
        assert_eq!(h.upload().await.unwrap(), "vid2");
        assert_eq!(h.server.stored(), h.data);
        assert_eq!(h.server.with(|s| s.creates), 2);
        assert!(h.store.load().is_none());
    }
}

#[tokio::test]
async fn a_second_attempt_continues_the_persisted_session() {
    let mut h = Harness::new().await;
    h.cfg.max_attempts = 2;
    let busy = || Fault::Status { code: 503, retry_after: None };
    h.faults(vec![Fault::PartialStore(CHUNK as usize), busy(), busy()]);
    let err = h.upload().await.unwrap_err();
    assert!(matches!(err, UploadError::Http { status: 503, .. }));
    let kept = h.store.load().expect("the session is kept after a failure");

    // Next attempt (a new app run, say): same session, from where it was.
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    assert_eq!(h.server.with(|s| s.creates), 1);
    assert!(kept.uri.contains("/session/s1"));
}

#[tokio::test]
async fn changed_metadata_or_old_session_starts_fresh() {
    let h = Harness::new().await;
    h.store.save(Some(&UploadSession::new(format!("{}/session/nope", h.server.base), "hash1", h.data.len() as u64, "other")));
    assert_eq!(h.upload().await.unwrap(), "vid1");
    assert_eq!(h.server.with(|s| (s.creates, s.queries)), (1, 0));

    let h = Harness::new().await;
    let mut old = UploadSession::new(format!("{}/session/nope", h.server.base), "hash1", h.data.len() as u64, "x");
    old.created = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    h.store.save(Some(&old));
    assert_eq!(h.upload().await.unwrap(), "vid1");
    assert_eq!(h.server.with(|s| s.queries), 0);
}

#[tokio::test]
async fn failures_reset_when_a_chunk_makes_progress() {
    let mut h = Harness::new().await;
    h.cfg.max_attempts = 2;
    let busy = || Fault::Status { code: 503, retry_after: None };
    let ok = || Fault::PartialStore(CHUNK as usize);
    // One failure before every chunk: never two in a row without progress.
    h.faults(vec![busy(), ok(), busy(), ok(), busy(), ok(), busy()]);
    assert_eq!(h.upload().await.unwrap(), "vid1");
    h.assert_complete_once();
    assert_eq!(h.sleeps.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn cancel_stops_and_keeps_the_session() {
    let mut h = Harness::new().await;
    h.cfg.stall_timeout = Duration::from_secs(30);
    h.faults(vec![Fault::PartialStore(CHUNK as usize), Fault::Stall]);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c.cancel();
    });
    let started = std::time::Instant::now();
    assert_eq!(h.upload_with(&cancel).await, Err(UploadError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(h.store.load().is_some());

    // And the next attempt continues it.
    assert_eq!(h.upload().await.unwrap(), "vid1");
    assert_eq!(h.server.stored(), h.data);
    assert_eq!(h.server.with(|s| s.creates), 1);
}

#[tokio::test]
async fn post_upload_problems_become_warnings() {
    let h = Harness::new().await;
    h.server.with(|s| {
        s.thumbnail_status = 403;
        s.playlist_status = 404;
    });
    let thumb = h._dir.path().join("thumb.jpg");
    std::fs::write(&thumb, b"jpg").unwrap();
    let after = AfterUpload {
        thumbnail: Some(thumb),
        captions: Some("1\n00:00:00,000 --> 00:00:01,000\nhi\n".into()),
        playlist_id: Some("PL1".into()),
    };
    let outcome = run(&h.http, &h.cfg, &h.tokens, &h.store, &CancellationToken::new(), &h.video(), &after, &|_, _| {})
        .await
        .unwrap();
    assert_eq!(outcome.video_id, "vid1");
    assert_eq!(outcome.url, "https://youtube.com/watch?v=vid1");
    let kinds: Vec<_> = outcome.warnings.iter().map(|w| w.kind).collect();
    assert_eq!(kinds, vec![WarningKind::Thumbnail, WarningKind::Playlist]);
    assert!(outcome.warnings[0].message.contains("can't set custom thumbnails"), "{}", outcome.warnings[0].message);
    assert_eq!(h.server.with(|s| (s.thumbnails, s.captions, s.playlist_adds)), (1, 1, 1));

    // Setting the thumbnail again, by the stored video id, works once allowed.
    h.server.with(|s| s.thumbnail_status = 200);
    post::set_thumbnail(&h.http, &h.cfg.endpoints, "tok1", &outcome.video_id, &after.thumbnail.clone().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn post_upload_steps_refresh_an_expired_token() {
    let h = Harness::new().await;
    let id = h.upload().await.unwrap();
    // The token expired while the video was uploading.
    h.server.with(|s| s.token = "tok2".into());
    let after = AfterUpload { captions: Some("1\n00:00:00,000 --> 00:00:01,000\nhi\n".into()), ..Default::default() };
    let warnings = post::after_upload(&h.http, &h.cfg.endpoints, &h.tokens, &id, &after).await;
    assert!(warnings.is_empty(), "{:?}", warnings);
    assert_eq!(h.tokens.refreshes.load(Ordering::SeqCst), 1);
}
