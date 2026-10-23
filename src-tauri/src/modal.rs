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
}

#[derive(Debug, Deserialize)]
struct ModalChapter {
    time: f64,
    title: String,
}

#[derive(Debug, Serialize)]
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
                eprintln!("[modal] Couldn't reach the server ({}); trying again", e);
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(40 * 60)) // upload + transcription + analysis of a long service
        // Modal answers requests that run past 150 s with a redirect to wait
        // on; each hop is another 150 s, so allow enough for a long service.
        .redirect(reqwest::redirect::Policy::limited(20))
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(600))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    transcribe_with(&client, api_url, api_key, body, &RETRY_DELAYS).await
}

async fn transcribe_with(
    client: &reqwest::Client,
    api_url: &str,
    api_key: Option<&str>,
    body: Vec<u8>,
    delays: &[Duration],
) -> Result<TranscriptionResult, String> {
    let make = || {
        let request = client
            .post(api_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        match api_key {
            Some(key) => request.header("Authorization", format!("Bearer {}", key)),
            None => request,
        }
    };
    let response = send_with_retry(make, delays)
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("API error ({}): {}", status, text));
    }

    let bytes = response.bytes().await.map_err(|e| format!("Failed to read response: {}", e))?;
    parse_response(&bytes)
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
    let (sermon_start, sermon_end, suggested_title, suggested_description, suggested_chapters) =
        if let Some(analysis) = modal_response.sermon_analysis {
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
    })
}

#[cfg(test)]
mod tests {
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
        reqwest::Client::builder().timeout(timeout).build().unwrap()
    }

    const NO_WAIT: [Duration; 2] = [Duration::ZERO, Duration::ZERO];

    #[tokio::test]
    async fn retries_only_when_the_server_was_never_reached() {
        // Nothing listening: refused before anything was sent.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{}/", port);
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
        let err = transcribe_with(&client(Duration::from_secs(5)), &url, None, b"{}".to_vec(), &NO_WAIT).await.unwrap_err();
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
        let r = transcribe_with(&client(Duration::from_secs(5)), &url, Some("k"), b"{}".to_vec(), &NO_WAIT).await.unwrap();
        assert_eq!(r.analysis_error.as_deref(), Some("x"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
