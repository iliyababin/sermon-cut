use std::path::Path;

/// Extract a video's audio as 16 kHz mono WAV (what Whisper expects).
pub async fn extract_audio(video_path: &Path, output_path: &Path) -> Result<(), String> {
    let video_path_str = video_path.to_str().ok_or("video path contains invalid UTF-8")?;
    let output_path_str = output_path.to_str().ok_or("output path contains invalid UTF-8")?;

    let output = crate::tools::command("ffmpeg")
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
        .map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr));
    }

    Ok(())
}

/// Trim video to specified start and end times
pub async fn trim_video(
    video_path: &Path,
    start_time: f64,
    end_time: f64,
    output_path: &Path,
) -> Result<(), String> {
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

    let duration = end_time - start_time;

    let video_path_str = video_path.to_str().ok_or("video path contains invalid UTF-8")?;
    let output_path_str = output_path.to_str().ok_or("output path contains invalid UTF-8")?;

    let output = crate::tools::command("ffmpeg")
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
        .map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr));
    }

    Ok(())
}

/// Blocking ffprobe for the library scan. None if the file can't be read
/// (still copying, corrupt, or not a video). Gives up after 20 seconds so a
/// stuck network drive can't hang the scan.
pub fn probe_duration_blocking(video_path: &Path) -> Option<f64> {
    use std::process::Stdio;
    let mut child = crate::tools::std_command("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(video_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                crate::tools::mark_missing("ffprobe");
            }
        })
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(_) => return None,
        }
    }
    crate::tools::clear_missing("ffprobe");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    out.trim().parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0)
}

/// Get video duration in seconds
pub async fn get_duration(video_path: &str) -> Result<f64, String> {
    let output = crate::tools::command("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
            video_path,
        ])
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("ffprobe", e))?;

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
    let output = crate::tools::command("ffmpeg")
        .args([
            "-ss", &format_time(time),
            "-i", video_path,
            "-vframes", "1",
            "-y",
            output_path,
        ])
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;

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
