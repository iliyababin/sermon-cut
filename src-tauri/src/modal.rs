use crate::state::{Chapter, TranscriptionResult, TranscriptSegment};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::io::AsyncReadExt;

#[derive(Debug, Deserialize)]
struct ModalTranscriptionResponse {
    segments: Vec<ModalSegment>,
    text: String,
    sermon_analysis: Option<SermonAnalysis>,
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

    // Encode as base64
    let audio_base64 = STANDARD.encode(&buffer);

    // Build JSON request
    let request_body = TranscribeRequest { audio_base64 };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600)) // 10 min total timeout
        .connect_timeout(std::time::Duration::from_secs(30))
        .pool_idle_timeout(std::time::Duration::from_secs(600))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let mut request = client.post(api_url).json(&request_body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    // Send request
    let response = request
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("API error ({}): {}", status, text));
    }

    let modal_response: ModalTranscriptionResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Convert to our types
    let segments: Vec<TranscriptSegment> = modal_response
        .segments
        .into_iter()
        .map(|s| TranscriptSegment {
            start: s.start,
            end: s.end,
            text: s.text,
        })
        .collect();

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
    })
}
