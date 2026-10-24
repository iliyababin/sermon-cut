use crate::state::{Chapter, TranscriptionResult, TranscriptSegment};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

#[derive(Debug, Deserialize)]
struct ModalTranscriptionResponse {
    segments: Vec<ModalSegment>,
    text: String,
    /// null when the AI step failed; the transcript is still good.
    #[serde(default)]
    sermon_analysis: Option<SermonAnalysis>,
    #[serde(default)]
    analysis_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModalSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Debug, Deserialize)]
struct SermonAnalysis {
    sermon_start: Option<f64>,
    sermon_end: Option<f64>,
    title: Option<String>,
    description: Option<String>,
    chapters: Option<Vec<ModalChapter>>,
    /// Why no sermon was found (then sermon_start is null).
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModalChapter {
    time: f64,
    title: String,
}

/// No Debug: it's the audio.
#[derive(Serialize)]
struct TranscribeRequest {
    audio_base64: String,
}

/// How long to wait before each retry of a request that never reached the
/// server.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];

/// Whether a failed request never reached the server (it couldn't connect,
/// or the name didn't resolve), so sending it again can't start a second
/// paid transcription. A timeout or an error answer never counts.
pub fn never_reached_server(e: &reqwest::Error) -> bool {
    e.is_connect() && !e.is_timeout()
}

/// Send the request `make` builds, again after each of `delays` while it
/// fails before reaching the server.
async fn send_with_retry(
    make: impl Fn() -> reqwest::RequestBuilder,
    delays: &[Duration],
) -> Result<reqwest::Response, reqwest::Error> {
    let mut attempt = 0;
    loop {
        match make().send().await {
            Err(e) if attempt < delays.len() && never_reached_server(&e) => {
                log::warn!("Couldn't reach the server ({}); trying again", e);
                tokio::time::sleep(delays[attempt]).await;
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The progress message while the server transcribes: "Transcribing… 4:12".
pub fn transcribing_message(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("Transcribing… {}:{:02}:{:02}", h, m, s)
    } else {
        format!("Transcribing… {}:{:02}", m, s)
    }
}

pub async fn transcribe_audio(
    audio_path: &str,
    api_url: &str,
    api_key: Option<&str>,
) -> Result<TranscriptionResult, String> {
    // Check file size before reading (reject files > 200MB)
    let metadata = tokio::fs::metadata(audio_path)
        .await
        .map_err(|e| format!("Failed to get audio file metadata: {}", e))?;
    if metadata.len() > 200 * 1024 * 1024 {
        return Err(format!(
            "Audio file too large ({:.0} MB). Maximum size is 200 MB.",
            metadata.len() as f64 / 1024.0 / 1024.0
        ));
    }

    // Read audio file
    let mut file = File::open(audio_path)
        .await
        .map_err(|e| format!("Failed to open audio file: {}", e))?;

    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .await
        .map_err(|e| format!("Failed to read audio file: {}", e))?;

    // Encode as base64, as JSON once for every attempt.
    let body = serde_json::to_vec(&TranscribeRequest { audio_base64: STANDARD.encode(&buffer) })
        .map_err(|e| format!("Failed to build request: {}", e))?;

    // Redirects are followed by hand (see `transcribe_with`), never by
    // reqwest, which could resend the audio.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(600))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    log::info!(
        "Sending {:.1} MB of audio to {}",
        metadata.len() as f64 / 1e6,
        crate::logging::url_host(api_url)
    );
    let started = std::time::Instant::now();
    let result = transcribe_with(&client, api_url, api_key, body, &RETRY_DELAYS, &Limits::default()).await;
    let secs = started.elapsed().as_secs();
    match &result {
        Ok(t) => log::info!(
            "Transcription done in {}m {:02}s: {} segments; sermon {}{}",
            secs / 60,
            secs % 60,
            t.segments.len(),
            match (t.sermon_start, t.sermon_end) {
                (Some(s), Some(e)) => format!("{:.0}–{:.0}s", s, e),
                _ => "not found".into(),
            },
            match (&t.analysis_error, &t.no_sermon_reason) {
                (Some(_), _) => "; the analysis failed (see the review screen)",
                (None, Some(_)) => "; the AI gave a reason",
                _ => "",
            }
        ),
        Err(e) => log::warn!("Transcription failed after {}m {:02}s: {}", secs / 60, secs % 60, e),
    }
    result
}

/// How long a transcription may take, and how its result is waited for.
#[derive(Debug, Clone)]
struct Limits {
    /// Everything: the upload, transcription and analysis of a long
    /// service, and every wait for the result.
    overall: Duration,
    /// One request for the result. Modal holds each about 150 s before
    /// redirecting again; one that hangs past this is tried again.
    poll: Duration,
    /// The first wait before asking for the result again after an error;
    /// it doubles, up to `max_backoff`.
    backoff: Duration,
    max_backoff: Duration,
    /// The least time between two requests for the result, even when the
    /// server redirects at once (it doubles while it keeps doing so, up to
    /// `max_backoff`), so a misbehaving server isn't asked in a tight loop.
    min_poll: Duration,
    /// Modal's own server errors (not ours) for this long in a row, with
    /// no other answer between: the job is gone (e.g. its container was
    /// stopped), so stop waiting for it.
    lost_after: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // Longer than the server's own limit (TIMEOUT in backend/app.py).
            overall: Duration::from_secs(55 * 60),
            poll: Duration::from_secs(5 * 60),
            backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(30),
            min_poll: Duration::from_secs(1),
            lost_after: Duration::from_secs(3 * 60),
        }
    }
}

const TIMED_OUT: &str = "Transcription took too long (over 55 minutes). Try again later.";
const LOST: &str = "The transcription server lost this job (its machine was restarted). Click Retry to send it again.";

/// Our server's own failure answer, {"success": false, "error": ...}: a
/// real answer, not worth asking again.
fn server_failure(bytes: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Failure {
        success: bool,
        error: String,
    }
    serde_json::from_slice::<Failure>(bytes).ok().filter(|f| !f.success).map(|f| f.error)
}

/// Send the audio once (again only if it never reached the server), then
/// wait for the answer. Modal answers a request that runs past 150 s with
/// a 303 to a result URL; that is polled with GET, which is safe to repeat,
/// so connection errors, timeouts and server errors there are retried with
/// backoff until `limits.overall`. The audio is never sent again after a
/// redirect.
async fn transcribe_with(
    client: &reqwest::Client,
    api_url: &str,
    api_key: Option<&str>,
    body: Vec<u8>,
    delays: &[Duration],
    limits: &Limits,
) -> Result<TranscriptionResult, String> {
    let deadline = tokio::time::Instant::now() + limits.overall;
    let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
    let auth = |request: reqwest::RequestBuilder| match api_key {
        Some(key) => request.header("Authorization", format!("Bearer {}", key)),
        None => request,
    };
    let make = || {
        auth(client.post(api_url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone())
            .timeout(remaining())
    };
    let response = send_with_retry(make, delays).await.map_err(|e| {
        if e.is_timeout() {
            TIMED_OUT.to_string()
        } else {
            format!("Failed to send request: {}", e)
        }
    })?;

    let origin = reqwest::Url::parse(api_url).map_err(|e| format!("Invalid server URL: {}", e))?;
    let (mut status, mut location, mut bytes) = read(response).await?;
    log::info!("The server answered {} ({} bytes){}", status, bytes.len(), if location.is_some() { "; waiting for the result" } else { "" });
    let mut polls = crate::logging::FailureTracker::new(module_path!(), "Asking for the result");
    let mut wait = limits.backoff;
    let mut gap = limits.min_poll;
    let mut last_ask = tokio::time::Instant::now();
    // Since when only Modal's own server errors came back.
    let mut failing_since: Option<tokio::time::Instant> = None;
    while let Some(next) = redirect_target(status, location.as_deref(), &origin)? {
        // Ask for the result until there's an answer worth reading.
        loop {
            // Never sooner than `gap` after the last ask.
            tokio::time::sleep_until((last_ask + gap).min(deadline)).await;
            let left = remaining();
            if left.is_zero() {
                log::warn!("Gave up waiting for the result after {} min", limits.overall.as_secs() / 60);
                return Err(TIMED_OUT.into());
            }
            last_ask = tokio::time::Instant::now();
            // The key goes only to the server it's for.
            let mut request = client.get(next.clone()).timeout(left.min(limits.poll));
            if next.origin() == origin.origin() {
                request = auth(request);
            }
            let failure = match request.send().await {
                Ok(response) if !response.status().is_server_error() => match read(response).await {
                    Ok(answer) => {
                        (status, location, bytes) = answer;
                        failing_since = None;
                        polls.log_recovered();
                        // Answered at once: wait longer before the next ask.
                        gap = if last_ask.elapsed() < gap { (gap * 2).min(limits.max_backoff.max(limits.min_poll)) } else { limits.min_poll };
                        break;
                    }
                    Err(e) => e,
                },
                Ok(response) => {
                    let code = response.status();
                    let body = response.bytes().await.unwrap_or_default();
                    // Ours: the job ran and failed, so asking again won't help.
                    if let Some(error) = server_failure(&body) {
                        return Err(error);
                    }
                    // Modal's: retried, but not for ever.
                    let since = *failing_since.get_or_insert(last_ask);
                    if since.elapsed() >= limits.lost_after {
                        log::warn!("Only server errors for {}s: the job is lost", since.elapsed().as_secs());
                        return Err(LOST.into());
                    }
                    format!("server error {} ({} bytes)", code, body.len())
                }
                Err(e) => e.to_string(),
            };
            polls.log_failure(format!("{}; asking again", crate::logging::hosts_only(&failure)));
            tokio::time::sleep(wait.min(remaining())).await;
            wait = (wait * 2).min(limits.max_backoff);
        }
        wait = limits.backoff;
    }

    if !status.is_success() {
        if let Some(error) = server_failure(&bytes) {
            return Err(error);
        }
        // Not the body: it can be anything, and the error goes in the log.
        log::warn!("The server answered {} ({} bytes)", status, bytes.len());
        return Err(format!("The transcription server answered with an error ({}). Try again later.", status));
    }
    parse_response(&bytes)
}

/// A response's status, Location and whole body.
async fn read(response: reqwest::Response) -> Result<(reqwest::StatusCode, Option<String>, Vec<u8>), String> {
    let status = response.status();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .map(String::from);
    let url = response.url().clone();
    let bytes = response.bytes().await.map_err(|e| format!("Failed to read response: {}", e))?;
    // A relative Location is relative to the URL that gave it.
    let location = location.and_then(|l| url.join(&l).ok()).map(String::from);
    Ok((status, location, bytes.to_vec()))
}

/// Where a redirect says the result is, to GET. Only "see other" kinds
/// (301, 302, 303) are followed: 307 and 308 would mean sending the audio
/// again, which this never does.
fn redirect_target(status: reqwest::StatusCode, location: Option<&str>, origin: &reqwest::Url) -> Result<Option<reqwest::Url>, String> {
    use reqwest::StatusCode;
    if !status.is_redirection() {
        return Ok(None);
    }
    if !matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER) {
        return Err(format!("API error ({}): unexpected redirect", status));
    }
    let location = location.ok_or_else(|| format!("API error ({}): redirect without a location", status))?;
    let url = origin.join(location).map_err(|e| format!("API error: bad redirect ({})", e))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("API error: bad redirect".into());
    }
    Ok(Some(url))
}

