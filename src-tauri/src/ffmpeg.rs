use std::path::Path;

/// Extract a video's audio for transcription: 16 kHz mono (what Whisper
/// uses) as Opus. Speech at 24 kb/s transcribes the same as WAV and is about
/// 15× smaller (~11 MB for a 2-hour service), so the upload is quick.
pub async fn extract_audio(video_path: &Path, output_path: &Path) -> Result<(), String> {
    let video_path_str = video_path.to_str().ok_or("video path contains invalid UTF-8")?;
    let output_path_str = output_path.to_str().ok_or("output path contains invalid UTF-8")?;

    let output = crate::tools::command("ffmpeg")
        .args([
            "-i", video_path_str,
            "-vn",                    // No video
            "-ar", "16000",           // 16kHz sample rate (optimal for Whisper)
            "-ac", "1",               // Mono
            "-c:a", "libopus",        // Opus in an Ogg container
            "-b:a", "24k",
            "-application", "voip",   // Tuned for speech
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

fn check_times(start_time: f64, end_time: f64) -> Result<(), String> {
    if !start_time.is_finite() || !end_time.is_finite() {
        return Err("Start and end times must be finite numbers".to_string());
    }
    if start_time < 0.0 || end_time < 0.0 {
        return Err("Start and end times must be non-negative".to_string());
    }
    if start_time >= end_time {
        return Err(format!("Start time ({:.2}s) must be less than end time ({:.2}s)", start_time, end_time));
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| "path contains invalid UTF-8".to_string())
}

/// Run ffmpeg (quiet) with `args`; Err with its message if it fails.
async fn run_ffmpeg(args: &[&str]) -> Result<(), String> {
    let output = crate::tools::command("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "error"])
        .args(args)
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg error: {}", stderr.trim()));
    }
    Ok(())
}

/// Run ffprobe (quiet) with `args` and return what it printed.
async fn run_ffprobe(args: &[&str]) -> Result<String, String> {
    let output = crate::tools::command("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("ffprobe", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe error: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Trim video to specified start and end times by stream copy. The cut
/// begins at the keyframe at or before `start_time`.
pub async fn trim_video(
    video_path: &Path,
    start_time: f64,
    end_time: f64,
    output_path: &Path,
) -> Result<(), String> {
    check_times(start_time, end_time)?;
    let duration = end_time - start_time;
    run_ffmpeg(&[
        "-ss", &format_time(start_time),
        "-i", path_str(video_path)?,
        "-t", &format_time(duration),
        "-c", "copy",             // Stream copy (fast, no re-encoding)
        "-avoid_negative_ts", "make_zero",
        "-y",                     // Overwrite
        path_str(output_path)?,
    ])
    .await
}

/// A start this close after a keyframe is cut at the keyframe itself
/// (stream copy, a moment early); further away, the start is cut exactly.
pub const SNAP_SECS: f64 = 2.0;

/// ffmpeg seeks to the keyframe at or before the time it's given, so a
/// keyframe's own time is passed a little late to never land on the one
/// before it after rounding.
const SEEK_PAD: f64 = 0.001;

/// Keyframe times (seconds) of the first video stream within `interval`
/// (ffprobe's -read_intervals syntax), in order.
async fn keyframes_in(video_path: &Path, interval: &str) -> Result<Vec<f64>, String> {
    let out = run_ffprobe(&[
        "-select_streams", "v:0",
        "-skip_frame", "nokey",
        "-show_entries", "frame=pts_time",
        "-of", "csv=p=0",
        "-read_intervals", interval,
        path_str(video_path)?,
    ])
    .await?;
    Ok(parse_times(&out))
}

/// Times from ffprobe's csv output, one per line ("12.345000," or "N/A").
fn parse_times(out: &str) -> Vec<f64> {
    let mut times: Vec<f64> = out
        .lines()
        .filter_map(|l| l.split(',').next()?.trim().parse::<f64>().ok())
        .filter(|t| t.is_finite())
        .collect();
    times.sort_by(|a, b| a.total_cmp(b));
    times
}

/// The last keyframe at or before `t` (0 if none is found).
pub async fn keyframe_at_or_before(video_path: &Path, t: f64) -> Result<f64, String> {
    for window in [30.0, 120.0, 600.0, f64::INFINITY] {
        let from = (t - window).max(0.0);
        let interval = format!("{:.3}%{:.3}", from, t + 0.05);
        let found = keyframes_in(video_path, &interval).await?.into_iter().filter(|k| *k <= t + 0.0005).last();
        if let Some(k) = found {
            return Ok(k.max(0.0));
        }
        if from == 0.0 {
            break;
        }
    }
    Ok(0.0)
}

/// The first keyframe after `t`, if there is one within `limit` seconds.
async fn keyframe_after(video_path: &Path, t: f64, limit: f64) -> Result<Option<f64>, String> {
    for window in [30.0_f64, 120.0, 600.0] {
        let window = window.min(limit);
        let interval = format!("{:.3}%{:.3}", t, t + window);
        if let Some(k) = keyframes_in(video_path, &interval).await?.into_iter().find(|k| *k > t + 0.0005) {
            return Ok(Some(k));
        }
        if window >= limit {
            break;
        }
    }
    Ok(None)
}

/// The first video stream's codec and format, for re-encoding a piece of
/// it to match.
#[derive(Debug, Clone, Default, PartialEq)]
struct VideoParams {
    codec: String,
    profile: Option<String>,
    pix_fmt: Option<String>,
    width: u32,
    height: u32,
    timescale: Option<u32>,
}

async fn video_params(video_path: &Path) -> Result<VideoParams, String> {
    let out = run_ffprobe(&[
        "-select_streams", "v:0",
        "-show_entries", "stream=codec_name,profile,pix_fmt,width,height,time_base",
        "-of", "json",
        path_str(video_path)?,
    ])
    .await?;
    let json: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("ffprobe output: {}", e))?;
    let stream = json["streams"].get(0).ok_or("The video has no picture")?;
    let text = |k: &str| stream[k].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty() && s != "unknown");
    Ok(VideoParams {
        codec: text("codec_name").unwrap_or_default(),
        profile: text("profile"),
        pix_fmt: text("pix_fmt"),
        width: stream["width"].as_u64().unwrap_or(0) as u32,
        height: stream["height"].as_u64().unwrap_or(0) as u32,
        timescale: text("time_base")
            .and_then(|tb| tb.split_once('/').and_then(|(_, den)| den.parse().ok())),
    })
}

/// ffmpeg arguments to re-encode `params`' codec so the result can be
/// joined to a stream copy of the original, and the bitstream filter that
/// puts its parameter sets in-band for joining. None for codecs we can't
/// match.
fn head_encoder(params: &VideoParams) -> Option<(Vec<String>, Option<&'static str>)> {
    let mut args: Vec<String> = Vec::new();
    let bsf = match params.codec.as_str() {
        "h264" => {
            args.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "16"].map(String::from));
            let profile = params.profile.as_deref().map(|p| p.to_lowercase());
            match profile.as_deref() {
                Some("high") => args.extend(["-profile:v".into(), "high".into()]),
                Some("main") => args.extend(["-profile:v".into(), "main".into()]),
                Some("baseline") | Some("constrained baseline") => {
                    args.extend(["-profile:v".into(), "baseline".into()])
                }
                _ => {}
            }
            Some("h264_mp4toannexb")
        }
        "hevc" => {
            args.extend(["-c:v", "libx265", "-preset", "veryfast", "-crf", "18"].map(String::from));
            Some("hevc_mp4toannexb")
        }
        "mpeg4" => {
            args.extend(["-c:v", "mpeg4", "-q:v", "2"].map(String::from));
            None
        }
        _ => return None,
    };
    if let Some(pix) = &params.pix_fmt {
        args.extend(["-pix_fmt".into(), pix.clone()]);
    }
    if params.width > 0 && params.height > 0 {
        args.extend(["-s".into(), format!("{}x{}", params.width, params.height)]);
    }
    Some((args, bsf))
}

fn is_mp4_family(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "mp4" | "m4v" | "mov"))
}

