use std::path::Path;
use tokio::process::Command;

/// Extract audio from video file as WAV
pub async fn extract_audio(video_path: &str, output_dir: &str) -> Result<String, String> {
    let video_path = Path::new(video_path);
    let video_stem = video_path
        .file_stem()
        .ok_or("Invalid video path")?
        .to_string_lossy();

    // Use provided output dir or fall back to ~/Videos/sermon-cut
    let default_dir;
    let output_dir = if output_dir.is_empty() {
        default_dir = dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("sermon-cut");
        default_dir.as_path()
    } else {
        Path::new(output_dir)
    };

    // Create audio subdirectory
    let audio_dir = output_dir.join("audio");
    std::fs::create_dir_all(&audio_dir)
        .map_err(|e| format!("Failed to create audio directory: {}", e))?;

    let output_path = audio_dir.join(format!("{}.wav", video_stem));

    let video_path_str = video_path.to_str().ok_or("video path contains invalid UTF-8")?;
    let output_path_str = output_path.to_str().ok_or("output path contains invalid UTF-8")?;

    let output = Command::new("ffmpeg")
        .args([
            "-i", video_path_str,
            "-vn",                    // No video
            "-acodec", "pcm_s16le",   // PCM 16-bit little-endian
            "-ar", "16000",           // 16kHz sample rate (optimal for Whisper)
            "-ac", "1",               // Mono
            "-y",                     // Overwrite
            output_path_str,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr));
    }

    Ok(output_path.to_string_lossy().to_string())
}

/// Trim video to specified start and end times
pub async fn trim_video(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    output_dir: &str,
) -> Result<String, String> {
    // Validate time inputs
    if !start_time.is_finite() || !end_time.is_finite() {
        return Err("Start and end times must be finite numbers".to_string());
    }
    if start_time < 0.0 || end_time < 0.0 {
        return Err("Start and end times must be non-negative".to_string());
    }
    if start_time >= end_time {
        return Err(format!("Start time ({:.2}s) must be less than end time ({:.2}s)", start_time, end_time));
    }

    let video_path = Path::new(video_path);
    let video_stem = video_path
        .file_stem()
        .ok_or("Invalid video path")?
        .to_string_lossy();

    let ext = video_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".to_string());

    // Use provided output dir or fall back to ~/Videos/sermon-cut
    let default_dir;
    let output_dir = if output_dir.is_empty() {
        default_dir = dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("sermon-cut");
        default_dir.as_path()
    } else {
        Path::new(output_dir)
    };

    // Create trimmed subdirectory
    let trimmed_dir = output_dir.join("trimmed");
    std::fs::create_dir_all(&trimmed_dir)
        .map_err(|e| format!("Failed to create trimmed directory: {}", e))?;

    let output_path = trimmed_dir.join(format!("{}_trimmed.{}", video_stem, ext));

    let duration = end_time - start_time;

    let video_path_str = video_path.to_str().ok_or("video path contains invalid UTF-8")?;
    let output_path_str = output_path.to_str().ok_or("output path contains invalid UTF-8")?;

    let output = Command::new("ffmpeg")
        .args([
            "-ss", &format_time(start_time),
            "-i", video_path_str,
            "-t", &format_time(duration),
            "-c", "copy",             // Stream copy (fast, no re-encoding)
            "-avoid_negative_ts", "make_zero",
            "-y",                     // Overwrite
            output_path_str,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr));
    }

    Ok(output_path.to_string_lossy().to_string())
}

/// Get video duration in seconds
pub async fn get_duration(video_path: &str) -> Result<f64, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
            video_path,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run ffprobe: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe error: {}", stderr));
    }

    let duration_str = String::from_utf8_lossy(&output.stdout);
    duration_str
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Failed to parse duration: {}", e))
}

/// Extract a single frame at specified time
pub async fn extract_frame(
    video_path: &str,
    time: f64,
    output_path: &str,
) -> Result<(), String> {
    let output = Command::new("ffmpeg")
        .args([
            "-ss", &format_time(time),
            "-i", video_path,
            "-vframes", "1",
            "-y",
            output_path,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr));
    }

    Ok(())
}

/// Format time in seconds to HH:MM:SS.mmm format
fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00:00.000".to_string();
    }
    let hours = (seconds / 3600.0).floor() as u32;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as u32;
    let secs = seconds % 60.0;
    format!("{:02}:{:02}:{:06.3}", hours, minutes, secs)
}
