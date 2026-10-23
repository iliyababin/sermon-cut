use crate::state::{VideoInfo, VideoStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
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

/// A link yt-dlp may be given: http(s), no whitespace or control
/// characters (so it can't be read as an option or split).
fn check_url(url: &str) -> Result<&str, String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let scheme = ["https://", "http://"].into_iter().find(|s| lower.starts_with(s));
    let has_host = scheme.is_some_and(|s| url.len() > s.len());
    if !has_host || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Paste a YouTube link that starts with https://".into());
    }
    Ok(url)
}

/// yt-dlp's own config files never apply, and the URL comes after `--`
/// so it's never taken for an option (docs/INVARIANTS.md #12).
fn ytdlp_args(options: &[&str], url: &str) -> Result<Vec<String>, String> {
    let url = check_url(url)?;
    let mut args = vec!["--ignore-config".to_string()];
    args.extend(options.iter().map(|s| s.to_string()));
    args.push("--".into());
    args.push(url.into());
    Ok(args)
}

fn info_args(url: &str) -> Result<Vec<String>, String> {
    ytdlp_args(&["--dump-json", "--no-download", "--no-playlist"], url)
}

pub async fn get_video_info(url: &str) -> Result<VideoInfo, String> {
    let output = crate::tools::command("yt-dlp")
        .args(info_args(url)?)
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("yt-dlp", e))?;

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

fn download_args(format: &str, output: &str, url: &str) -> Result<Vec<String>, String> {
    ytdlp_args(
        &[
            "--format", format,
            "--output", output,
            "--no-playlist",
            "--newline",
            "--progress",
            "--progress-template", "PROGRESS:%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s",
        ],
        url,
    )
}