/// Cut exactly at `start`: re-encode the picture from `start` to the next
/// keyframe, stream copy it from there to `end`, join the two (concat
/// demuxer), and copy the sound from `start`. Temporary files go in `work`.
async fn smart_cut(
    video_path: &Path,
    start: f64,
    end: f64,
    output_path: &Path,
    work: &Path,
    on_step: &(dyn Fn(f32, &str) + Send + Sync),
) -> Result<(), String> {
    let params = video_params(video_path).await?;
    let (encode, bsf) = head_encoder(&params).ok_or_else(|| format!("can't re-encode {} video", params.codec))?;
    on_step(0.3, "Finding the next keyframe…");
    let next = keyframe_after(video_path, start, end - start).await?.filter(|k| *k < end);
    let src = path_str(video_path)?;

    on_step(0.4, "Re-encoding the first few seconds…");
    let head = work.join("head.ts");
    // Up to just before the keyframe, so it isn't in both pieces.
    let head_len = next.map_or(end - start, |k| (k - start - 0.0005).max(0.001));
    let mut args: Vec<String> = ["-ss", &format_time(start), "-i", src, "-t", &format_time(head_len), "-map", "0:v:0", "-an"]
        .map(String::from)
        .to_vec();
    args.extend(encode);
    if let Some(b) = bsf {
        args.extend(["-bsf:v".into(), b.into()]);
    }
    args.extend(["-f", "mpegts", "-y", path_str(&head)?].map(String::from));
    run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>()).await?;

    let mut pieces = vec![head];
    if let Some(k) = next {
        on_step(0.6, "Copying the rest…");
        let tail = work.join("tail.ts");
        let mut args: Vec<String> =
            ["-ss", &format_time(k + SEEK_PAD), "-i", src, "-t", &format_time(end - k), "-map", "0:v:0", "-an", "-c:v", "copy"]
                .map(String::from)
                .to_vec();
        if let Some(b) = bsf {
            args.extend(["-bsf:v".into(), b.into()]);
        }
        args.extend(["-f", "mpegts", "-y", path_str(&tail)?].map(String::from));
        run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>()).await?;
        pieces.push(tail);
    }

    on_step(0.85, "Joining…");
    let list = work.join("pieces.txt");
    let listing: String = pieces
        .iter()
        .map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list, listing).map_err(|e| format!("Failed to write {}: {}", list.display(), e))?;
    let duration = format_time(end - start);
    let mut args: Vec<String> = [
        "-f", "concat", "-safe", "0", "-i", path_str(&list)?,
        "-ss", &format_time(start), "-t", &duration, "-i", src,
        "-map", "0:v:0", "-map", "1:a?", "-c", "copy", "-t", &duration,
    ]
    .map(String::from)
    .to_vec();
    if is_mp4_family(output_path) {
        if let Some(ts) = params.timescale.filter(|t| *t > 0) {
            args.extend(["-video_track_timescale".into(), ts.to_string()]);
        }
        if params.codec == "hevc" {
            args.extend(["-tag:v".into(), "hvc1".into()]);
        }
    }
    args.extend(["-y".into(), path_str(output_path)?.to_string()]);
    run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>()).await
}