/// The server's answer as a transcription. Without an analysis (the AI
/// step failed) the sermon bounds are unknown and `analysis_error` says why.
fn parse_response(bytes: &[u8]) -> Result<TranscriptionResult, String> {
    let modal_response: ModalTranscriptionResponse =
        serde_json::from_slice(bytes).map_err(|e| format!("Failed to parse response: {}", e))?;

    let segments: Vec<TranscriptSegment> = modal_response
        .segments
        .into_iter()
        .map(|s| TranscriptSegment {
            start: s.start,
            end: s.end,
            text: s.text,
        })
        .collect();

    let mut analysis_error = None;
    let mut no_sermon_reason = None;
    let (sermon_start, sermon_end, suggested_title, suggested_description, suggested_chapters) =
        if let Some(analysis) = modal_response.sermon_analysis {
            if analysis.sermon_start.is_none() {
                no_sermon_reason = analysis.reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
            }
            (
                analysis.sermon_start,
                analysis.sermon_end,
                analysis.title,
                analysis.description,
                analysis.chapters.map(|chapters| {
                    chapters
                        .into_iter()
                        .map(|c| Chapter {
                            time: c.time,
                            title: c.title,
                        })
                        .collect()
                }),
            )
        } else {
            analysis_error = Some(
                modal_response
                    .analysis_error
                    .filter(|e| !e.trim().is_empty())
                    .unwrap_or_else(|| "The AI didn't find the sermon".into()),
            );
            (None, None, None, None, None)
        };

    Ok(TranscriptionResult {
        segments,
        full_text: modal_response.text,
        sermon_start,
        sermon_end,
        suggested_title,
        suggested_description,
        suggested_chapters,
        analysis_error,
        no_sermon_reason,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn the_app_outwaits_the_server_and_the_server_has_room_for_the_longest_video() {
        let app_py = include_str!("../../backend/app.py");
        let server_secs: u64 = app_py
            .lines()
            .find_map(|l| l.strip_prefix("TIMEOUT = "))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .expect("TIMEOUT in backend/app.py");
        // The app keeps waiting until the server has certainly given up.
        assert!(super::Limits::default().overall.as_secs() > server_secs + 5 * 60);
        // Transcription runs at ~12x real time on the T4: leave at least 3x that.
        let longest = crate::library::MAX_TRANSCRIBE_SECS as u64;
        assert!(server_secs >= longest / 12 * 3, "{server_secs} s for a {longest} s video");
    }

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn a_null_analysis_keeps_the_transcript_and_says_why() {
        let r = parse_response(
            br#"{"success":true,"segments":[{"start":0,"end":2,"text":"Hi"}],"text":"Hi","sermon_analysis":null,"analysis_error":"model overloaded"}"#,
        )
        .unwrap();
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.full_text, "Hi");
        assert_eq!((r.sermon_start, r.sermon_end), (None, None));
        assert_eq!(r.analysis_error.as_deref(), Some("model overloaded"));

        // An older server leaves analysis_error out.
        let r = parse_response(br#"{"segments":[],"text":"","sermon_analysis":null}"#).unwrap();
        assert!(r.analysis_error.is_some());
    }

    #[test]
    fn a_full_analysis_has_no_error() {
        let r = parse_response(
            br#"{"success":true,"segments":[],"text":"","sermon_analysis":{"sermon_start":60,"sermon_end":1800,"title":"Hope","description":"d","chapters":[{"time":60,"title":"Intro"}]}}"#,
        )
        .unwrap();
        assert_eq!((r.sermon_start, r.sermon_end), (Some(60.0), Some(1800.0)));
        assert_eq!(r.suggested_title.as_deref(), Some("Hope"));
        assert_eq!(r.suggested_chapters.unwrap().len(), 1);
        assert_eq!(r.analysis_error, None);
        // Saved transcripts from before have no analysis_error.
        let old: TranscriptionResult = serde_json::from_str(
            r#"{"segments":[],"full_text":"","sermon_start":null,"sermon_end":null,"suggested_title":null,"suggested_description":null,"suggested_chapters":null}"#,
        )
        .unwrap();
        assert_eq!(old.analysis_error, None);
    }

    #[test]
    fn the_reason_no_sermon_was_found_is_kept() {
        let r = parse_response(
            br#"{"segments":[],"text":"","sermon_analysis":{"sermon_start":null,"sermon_end":null,"title":null,"description":null,"chapters":null,"reason":"Only worship songs"}}"#,
        )
        .unwrap();
        assert_eq!(r.no_sermon_reason.as_deref(), Some("Only worship songs"));
        assert_eq!((r.sermon_start, r.analysis_error.as_deref()), (None, None));
        // Older servers don't send one.
        let r = parse_response(br#"{"segments":[],"text":"","sermon_analysis":{"sermon_start":null,"sermon_end":null,"title":null,"description":null,"chapters":null}}"#).unwrap();
        assert_eq!(r.no_sermon_reason, None);
        // Saved, and read back.
        let saved: TranscriptionResult = serde_json::from_str(&serde_json::to_string(&TranscriptionResult {
            no_sermon_reason: Some("why".into()),
            ..r
        }).unwrap())
        .unwrap();
        assert_eq!(saved.no_sermon_reason.as_deref(), Some("why"));
    }

    #[test]
    fn elapsed_time_reads_like_a_clock() {
        assert_eq!(transcribing_message(Duration::from_secs(0)), "Transcribing… 0:00");
        assert_eq!(transcribing_message(Duration::from_secs(252)), "Transcribing… 4:12");
        assert_eq!(transcribing_message(Duration::from_secs(3723)), "Transcribing… 1:02:03");
    }

    /// A local server that counts connections and answers each with
    /// `reply`, or never answers when it's None.
    async fn local_server(reply: Option<&'static str>) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    let _ = tokio::time::timeout(Duration::from_millis(200), sock.read(&mut buf)).await;
                    match reply {
                        Some(r) => {
                            let _ = sock.write_all(r.as_bytes()).await;
                        }
                        None => tokio::time::sleep(Duration::from_secs(10)).await,
                    }
                });
            }
        });
        (url, hits)
    }

    fn client(timeout: Duration) -> reqwest::Client {
        reqwest::Client::builder().timeout(timeout).redirect(reqwest::redirect::Policy::none()).build().unwrap()
    }

    fn fast() -> Limits {
        Limits {
            overall: Duration::from_secs(10),
            poll: Duration::from_millis(500),
            backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            min_poll: Duration::from_millis(10),
            lost_after: Duration::from_secs(2),
        }
    }

    pub(crate) const RESULT: &str = r#"{"segments":[{"start":0,"end":1,"text":"Hi"}],"text":"Hi","sermon_analysis":null,"analysis_error":"x"}"#;

    pub(crate) fn reply(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    /// What the mock server does with one request.
    pub(crate) enum Act {
        Reply(String),
        /// Close the connection without answering.
        Drop,
        /// Never answer.
        Hang,
    }

    /// Requests the mock server saw: (method, path, had the key).
    pub(crate) type Seen = Arc<std::sync::Mutex<Vec<(String, String, bool)>>>;

    /// A local HTTP server answering each request with `handler(method,
    /// path, how many requests came before)`.
    pub(crate) async fn mock(handler: impl Fn(&str, &str, usize) -> Act + Send + Sync + 'static) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/transcribe", listener.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (log, handler) = (log.clone(), handler.clone());
                tokio::spawn(async move {
                    // The head, then as much body as it says.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let head_end = loop {
                        let Ok(n) = sock.read(&mut chunk).await else { return };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let length = head
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    while buf.len() < head_end + length {
                        let Ok(n) = sock.read(&mut chunk).await else { return };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let mut first = head.lines().next().unwrap_or("").split(' ');
                    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
                    let keyed = head.to_ascii_lowercase().contains("authorization: bearer k");
                    let n = {
                        let mut log = log.lock().unwrap();
                        log.push((method.clone(), path.clone(), keyed));
                        log.len() - 1
                    };
                    match handler(&method, &path, n) {
                        Act::Reply(r) => {
                            let _ = sock.write_all(r.as_bytes()).await;
                        }
                        Act::Drop => {}
                        Act::Hang => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        (url, seen)
    }

    fn methods(seen: &Seen) -> Vec<String> {
        seen.lock().unwrap().iter().map(|(m, p, _)| format!("{m} {p}")).collect()
    }

    #[tokio::test]
    async fn a_redirect_is_polled_with_get_until_the_result_comes() {
        let (url, seen) = mock(|method, _, n| match (method, n) {
            ("POST", _) => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            // Still working: Modal redirects again to the same place.
            (_, 1) => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            _ => Act::Reply(reply("200 OK", "Content-Type: application/json\r\n", RESULT)),
        })
        .await;
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, Some("k"), b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap();
        assert_eq!(r.full_text, "Hi");
        assert_eq!(methods(&seen), ["POST /transcribe", "GET /result/1", "GET /result/1"]);
        assert!(seen.lock().unwrap().iter().all(|(_, _, keyed)| *keyed), "the key goes along to the same server");
    }

    #[tokio::test]
    async fn a_failed_poll_is_asked_again_never_resending_the_audio() {
        let (url, seen) = mock(|method, _, n| match (method, n) {
            ("POST", _) => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            (_, 1) => Act::Reply(reply("502 Bad Gateway", "", "busy")),
            (_, 2) => Act::Drop,
            (_, 3) => Act::Hang,
            _ => Act::Reply(reply("200 OK", "", RESULT)),
        })
        .await;
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap();
        assert_eq!(r.full_text, "Hi");
        assert_eq!(methods(&seen), ["POST /transcribe", "GET /result/1", "GET /result/1", "GET /result/1", "GET /result/1"]);
    }

    /// The server's own failure answer ends the wait at once.
    #[tokio::test]
    async fn the_servers_failure_answer_is_not_asked_again() {
        let failed = r#"{"success":false,"error":"Transcription failed: EOFError: End of file"}"#;
        let (url, seen) = mock(move |method, _, _| match method {
            "POST" => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            _ => Act::Reply(reply("500 Internal Server Error", "Content-Type: application/json\r\n", failed)),
        })
        .await;
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap_err();
        assert_eq!(err, "Transcription failed: EOFError: End of file");
        assert_eq!(methods(&seen), ["POST /transcribe", "GET /result/1"]);

        // Also when it comes straight back from the POST.
        let (url, _) = mock(move |_, _, _| Act::Reply(reply("500 Internal Server Error", "", failed))).await;
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap_err();
        assert_eq!(err, "Transcription failed: EOFError: End of file");
    }

    /// Modal's own errors for `lost_after` in a row: the job is gone (a
    /// stopped container), so the wait ends long before `overall`.
    #[tokio::test]
    async fn a_job_modal_lost_ends_the_wait_early() {
        let (url, seen) = mock(|method, _, _| match method {
            "POST" => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            _ => Act::Reply(reply("500 Internal Server Error", "", "modal-http: internal error: function was terminated by signal")),
        })
        .await;
        let limits = Limits { overall: Duration::from_secs(30), lost_after: Duration::from_millis(300), ..fast() };
        let started = std::time::Instant::now();
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &limits).await.unwrap_err();
        assert_eq!(err, LOST);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(methods(&seen).iter().filter(|m| m.starts_with("POST")).count(), 1, "the audio isn't sent again");
    }

    /// Any real answer between Modal's errors starts the count again.
    #[tokio::test]
    async fn modal_errors_between_redirects_are_not_a_lost_job() {
        let n = Arc::new(AtomicUsize::new(0));
        let (url, _) = mock(move |method, _, _| {
            if method == "POST" {
                return Act::Reply(reply("303 See Other", "Location: /result/1\r\n", ""));
            }
            match n.fetch_add(1, Ordering::SeqCst) {
                // Three runs of three errors, a redirect (still running)
                // between them, then the result.
                0..=2 | 4..=6 | 8..=10 => Act::Reply(reply("502 Bad Gateway", "", "busy")),
                3 | 7 => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
                _ => Act::Reply(reply("200 OK", "", RESULT)),
            }
        })
        .await;
        // Covers one run of three errors (about 0.1 s) but not all nine.
        let limits = Limits { lost_after: Duration::from_millis(250), backoff: Duration::from_millis(40), max_backoff: Duration::from_millis(40), ..fast() };
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &limits).await.unwrap();
        assert_eq!(r.full_text, "Hi");
    }

    #[tokio::test]
    async fn polling_stops_at_the_overall_limit() {
        let (url, seen) = mock(|method, _, _| match method {
            "POST" => Act::Reply(reply("303 See Other", "Location: /result/1\r\n", "")),
            _ => Act::Reply(reply("503 Service Unavailable", "", "")),
        })
        .await;
        let limits = Limits { overall: Duration::from_millis(400), ..fast() };
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &limits).await.unwrap_err();
        assert_eq!(err, TIMED_OUT);
        let seen = methods(&seen);
        assert_eq!(seen.iter().filter(|m| m.starts_with("POST")).count(), 1);
        assert!(seen.len() > 2, "asked again meanwhile: {seen:?}");
    }

    /// A server that redirects at once, every time, is asked at most about
    /// once per `min_poll`, backing off, never in a tight loop.
    #[tokio::test]
    async fn an_immediate_redirect_is_not_polled_in_a_tight_loop() {
        let (url, seen) = mock(|_, _, _| Act::Reply(reply("303 See Other", "Location: /result/1\r\n", ""))).await;
        let limits = Limits {
            overall: Duration::from_millis(1500),
            min_poll: Duration::from_millis(100),
            max_backoff: Duration::from_millis(400),
            ..fast()
        };
        let started = std::time::Instant::now();
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &limits).await.unwrap_err();
        assert_eq!(err, TIMED_OUT);
        assert!(started.elapsed() >= Duration::from_millis(1400));
        let gets = methods(&seen).iter().filter(|m| m.starts_with("GET")).count();
        // 0.1 s, then 0.2, 0.4, 0.4, 0.4 s apart: about 5.
        assert!((2..=7).contains(&gets), "{gets} polls in 1.5 s");
    }

    #[tokio::test]
    async fn a_redirect_that_would_resend_the_audio_is_refused() {
        let (url, seen) = mock(|_, _, _| Act::Reply(reply("307 Temporary Redirect", "Location: /again\r\n", ""))).await;
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap_err();
        assert!(err.contains("307"), "{err}");
        assert_eq!(methods(&seen), ["POST /transcribe"]);
    }

    #[tokio::test]
    async fn a_refused_post_is_sent_again() {
        // Nothing listens at first; the server comes up during the wait.
        // (A free port, checked to refuse; it has to be one this test can
        // listen on later.)
        let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_refuses(addr);
        let url = format!("http://{}/transcribe", addr);
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 64 * 1024];
            let _ = tokio::time::timeout(Duration::from_millis(200), sock.read(&mut buf)).await;
            let _ = sock.write_all(reply("200 OK", "", RESULT).as_bytes()).await;
        });
        let delays = [Duration::from_millis(400), Duration::from_millis(400)];
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &delays, &fast()).await.unwrap();
        assert_eq!(r.full_text, "Hi");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    const NO_WAIT: [Duration; 2] = [Duration::ZERO, Duration::ZERO];

    /// Check that nothing answers at `addr`: a connection is refused.
    fn assert_refuses(addr: std::net::SocketAddr) {
        let err = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused, "{addr} should refuse: {err}");
    }

    /// A local address that refuses connections and that no test can take:
    /// a privileged port (an unprivileged process can't listen there).
    /// Checked to refuse, so a machine running something there fails loudly
    /// instead of passing by accident.
    fn refused_addr() -> std::net::SocketAddr {
        let addr: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert_refuses(addr);
        addr
    }

    #[tokio::test]
    async fn retries_only_when_the_server_was_never_reached() {
        // Nothing listening: refused before anything was sent.
        let url = format!("http://{}/", refused_addr());
        let tries = AtomicUsize::new(0);
        let c = client(Duration::from_secs(5));
        let err = send_with_retry(
            || {
                tries.fetch_add(1, Ordering::SeqCst);
                c.post(&url).body("x")
            },
            &NO_WAIT,
        )
        .await
        .unwrap_err();
        assert!(never_reached_server(&err), "{err}");
        assert_eq!(tries.load(Ordering::SeqCst), 3, "the first try and two retries");
    }

    #[tokio::test]
    async fn a_server_error_is_not_retried() {
        let (url, hits) =
            local_server(Some("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom")).await;
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap_err();
        assert!(err.contains("500"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_timeout_is_not_retried() {
        let (url, hits) = local_server(None).await;
        let c = client(Duration::from_millis(300));
        let err = send_with_retry(|| c.post(&url).body("x"), &NO_WAIT).await.unwrap_err();
        assert!(err.is_timeout(), "{err}");
        assert!(!never_reached_server(&err));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_good_answer_is_parsed() {
        let (url, hits) = local_server(Some(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 84\r\nConnection: close\r\n\r\n{\"success\":true,\"segments\":[],\"text\":\"\",\"sermon_analysis\":null,\"analysis_error\":\"x\"}",
        ))
        .await;
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, Some("k"), b"{}".to_vec(), &NO_WAIT, &fast()).await.unwrap();
        assert_eq!(r.analysis_error.as_deref(), Some("x"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
