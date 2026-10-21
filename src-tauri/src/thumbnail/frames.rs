use futures::StreamExt;
use std::path::{Path, PathBuf};

/// Format seconds as HH:MM:SS.mmm for ffmpeg.
fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00:00.000".to_string();
    }
    let hours = (seconds / 3600.0).floor() as u32;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as u32;
    let secs = seconds % 60.0;
    format!("{:02}:{:02}:{:06.3}", hours, minutes, secs)
}

/// Extract candidate frames from a video at evenly-spaced timestamps.
///
/// Returns `(timestamp, path)` pairs for successfully extracted frames.
pub async fn extract_candidate_frames(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    count: u32,
    output_dir: &Path,
) -> Result<Vec<(f64, PathBuf)>, String> {
    let duration = end_time - start_time;
    let offset = (300.0_f64).min(duration * 0.2);
    let sample_start = start_time + offset;
    let sample_end = start_time + (duration * 0.6).min(900.0);
    let sample_duration = sample_end - sample_start;
    let interval = sample_duration / count as f64;

    println!(
        "[thumbnail] Sampling frames from {:.1}min to {:.1}min into sermon",
        offset / 60.0,
        (sample_end - start_time) / 60.0
    );

    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create frame output directory: {}", e))?;

    // Grab frames a few at a time. Each grab is its own ffmpeg (seek, decode
    // one frame); 4K HEVC decoding is the slow part, so overlapping them helps.
    // JPEG at top quality instead of PNG: a 4K PNG is ~20 MB and takes ~1.5 s
    // to write (and as long again to read back), a JPEG ~0.2 MB and ~0.2 s.
    let jobs = (0..count).map(|i| {
        let timestamp = sample_start + (i as f64 * interval);
        let frame_path = output_dir.join(format!("frame_{:03}_{:.0}s.jpg", i, timestamp));
        async move {
            let output = crate::tools::command("ffmpeg")
                .args([
                    "-ss",
                    &format_time(timestamp),
                    "-i",
                    video_path,
                    "-vframes",
                    "1",
                    "-q:v",
                    "2",
                    "-y",
                    frame_path.to_str().unwrap_or("frame.jpg"),
                ])
                .output()
                .await;
            match output {
                Ok(o) if o.status.success() && frame_path.exists() => Some((timestamp, frame_path)),
                // Frame extraction failed for this timestamp, skip
                _ => None,
            }
        }
    });
    let frames: Vec<(f64, PathBuf)> = futures::stream::iter(jobs)
        .buffered(4)
        .filter_map(|f| async move { f })
        .collect()
        .await;

    if frames.is_empty() {
        return Err("No frames could be extracted from the video".to_string());
    }

    println!(
        "[thumbnail] Extracted {} of {} candidate frames",
        frames.len(),
        count
    );

    Ok(frames)
}