/// Trim to `start_time`..`end_time` as accurately as stream copying
/// allows, and return where the result actually starts in the original.
///
/// A start within SNAP_SECS after a keyframe is cut at that keyframe
/// (returned). Otherwise the start is cut exactly (see `smart_cut`); if
/// that fails it falls back to the keyframe. `on_step` hears progress
/// (0..1) and what's happening.
pub async fn trim_accurate(
    video_path: &Path,
    start_time: f64,
    end_time: f64,
    output_path: &Path,
    on_step: &(dyn Fn(f32, &str) + Send + Sync),
) -> Result<f64, String> {
    check_times(start_time, end_time)?;
    on_step(0.0, "Finding the nearest keyframe…");
    let keyframe = keyframe_at_or_before(video_path, start_time).await?.min(start_time);
    if start_time - keyframe > SNAP_SECS {
        let work = output_path.with_extension("smartcut");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| format!("Failed to create {}: {}", work.display(), e))?;
        let cut = smart_cut(video_path, start_time, end_time, output_path, &work, on_step).await;
        let _ = std::fs::remove_dir_all(&work);
        match cut {
            Ok(()) => {
                on_step(1.0, "Done");
                return Ok(start_time);
            }
            Err(e) => {
                eprintln!("[trim] Exact cut failed, cutting at the keyframe instead: {}", e);
                let _ = std::fs::remove_file(output_path);
            }
        }
    }
    on_step(0.5, "Copying…");
    // A keyframe at 0 needs no seek at all.
    let seek = if keyframe <= 0.0 { 0.0 } else { keyframe + SEEK_PAD };
    trim_video(video_path, seek, end_time, output_path).await?;
    on_step(1.0, "Done");
    Ok(keyframe)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_times_are_parsed_and_sorted() {
        assert_eq!(parse_times("10.000000,\n0.000000\nN/A\n\n20.5,side\n"), vec![0.0, 10.0, 20.5]);
    }

    #[test]
    fn only_known_codecs_are_re_encoded() {
        let p = |codec: &str| VideoParams { codec: codec.into(), pix_fmt: Some("yuv420p".into()), width: 640, height: 360, ..Default::default() };
        let (args, bsf) = head_encoder(&VideoParams { profile: Some("High".into()), ..p("h264") }).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|w| w == ["-profile:v", "high"]));
        assert!(args.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]));
        assert!(args.windows(2).any(|w| w == ["-s", "640x360"]));
        assert_eq!(bsf, Some("h264_mp4toannexb"));
        assert!(head_encoder(&p("hevc")).is_some());
        assert!(head_encoder(&p("prores")).is_none());
    }

    /// The best encoder the test ffmpeg has: H.264, else MPEG-4 part 2.
    fn test_encoder() -> &'static str {
        let out = crate::tools::std_command("ffmpeg").args(["-hide_banner", "-encoders"]).output().unwrap();
        if String::from_utf8_lossy(&out.stdout).contains("libx264") {
            "libx264"
        } else {
            "mpeg4"
        }
    }

    /// A 40-second test clip (picture and tone) with a keyframe every
    /// `gop` seconds.
    fn clip(dir: &Path, gop: u32) -> std::path::PathBuf {
        let path = dir.join(format!("gop{}.mp4", gop));
        let frames = (gop * 30).to_string();
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=30"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
            .args(["-t", "40", "-c:v", test_encoder(), "-g", &frames, "-keyint_min", &frames, "-sc_threshold", "0"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    fn duration_of(path: &Path) -> f64 {
        probe_duration_blocking(path).expect("ffprobe reads the result")
    }

    /// Keyframe times of a file, from its start.
    fn keyframes(path: &Path) -> Vec<f64> {
        let out = crate::tools::std_command("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-skip_frame", "nokey"])
            .args(["-show_entries", "frame=pts_time", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap();
        parse_times(&String::from_utf8_lossy(&out.stdout))
    }

    /// Every frame decodes without an error.
    fn decodes_cleanly(path: &Path) -> bool {
        let out = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-i"])
            .arg(path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        out.status.success() && out.stderr.is_empty()
    }

    #[tokio::test]
    async fn start_near_a_keyframe_snaps_to_it() {
        if !crate::tools::ffmpeg_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 2);
        let out = tmp.path().join("trimmed.mp4");
        assert_eq!(keyframe_at_or_before(&src, 13.3).await.unwrap(), 12.0);
        let start = trim_accurate(&src, 13.3, 30.0, &out, &|_, _| {}).await.unwrap();
        assert!((start - 12.0).abs() < 0.01, "snapped to the keyframe, got {start}");
        assert!((duration_of(&out) - 18.0).abs() < 0.15, "{}", duration_of(&out));
        assert!(decodes_cleanly(&out));
    }

    #[tokio::test]
    async fn start_far_from_a_keyframe_is_cut_exactly() {
        if !crate::tools::ffmpeg_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 10);
        let out = tmp.path().join("trimmed.mp4");
        assert_eq!(keyframe_at_or_before(&src, 13.37).await.unwrap(), 10.0);
        let start = trim_accurate(&src, 13.37, 35.0, &out, &|_, _| {}).await.unwrap();
        assert_eq!(start, 13.37);
        assert!(decodes_cleanly(&out), "the joined file plays");
        assert!((duration_of(&out) - 21.63).abs() < 0.15, "{}", duration_of(&out));
        // The original's keyframe at 20 s is where it should be if the cut
        // began at 13.37: 6.63 s in, give or take a frame.
        let kf = keyframes(&out);
        assert_eq!(kf[0], 0.0, "starts on a keyframe: {kf:?}");
        assert!(kf.iter().any(|k| (k - (20.0 - 13.37)).abs() < 0.1), "{kf:?}");
        assert!(!tmp.path().join("trimmed.smartcut").exists(), "work files are cleaned up");
    }

    #[tokio::test]
    async fn a_short_cut_inside_one_gop_is_re_encoded_whole() {
        if !crate::tools::ffmpeg_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 10);
        let out = tmp.path().join("trimmed.mov");
        let start = trim_accurate(&src, 23.5, 27.0, &out, &|_, _| {}).await.unwrap();
        assert_eq!(start, 23.5);
        assert!(decodes_cleanly(&out));
        assert!((duration_of(&out) - 3.5).abs() < 0.15, "{}", duration_of(&out));
    }
}
