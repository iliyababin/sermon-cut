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

/// Candidate frames are at least this far apart: closer ones show the same
/// shot, and each costs an ffmpeg run.
const MIN_FRAME_SPACING_SECS: f64 = 20.0;

/// Up to `count` frames between `start_time` and `end_time`, at least
/// `MIN_FRAME_SPACING_SECS` apart: fewer for a short clip, and always one.
/// Timestamps are in seconds; an unknown or empty range is an error.
fn sample_times(start_time: f64, end_time: f64, count: u32) -> Result<Vec<f64>, String> {
    let duration = end_time - start_time;
    if !start_time.is_finite() || !duration.is_finite() || duration <= 0.0 {
        return Err("Couldn't tell how long the video is, so no thumbnails could be made. Try processing it again.".into());
    }
    let offset = (300.0_f64).min(duration * 0.2);
    let sample_start = start_time + offset;
    let sample_end = start_time + (duration * 0.6).min(900.0);
    let sample_duration = (sample_end - sample_start).max(0.0);
    let count = count.min((sample_duration / MIN_FRAME_SPACING_SECS).floor() as u32).max(1);
    let interval = sample_duration / count as f64;
    Ok((0..count).map(|i| sample_start + i as f64 * interval).collect())
}

/// Extract candidate frames from a video at evenly-spaced timestamps (see
/// `sample_times`).
///
/// Returns `(timestamp, path)` pairs for successfully extracted frames.
pub async fn extract_candidate_frames(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    count: u32,
    output_dir: &Path,
) -> Result<Vec<(f64, PathBuf)>, String> {
    let times = sample_times(start_time, end_time, count)?;
    let count = times.len();
    println!(
        "[thumbnail] Sampling {} frames from {:.1}min to {:.1}min into the sermon",
        count,
        (times[0] - start_time) / 60.0,
        (times[count - 1] - start_time) / 60.0
    );

    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create frame output directory: {}", e))?;

    // Grab frames a few at a time. Each grab is its own ffmpeg (seek, decode
    // one frame); 4K HEVC decoding is the slow part, so overlapping them helps.
    // JPEG at top quality instead of PNG: a 4K PNG is ~20 MB and takes ~1.5 s
    // to write (and as long again to read back), a JPEG ~0.2 MB and ~0.2 s.
    let jobs = times.into_iter().enumerate().map(|(i, timestamp)| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_spaced_out_and_short_clips_get_fewer() {
        // A two-hour service: 100 asked for, 20 s apart at least.
        let t = sample_times(0.0, 7200.0, 100).unwrap();
        assert_eq!(t.len(), 30);
        assert!(t.windows(2).all(|w| w[1] - w[0] >= MIN_FRAME_SPACING_SECS - 1e-9));
        assert!(t[0] >= 300.0 && *t.last().unwrap() < 900.0);

        // A one-minute clip: a single frame, inside it.
        let t = sample_times(100.0, 160.0, 100).unwrap();
        assert_eq!(t.len(), 1);
        assert!(t[0] >= 100.0 && t[0] < 160.0);
        // Ten minutes: a few.
        assert_eq!(sample_times(0.0, 600.0, 100).unwrap().len(), 12);
    }

    #[test]
    fn an_unknown_length_is_a_friendly_error() {
        for (start, end) in [(0.0, 0.0), (10.0, 5.0), (0.0, f64::NAN), (f64::INFINITY, 1.0)] {
            let err = sample_times(start, end, 10).unwrap_err();
            assert!(err.starts_with("Couldn't tell how long the video is"), "{err}");
        }
    }
}