pub async fn download_video(
    url: &str,
    quality: &str,
    video_id: &str,
    output_dir: &Path,
    mut cancel_rx: watch::Receiver<bool>,
    app_handle: AppHandle,
) -> Result<PathBuf, String> {
    // output_dir is a private, empty folder for this download (inside the
    // library's .sermon-cut/tmp), so whatever video ends up in it is ours.

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

    let mut child = crate::tools::command("yt-dlp")
        .args(download_args(format, output_template_str, url)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::tools::spawn_error("yt-dlp", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture yt-dlp stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // Drain stderr concurrently: if nobody reads it, yt-dlp blocks once the
    // pipe buffer fills. Keep the tail for the error message.
    let stderr = child.stderr.take().ok_or("Failed to capture yt-dlp stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail = std::collections::VecDeque::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if tail.len() == 20 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        Vec::from(tail).join("\n")
    });

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
                    Ok(Some(line)) => match parse_line(&line) {
                        // Post-processing (merging audio and video), once.
                        Some(Line::Processing) if !sent_processing => {
                            sent_processing = true;
                            let _ = app_handle.emit("download-progress", DownloadProgress {
                                video_id: video_id.to_string(),
                                progress: 100.0,
                                speed: None,
                                eta: None,
                                status: "processing".to_string(),
                            });
                        }
                        Some(Line::Progress { percent, speed, eta }) => {
                            // Throttled, to avoid UI jitter.
                            let now = std::time::Instant::now();
                            if now.duration_since(last_update) >= update_interval {
                                last_update = now;
                                let _ = app_handle.emit("download-progress", DownloadProgress {
                                    video_id: video_id.to_string(),
                                    progress: percent,
                                    speed,
                                    eta,
                                    status: "downloading".to_string(),
                                });
                            }
                        }
                        _ => {}
                    },
                    Ok(None) => break,
                    Err(e) => return Err(format!("Error reading output: {}", e)),
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;

    if !status.success() {
        let stderr_tail = stderr_task.await.unwrap_or_default();
        return Err(format!("Download failed: {}", stderr_tail.trim()));
    }

    find_downloaded_file(output_dir)
}

/// The finished video in a download folder: the largest file with a video
/// extension (yt-dlp removes its .part and per-format files once merged).
fn find_downloaded_file(dir: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Failed to read download folder: {}", e))?;
    entries
        .flatten()
        .filter(|e| crate::library::is_candidate_video(&e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
        .max_by_key(|(len, _)| *len)
        .map(|(_, path)| path)
        .ok_or_else(|| "Download finished but no video file was found".to_string())
}

/// What a line of yt-dlp's output says.
#[derive(Debug, PartialEq)]
enum Line {
    /// A progress line (from our --progress-template).
    Progress { percent: f32, speed: Option<String>, eta: Option<String> },
    /// Downloading is done; the parts are being merged or converted.
    Processing,
}

fn parse_line(line: &str) -> Option<Line> {
    if line.contains("[Merger]") || line.contains("[ffmpeg]") || line.contains("[ExtractAudio]") {
        return Some(Line::Processing);
    }
    let (percent, speed, eta) = parse_progress(line)?;
    Some(Line::Progress { percent, speed, eta })
}

/// "PROGRESS:percent|speed|eta" (see download_args); unknown parts (empty,
/// N/A, Unknown) are None. Colour codes, if yt-dlp adds any, are ignored.
fn parse_progress(line: &str) -> Option<(f32, Option<String>, Option<String>)> {
    let line = strip_ansi(line);
    let data = line.trim().strip_prefix("PROGRESS:")?;
    let mut parts = data.split('|').map(str::trim);
    let percent = parts.next()?.trim_end_matches('%').trim().parse::<f32>().ok()?;
    if !percent.is_finite() {
        return None;
    }
    let known = |s: Option<&str>| s.filter(|s| !s.is_empty() && *s != "N/A" && *s != "Unknown").map(str::to_string);
    let speed = known(parts.next());
    let eta = known(parts.next());
    Some((percent.clamp(0.0, 100.0), speed, eta))
}

/// `s` without ANSI escape sequences (ESC [ ... letter).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_links_are_passed_on() {
        for bad in [
            "--exec=touch /tmp/x",
            "-o/tmp/x",
            "file:///etc/passwd",
            "ftp://example.com/v",
            "youtube.com/watch?v=abc",
            "https://youtube.com/watch?v=a b",
            "https://youtube.com/watch?v=a\n--exec=x",
            "https://youtube.com/\twatch",
            "https://",
            "",
        ] {
            assert!(info_args(bad).is_err(), "{bad:?}");
            assert!(download_args("best", "/tmp/%(id)s.%(ext)s", bad).is_err(), "{bad:?}");
        }
        for good in ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "HTTP://youtu.be/dQw4w9WgXcQ"] {
            assert!(info_args(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn config_is_ignored_and_the_url_comes_after_the_separator() {
        let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
        for args in [info_args(url).unwrap(), download_args("best", "/tmp/%(id)s.%(ext)s", url).unwrap()] {
            assert_eq!(args[0], "--ignore-config");
            assert_eq!(&args[args.len() - 2..], ["--", url]);
            assert_eq!(args.iter().filter(|a| *a == "--").count(), 1);
        }
    }

    #[test]
    fn progress_lines_are_read() {
        let p = |percent: f32, speed: Option<&str>, eta: Option<&str>| {
            Some(Line::Progress { percent, speed: speed.map(Into::into), eta: eta.map(Into::into) })
        };
        assert_eq!(parse_line("PROGRESS: 12.3%|2.50MiB/s|00:42"), p(12.3, Some("2.50MiB/s"), Some("00:42")));
        assert_eq!(parse_line("  PROGRESS:100%|N/A|Unknown  "), p(100.0, None, None));
        assert_eq!(parse_line("PROGRESS:7.0%||"), p(7.0, None, None));
        assert_eq!(parse_line("PROGRESS:7.0%"), p(7.0, None, None));
        // Coloured output.
        assert_eq!(parse_line("PROGRESS:\u{1b}[0;94m 45.1%\u{1b}[0m|\u{1b}[0;32m1.0MiB/s\u{1b}[0m|00:10"), p(45.1, Some("1.0MiB/s"), Some("00:10")));
        // Out of range is kept in range.
        assert_eq!(parse_line("PROGRESS:100.4%|x|y"), p(100.0, Some("x"), Some("y")));
    }

    #[test]
    fn other_lines_are_not_progress() {
        for line in [
            "",
            "[youtube] dQw4w9WgXcQ: Downloading webpage",
            "[download] Destination: /tmp/x.webm",
            "PROGRESS:N/A|N/A|N/A",
            "PROGRESS:abc%|1|2",
            "PROGRESS:NaN%|1|2",
            "progress:12%|1|2",
            "xPROGRESS:12%|1|2",
        ] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn merging_is_processing() {
        for line in [
            "[Merger] Merging formats into \"/tmp/x.mkv\"",
            "[ffmpeg] Merging formats into \"/tmp/x.mp4\"",
            "[ExtractAudio] Destination: /tmp/x.m4a",
        ] {
            assert_eq!(parse_line(line), Some(Line::Processing), "{line:?}");
        }
    }
}
