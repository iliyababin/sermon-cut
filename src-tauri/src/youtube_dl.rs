use crate::state::{VideoInfo, VideoStatus};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    pub video_id: String,
    pub progress: f32,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct YtDlpInfo {
    id: String,
    title: String,
    thumbnail: Option<String>,
    duration: Option<f64>,
    channel: Option<String>,
    upload_date: Option<String>,
    webpage_url: String,
}

pub async fn get_video_info(url: &str) -> Result<VideoInfo, String> {
    let output = Command::new("yt-dlp")
        .args([
            "--dump-json",
            "--no-download",
            "--no-playlist",
            url,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp error: {}", stderr));
    }

    let info: YtDlpInfo = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse yt-dlp output: {}", e))?;

    Ok(VideoInfo {
        id: info.id,
        title: info.title,
        url: info.webpage_url,
        thumbnail: info.thumbnail,
        duration: info.duration,
        channel: info.channel,
        upload_date: info.upload_date,
        file_path: None,
        audio_path: None,
        thumbnail_path: None,
        transcription: None,
        status: VideoStatus::Pending,
        download_progress: 0.0,
        created_at: chrono::Utc::now().to_rfc3339(),
        trimmed_path: None,
        thumbnail_options: None,
        processing_stage: None,
        processing_progress: None,
    })
}

pub async fn download_video(
    url: &str,
    quality: &str,
    video_id: &str,
    output_dir: &str,
    mut cancel_rx: watch::Receiver<bool>,
    app_handle: AppHandle,
) -> Result<String, String> {
    // Get output directory: use provided setting, or fall back to ~/Videos/sermon-cut
    let output_dir = if output_dir.is_empty() {
        let base = dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        base.join("sermon-cut")
    } else {
        std::path::PathBuf::from(output_dir)
    };
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let output_template = output_dir.join("%(id)s.%(ext)s");
    let output_template_str = output_template.to_str()
        .ok_or("output template path contains invalid UTF-8")?;

    // Build format string based on quality
    // Don't force mp4 - let yt-dlp pick the best format (often webm/vp9)
    let format = match quality {
        "best" => "bestvideo+bestaudio/best",
        "2160p" => "bestvideo[height<=2160]+bestaudio/best[height<=2160]/best",
        "1440p" => "bestvideo[height<=1440]+bestaudio/best[height<=1440]/best",
        "1080p" => "bestvideo[height<=1080]+bestaudio/best[height<=1080]/best",
        "720p" => "bestvideo[height<=720]+bestaudio/best[height<=720]/best",
        "480p" => "bestvideo[height<=480]+bestaudio/best[height<=480]/best",
        _ => "bestvideo+bestaudio/best",
    };

    let mut child = Command::new("yt-dlp")
        .args([
            "--format", format,
            "--output", output_template_str,
            "--no-playlist",
            "--newline",
            "--progress",
            "--progress-template", "PROGRESS:%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start yt-dlp: {}", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture yt-dlp stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // Capture the output path
    let mut output_path = String::new();

    // Throttle progress updates to avoid UI jitter
    let mut last_update = std::time::Instant::now();
    let update_interval = std::time::Duration::from_secs(2);
    let mut sent_processing = false;

    loop {
        tokio::select! {
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    child.kill().await.ok();
                    return Err("Download cancelled".to_string());
                }
            }
            line = reader.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        // Detect post-processing phase (merging audio/video)
                        if line.contains("[Merger]") || line.contains("[ffmpeg]") || line.contains("[ExtractAudio]") {
                            if !sent_processing {
                                sent_processing = true;
                                let _ = app_handle.emit("download-progress", DownloadProgress {
                                    video_id: video_id.to_string(),
                                    progress: 100.0,
                                    speed: None,
                                    eta: None,
                                    status: "processing".to_string(),
                                });
                            }
                        }
                        // Parse progress info
                        else if let Some(progress_info) = parse_progress(&line) {
                            // Only emit if enough time has passed (throttle)
                            let now = std::time::Instant::now();
                            if now.duration_since(last_update) >= update_interval {
                                last_update = now;
                                let _ = app_handle.emit("download-progress", DownloadProgress {
                                    video_id: video_id.to_string(),
                                    progress: progress_info.0,
                                    speed: progress_info.1,
                                    eta: progress_info.2,
                                    status: "downloading".to_string(),
                                });
                            }
                        }
                        // Check for output path in line
                        if line.contains("[download] Destination:") {
                            if let Some(path) = line.strip_prefix("[download] Destination: ") {
                                output_path = path.trim().to_string();
                            }
                        }
                        if line.contains("[Merger] Merging formats into") {
                            if let Some(path) = line.strip_prefix("[Merger] Merging formats into \"") {
                                output_path = path.trim_end_matches('"').to_string();
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(format!("Error reading output: {}", e)),
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;

    if !status.success() {
        return Err("Download failed".to_string());
    }

    // If we didn't capture the path, try to find it
    if output_path.is_empty() {
        // Get video ID from URL and find the file
        let info = get_video_info(url).await?;
        let pattern = output_dir.join(format!("{}.*", info.id));

        if let Some(pattern_str) = pattern.to_str() {
            if let Ok(entries) = glob::glob(pattern_str) {
                for entry in entries.flatten() {
                    if entry.extension().map_or(false, |e| e == "mp4" || e == "mkv" || e == "webm") {
                        output_path = entry.to_string_lossy().to_string();
                        break;
                    }
                }
            }
        }
    }

    if output_path.is_empty() {
        return Err("Could not determine output file path".to_string());
    }

    Ok(output_path)
}

fn parse_progress(line: &str) -> Option<(f32, Option<String>, Option<String>)> {
    // Parse progress line format: "PROGRESS:XX.X%|speed|eta"
    let line = line.trim();
    if !line.starts_with("PROGRESS:") {
        return None;
    }

    let data = line.strip_prefix("PROGRESS:")?;
    let parts: Vec<&str> = data.split('|').collect();

    if parts.is_empty() {
        return None;
    }

    // Parse percentage
    let percent_str = parts[0].trim().trim_end_matches('%');
    let percent = percent_str.parse::<f32>().ok()?;

    // Parse speed (optional)
    let speed = parts.get(1)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "N/A" && *s != "Unknown")
        .map(|s| s.to_string());

    // Parse ETA (optional)
    let eta = parts.get(2)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "N/A" && *s != "Unknown")
        .map(|s| s.to_string());

    Some((percent, speed, eta))
}
