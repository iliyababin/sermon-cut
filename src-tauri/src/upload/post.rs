//! After the video is on YouTube: thumbnail, captions, playlist. None of
//! them fails the upload; each problem becomes a warning stored with the
//! upload record, and the thumbnail can be set again later.

use super::error::UploadError;
use super::{content_type, Endpoints, TokenSource};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

/// Thumbnails, captions and playlist calls are small; this is plenty.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    Thumbnail,
    Captions,
    Playlist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadWarning {
    pub kind: WarningKind,
    pub message: String,
}

impl UploadWarning {
    pub fn thumbnail(e: &UploadError) -> Self {
        UploadWarning { kind: WarningKind::Thumbnail, message: format!("The thumbnail wasn't set. {}", e.user_message()) }
    }
}

/// A thumbnail ready to send. Held in memory from the moment the upload
/// starts, so edits made meanwhile (which replace render files) can't
/// change or remove what's sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub data: Vec<u8>,
    pub mime: &'static str,
}

impl Thumbnail {
    /// `data`, read from `path`, within YouTube's limits (1280×720, under
    /// 2 MB), in memory: the file stays as the user made it. CPU work.
    pub fn prepare(data: Vec<u8>, path: &Path) -> Self {
        match crate::thumbnail::fit_for_youtube(&data) {
            Ok(jpeg) => Thumbnail { data: jpeg, mime: "image/jpeg" },
            Err(e) => {
                // Let YouTube judge it; a refusal becomes an upload warning.
                log::warn!("Sending the thumbnail as it is ({})", e);
                let mime = match content_type(path) {
                    "image/png" => "image/png",
                    _ => "image/jpeg",
                };
                Thumbnail { data, mime }
            }
        }
    }

    /// Read and prepare the file at `path`. Blocking.
    pub fn read(path: &Path) -> Result<Self, UploadError> {
        let data = std::fs::read(path).map_err(|e| UploadError::File(e.to_string()))?;
        Ok(Self::prepare(data, path))
    }
}

/// What to do once the video is up.
#[derive(Debug, Default)]
pub struct AfterUpload {
    pub thumbnail: Option<Thumbnail>,
    /// SRT text; empty or None skips captions.
    pub captions: Option<String>,
    pub playlist_id: Option<String>,
}

/// Run a call with a fresh token, and once more with a refreshed one after
/// a 401.
async fn authed<F, Fut>(tokens: &dyn TokenSource, call: F) -> Result<(), UploadError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<(), UploadError>>,
{
    let token = tokens.token().await?;
    match call(token).await {
        Err(UploadError::Auth(_)) => call(tokens.refresh().await?).await,
        r => r,
    }
}

/// The post-upload steps; returns what went wrong, as warnings.
pub async fn after_upload(
    http: &Client,
    endpoints: &Endpoints,
    tokens: &dyn TokenSource,
    video_id: &str,
    steps: &AfterUpload,
) -> Vec<UploadWarning> {
    let mut warnings = Vec::new();

    if let Some(thumb) = &steps.thumbnail {
        if let Err(e) = authed(tokens, |t| async move { set_thumbnail(http, endpoints, &t, video_id, thumb).await }).await {
            log::warn!("Thumbnail failed: {}", e.log_text());
            warnings.push(UploadWarning::thumbnail(&e));
        }
    }

    if let Some(srt) = steps.captions.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Err(e) = authed(tokens, |t| async move { upload_captions(http, endpoints, &t, video_id, srt).await }).await {
            log::warn!("Captions failed: {}", e.log_text());
            warnings.push(UploadWarning {
                kind: WarningKind::Captions,
                message: format!("Captions weren't added. {}", e.user_message()),
            });
        }
    }

    if let Some(playlist) = steps.playlist_id.as_deref() {
        if let Err(e) = authed(tokens, |t| async move { add_to_playlist(http, endpoints, &t, playlist, video_id).await }).await {
            log::warn!("Playlist failed: {}", e.log_text());
            warnings.push(UploadWarning {
                kind: WarningKind::Playlist,
                message: format!("The video wasn't added to the playlist. {}", e.user_message()),
            });
        }
    }

    warnings
}

async fn check(resp: Result<reqwest::Response, reqwest::Error>) -> Result<(), UploadError> {
    let resp = resp.map_err(UploadError::from_reqwest)?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(UploadError::from_response(status, body, None, false))
}

pub async fn set_thumbnail(
    http: &Client,
    endpoints: &Endpoints,
    token: &str,
    video_id: &str,
    thumbnail: &Thumbnail,
) -> Result<(), UploadError> {
    let (data, mime) = (thumbnail.data.clone(), thumbnail.mime);
    log::info!("Setting the thumbnail for {} ({} bytes)", video_id, data.len());
    let resp = http
        .post(format!("{}?videoId={}&uploadType=media", endpoints.thumbnails, urlencoding::encode(video_id)))
        .bearer_auth(token)
        .timeout(REQUEST_TIMEOUT)
        .header(CONTENT_TYPE, mime)
        .header(CONTENT_LENGTH, data.len().to_string())
        .body(data)
        .send()
        .await;
    check(resp).await
}

pub async fn upload_captions(
    http: &Client,
    endpoints: &Endpoints,
    token: &str,
    video_id: &str,
    srt: &str,
) -> Result<(), UploadError> {
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
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(metadata.to_string().as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(srt.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let resp = http
        .post(format!("{}?part=snippet", endpoints.captions))
        .bearer_auth(token)
        .timeout(REQUEST_TIMEOUT)
        .header(CONTENT_TYPE, format!("multipart/related; boundary={}", boundary))
        .header(CONTENT_LENGTH, body.len().to_string())
        .body(body)
        .send()
        .await;
    check(resp).await
}

/// `videos.update`: give a video on YouTube this title and description
/// (the rest of its snippet as Sermon Cut uploads it). Its privacy, file and
/// everything else stay.
pub async fn update_metadata(
    http: &Client,
    endpoints: &Endpoints,
    token: &str,
    video_id: &str,
    title: &str,
    description: &str,
) -> Result<(), UploadError> {
    let mut snippet = super::client::video_metadata(title, description, "private")["snippet"].clone();
    let body = serde_json::json!({ "id": video_id, "snippet": snippet.take() });
    let resp = http
        .put(format!("{}?part=snippet", endpoints.videos))
        .bearer_auth(token)
        .timeout(REQUEST_TIMEOUT)
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await;
    check(resp).await
}

/// `update_metadata` with a fresh token, refreshed once after a 401.
pub async fn update_metadata_authed(
    http: &Client,
    endpoints: &Endpoints,
    tokens: &dyn TokenSource,
    video_id: &str,
    title: &str,
    description: &str,
) -> Result<(), UploadError> {
    authed(tokens, |t| async move { update_metadata(http, endpoints, &t, video_id, title, description).await }).await
}

pub async fn add_to_playlist(
    http: &Client,
    endpoints: &Endpoints,
    token: &str,
    playlist_id: &str,
    video_id: &str,
) -> Result<(), UploadError> {
    let body = serde_json::json!({
        "snippet": {
            "playlistId": playlist_id,
            "resourceId": { "kind": "youtube#video", "videoId": video_id }
        }
    });
    let resp = http
        .post(format!("{}?part=snippet", endpoints.playlist_items))
        .bearer_auth(token)
        .timeout(REQUEST_TIMEOUT)
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await;
    check(resp).await
}

/// SRT for the trimmed video: cues shifted by `start_offset`, those outside
/// the clip dropped, and every cue clamped to the clip's length.
pub fn generate_srt(segments: &[crate::state::TranscriptSegment], start_offset: f64, end_time: f64) -> String {
    let clip_len = end_time - start_offset;
    let mut out = Vec::new();
    let mut counter = 1;
    for seg in segments {
        let start = (seg.start - start_offset).max(0.0);
        let end = (seg.end - start_offset).min(clip_len);
        if seg.text.trim().is_empty() || end <= start || start >= clip_len {
            continue;
        }
        out.push(counter.to_string());
        out.push(format!("{} --> {}", format_srt_timestamp(start), format_srt_timestamp(end)));
        out.push(seg.text.trim().to_string());
        out.push(String::new());
        counter += 1;
    }
    out.join("\n")
}

fn format_srt_timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let (h, rest) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TranscriptSegment;

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment { start, end, text: text.into() }
    }

    #[test]
    fn srt_cues_are_shifted_and_clamped_to_the_clip() {
        let segs = vec![
            seg(5.0, 12.0, "before and into the clip"),
            seg(20.0, 25.5, " middle "),
            seg(95.0, 130.0, "runs past the end"),
            seg(100.0, 101.0, "starts at the end"),
            seg(140.0, 150.0, "after"),
            seg(30.0, 31.0, "   "),
        ];
        let srt = generate_srt(&segs, 10.0, 100.0);
        assert_eq!(
            srt,
            "1\n00:00:00,000 --> 00:00:02,000\nbefore and into the clip\n\n\
             2\n00:00:10,000 --> 00:00:15,500\nmiddle\n\n\
             3\n00:01:25,000 --> 00:01:30,000\nruns past the end\n"
        );
    }

    #[test]
    fn srt_timestamps_round_properly() {
        assert_eq!(format_srt_timestamp(3661.2345), "01:01:01,235");
        assert_eq!(format_srt_timestamp(0.1), "00:00:00,100");
        assert_eq!(format_srt_timestamp(-1.0), "00:00:00,000");
    }

    #[test]
    fn open_ended_clip_keeps_everything_after_the_start() {
        let srt = generate_srt(&[seg(1.0, 2.0, "a")], 0.0, f64::MAX);
        assert!(srt.contains("00:00:01,000 --> 00:00:02,000"));
    }
}
