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
            // Silence before a sound that starts late, so the transcript
            // counts from the file's start like everything else.
            "-af", "aresample=async=1:first_pts=0",
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

// Times. A file's timestamps can start anywhere: camcorder MTS/TS files
// often begin seconds in, and an MP4 with an edit list can have frames
// before its 0. The app counts time the way players and `-ss` do, from the
// file's start (`format=start_time`), called "relative" here. ffprobe
// prints, and `-read_intervals` takes, the file's own ("absolute") times.

/// A file's start (`format=start_time`), 0 if it doesn't say.
async fn file_start(path: &Path) -> Result<f64, String> {
    let out = run_ffprobe(&["-show_entries", "format=start_time", "-of", "csv=p=0", path_str(path)?]).await?;
    Ok(out.trim().trim_end_matches(',').parse::<f64>().ok().filter(|t| t.is_finite()).unwrap_or(0.0))
}

/// How far before a keyframe to seek: a transport stream has no index,
/// so a seek lands near its target, not on a keyframe, and may land just
/// after the one wanted. Landing early is harmless: what comes before the
/// wanted point is dropped.
const SEEK_EARLY: f64 = 1.5;

/// A bitstream filter dropping the packets shown before `t` (absolute).
fn drop_before(t: f64) -> String {
    format!("noise=drop=lt(if(eq(pts\\,nopts)\\,dts\\,pts)*tb\\,{:.6})", t - 0.0005)
}

/// Stream copy from `keyframe` (relative) to `end_time`, both streams
/// starting at the keyframe (None: from the very beginning, with no seek
/// at all, so an MP4's edit list is kept as it is). Timestamps are copied
/// as they are and then all moved by the same amount to start at 0.
async fn copy_cut(video_path: &Path, st: f64, keyframe: Option<f64>, end_time: f64, output_path: &Path) -> Result<(), String> {
    let src = path_str(video_path)?;
    let out = path_str(output_path)?;
    let Some(k) = keyframe else {
        let duration = format_time(end_time);
        run_ffmpeg(&["-i", src, "-t", &duration, "-map", "0:v:0?", "-map", "0:a:0?", "-c", "copy", "-y", out]).await?;
        return non_empty(output_path);
    };
    let seek = (k - SEEK_EARLY).max(0.0);
    let drop = drop_before(k + st);
    run_ffmpeg(&[
        "-ss", &format_time(seek), "-t", &format_time(end_time - seek), "-i", src,
        "-map", "0:v:0?", "-map", "0:a:0?", "-c", "copy", "-copyts", "-bsf:v", &drop, "-bsf:a", &drop,
        "-avoid_negative_ts", "make_zero", "-y", out,
    ])
    .await?;
    non_empty(output_path)
}

/// ffmpeg can exit 0 having written nothing (a seek past the end).
fn non_empty(path: &Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(format!("ffmpeg wrote nothing to {}", path.display())),
    }
}

/// A start this close after a keyframe is cut at the keyframe itself
/// (stream copy, a moment early); further away, the start is cut exactly.
pub const SNAP_SECS: f64 = 2.0;

/// Keyframe times (seconds) of the first video stream within `interval`
/// (ffprobe's -read_intervals syntax, absolute times), in order, absolute.
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

/// The last keyframe at or before `t` (relative; 0 if none is found).
#[cfg(test)]
async fn keyframe_at_or_before(video_path: &Path, t: f64) -> Result<f64, String> {
    let st = file_start(video_path).await?;
    keyframe_before(video_path, st, t).await
}

/// `keyframe_at_or_before` for a file that starts at `st`.
async fn keyframe_before(video_path: &Path, st: f64, t: f64) -> Result<f64, String> {
    for window in [30.0, 120.0, 600.0, f64::INFINITY] {
        let from = (t - window).max(0.0);
        let interval = format!("{:.3}%{:.3}", from + st, t + st + 0.05);
        let found = keyframes_in(video_path, &interval)
            .await?
            .into_iter()
            .map(|k| k - st)
            .filter(|k| *k <= t + 0.0005)
            .last();
        if let Some(k) = found {
            return Ok(k.max(0.0));
        }
        if from == 0.0 {
            break;
        }
    }
    Ok(0.0)
}

/// The first keyframe after `t` (relative), if there is one within
/// `limit` seconds.
async fn keyframe_after(video_path: &Path, st: f64, t: f64, limit: f64) -> Result<Option<f64>, String> {
    for window in [30.0_f64, 120.0, 600.0] {
        let window = window.min(limit);
        let interval = format!("{:.3}%{:.3}", t + st, t + st + window);
        if let Some(k) = keyframes_in(video_path, &interval).await?.into_iter().map(|k| k - st).find(|k| *k > t + 0.0005) {
            return Ok(Some(k));
        }
        if window >= limit {
            break;
        }
    }
    Ok(None)
}

/// One packet's presentation time (absolute) and whether it's a keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Packet {
    pts: f64,
    key: bool,
}

/// Packets of `stream` ("v:0", "a:0") within `interval`, in file order.
async fn packets(path: &Path, stream: &str, interval: &str) -> Result<Vec<Packet>, String> {
    let out = run_ffprobe(&[
        "-select_streams", stream,
        "-show_entries", "packet=pts_time,dts_time,flags",
        "-of", "csv=p=0",
        "-read_intervals", interval,
        path_str(path)?,
    ])
    .await?;
    Ok(parse_packets(&out))
}

/// ffprobe's "pts,dts,flags" lines; a packet with no pts uses its dts.
fn parse_packets(out: &str) -> Vec<Packet> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split(',');
            let (pts, dts, flags) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            let pts = pts.trim().parse::<f64>().ok().or_else(|| dts.trim().parse::<f64>().ok())?;
            pts.is_finite().then_some(Packet { pts, key: flags.starts_with('K') })
        })
        .collect()
}

/// Where a file's picture and sound begin, relative to its start.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Starts {
    /// The first keyframe packet (relative).
    video_key: Option<f64>,
    video: Option<f64>,
    audio: Option<f64>,
}

async fn starts(path: &Path) -> Result<Starts, String> {
    let out = run_ffprobe(&[
        "-show_entries", "format=start_time:stream=codec_type,start_time",
        "-of", "json",
        path_str(path)?,
    ])
    .await?;
    let json: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("ffprobe output: {}", e))?;
    let num = |v: &serde_json::Value| v.as_str().and_then(|s| s.parse::<f64>().ok()).filter(|t| t.is_finite());
    let st = num(&json["format"]["start_time"]).unwrap_or(0.0);
    let first = |kind: &str| {
        json["streams"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["codec_type"] == kind))
            .and_then(|s| num(&s["start_time"]))
            .map(|t| t - st)
    };
    let video_key = packets(path, "v:0", "%+#120").await?.into_iter().find(|p| p.key).map(|p| p.pts - st);
    Ok(Starts { video_key, video: first("video"), audio: first("audio") })
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
    has_audio: bool,
}

async fn video_params(video_path: &Path) -> Result<VideoParams, String> {
    let out = run_ffprobe(&[
        "-show_entries", "stream=codec_type,codec_name,profile,pix_fmt,width,height,time_base",
        "-of", "json",
        path_str(video_path)?,
    ])
    .await?;
    let json: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("ffprobe output: {}", e))?;
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let stream = streams.iter().find(|s| s["codec_type"] == "video").ok_or("The video has no picture")?;
    let text = |k: &str| stream[k].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty() && s != "unknown");
    Ok(VideoParams {
        codec: text("codec_name").unwrap_or_default(),
        profile: text("profile"),
        pix_fmt: text("pix_fmt"),
        width: stream["width"].as_u64().unwrap_or(0) as u32,
        height: stream["height"].as_u64().unwrap_or(0) as u32,
        timescale: text("time_base")
            .and_then(|tb| tb.split_once('/').and_then(|(_, den)| den.parse().ok())),
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}

/// ffmpeg arguments to re-encode `params`' picture so the result can be
/// joined to a stream copy of the original. H.264 only: HEVC and others
/// are cut at a keyframe instead.
fn head_encoder(params: &VideoParams) -> Option<Vec<String>> {
    if params.codec != "h264" {
        return None;
    }
    let mut args: Vec<String> = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "16"].map(String::from).to_vec();
    let profile = params.profile.as_deref().map(|p| p.to_lowercase());
    match profile.as_deref() {
        Some("high") => args.extend(["-profile:v".into(), "high".into()]),
        Some("main") => args.extend(["-profile:v".into(), "main".into()]),
        Some("baseline") | Some("constrained baseline") => args.extend(["-profile:v".into(), "baseline".into()]),
        _ => {}
    }
    if let Some(pix) = &params.pix_fmt {
        args.extend(["-pix_fmt".into(), pix.clone()]);
    }
    if params.width > 0 && params.height > 0 {
        args.extend(["-s".into(), format!("{}x{}", params.width, params.height)]);
    }
    Some(args)
}

/// Puts H.264's parameter sets in-band, for joining.
const ANNEXB: &str = "h264_mp4toannexb";

/// The NAL unit types `trace_headers` printed, in order.
fn nal_types(log: &str) -> Vec<u8> {
    log.lines()
        .filter(|l| l.contains("nal_unit_type"))
        .filter_map(|l| l.rsplit('=').next()?.trim().parse().ok())
        .collect()
}

/// Whether the H.264 keyframe at `k` (relative) is an IDR picture, which
/// nothing after it refers back past. An open-GOP keyframe (a plain I
/// picture with a recovery point) has pictures after it that use the
/// ones before it, which a join would corrupt. Unknown counts as no.
async fn is_idr(video_path: &Path, st: f64, k: f64) -> Result<bool, String> {
    let seek = (k - SEEK_EARLY).max(0.0);
    let filters = format!("{},trace_headers", drop_before(k + st));
    let output = crate::tools::command("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "info", "-ss", &format_time(seek), "-i", path_str(video_path)?])
        .args(["-map", "0:v:0", "-c", "copy", "-copyts", "-bsf:v", &filters, "-frames:v", "1", "-f", "null", "-"])
        .output()
        .await
        .map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;
    if !output.status.success() {
        return Err(format!("ffmpeg error: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    // Parameter sets and SEI come first; the first slice decides.
    Ok(nal_types(&String::from_utf8_lossy(&output.stderr)).into_iter().find(|t| (1..=5).contains(t)) == Some(5))
}

fn is_mp4_family(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "mp4" | "m4v" | "mov"))
}

/// Whether `path` decodes without a single error, in full or (`limit`)
/// its first seconds.
async fn decodes_cleanly(path: &Path, limit: Option<f64>) -> Result<(), String> {
    let mut cmd = crate::tools::command("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin", "-v", "error", "-i", path_str(path)?]);
    if let Some(t) = limit {
        cmd.args(["-t", &format_time(t)]);
    }
    let output = cmd.args(["-f", "null", "-"]).output().await.map_err(|e| crate::tools::spawn_error("ffmpeg", e))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() || !stderr.trim().is_empty() {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        return Err(format!("{} doesn't decode cleanly: {}", name, stderr.trim()));
    }
    Ok(())
}

/// Cut exactly at `start` (relative), for H.264 whose next keyframe is an
/// IDR picture (else Err, and the caller cuts at the keyframe): re-encode
/// the picture from `start` to the next keyframe, stream copy it from there to `end`, join the two
/// (concat demuxer), and copy the sound from `start`. Each piece is checked
/// before the join, and the join itself after. Returns the original's time
/// that the result's 0 shows, measured through its sound. Temporary files
/// go in `work`.
async fn smart_cut(
    video_path: &Path,
    st: f64,
    keyframe: f64,
    start: f64,
    end: f64,
    output_path: &Path,
    work: &Path,
    on_step: &(dyn Fn(f32, &str) + Send + Sync),
) -> Result<f64, String> {
    let params = video_params(video_path).await?;
    let encode = head_encoder(&params).ok_or_else(|| format!("{} video is cut at a keyframe", params.codec))?;
    on_step(0.3, "Finding the next keyframe…");
    let next = keyframe_after(video_path, st, start, end - start).await?.filter(|k| *k < end);
    if let Some(k) = next {
        if !is_idr(video_path, st, k).await? {
            return Err(format!("the keyframe at {:.2}s isn't an IDR picture (open GOP)", k));
        }
    }
    let src = path_str(video_path)?;

    on_step(0.4, "Re-encoding the first few seconds…");
    let head = work.join("head.ts");
    // Decoded from before the keyframe at or before `start` (see
    // SEEK_EARLY), and kept from `start`, up to just before the next
    // keyframe so it isn't in both pieces.
    let head_len = next.map_or(end - start, |k| (k - start - 0.0005).max(0.001));
    let pre = (keyframe - SEEK_EARLY).max(0.0);
    let mut args: Vec<String> = [
        "-ss", &format_time(pre), "-i", src, "-ss", &format_time(start - pre), "-t", &format_time(head_len),
        "-map", "0:v:0", "-an",
    ]
    .map(String::from)
    .to_vec();
    args.extend(encode);
    args.extend(["-bsf:v", ANNEXB, "-f", "mpegts", "-y", path_str(&head)?].map(String::from));
    run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    non_empty(&head)?;
    decodes_cleanly(&head, None).await?;

    let mut pieces = vec![head];
    if let Some(k) = next {
        on_step(0.6, "Copying the rest…");
        let tail = work.join("tail.ts");
        let seek = (k - SEEK_EARLY).max(0.0);
        let filters = format!("{},{}", drop_before(k + st), ANNEXB);
        run_ffmpeg(&[
            "-ss", &format_time(seek), "-t", &format_time(end - seek), "-i", src,
            "-map", "0:v:0", "-an", "-c:v", "copy", "-copyts", "-bsf:v", &filters, "-f", "mpegts", "-y", path_str(&tail)?,
        ])
        .await?;
        non_empty(&tail)?;
        decodes_cleanly(&tail, Some(3.0)).await?;
        pieces.push(tail);
    }

    // The original's time of the head's first picture: the first frame
    // at or after `start` (the re-encode drops the ones before it).
    let near = format!("{:.3}%{:.3}", start + st - 1.0, start + st + 1.0);
    let v0 = packets(video_path, "v:0", &near)
        .await?
        .into_iter()
        .map(|p| p.pts - st)
        .filter(|t| *t >= start - 0.0005)
        .min_by(|a, b| a.total_cmp(b))
        .unwrap_or(start);

    // The sound, copied with its own timestamps kept, so where its first
    // packet sits in the original is known exactly. The seek lands a
    // little before `start`; the packets before it are dropped.
    let sound = if params.has_audio {
        let audio = work.join("audio.nut");
        let drop = drop_before(start + st);
        let seek = (start - SEEK_EARLY).max(0.0);
        run_ffmpeg(&[
            "-ss", &format_time(seek), "-t", &format_time(end - seek), "-i", src,
            "-map", "0:a:0", "-vn", "-c", "copy", "-copyts", "-bsf:a", &drop, "-f", "nut", "-y", path_str(&audio)?,
        ])
        .await?;
        non_empty(&audio)?;
        let first = packets(&audio, "a:0", "%+#1").await?.first().map(|p| p.pts - st).ok_or("the sound piece is empty")?;
        Some((audio, first))
    } else {
        None
    };

    on_step(0.85, "Joining…");
    let list = work.join("pieces.txt");
    let listing: String = pieces
        .iter()
        .map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list, listing).map_err(|e| format!("Failed to write {}: {}", list.display(), e))?;
    let duration = format_time(end - start);
    let mut args: Vec<String> = ["-f", "concat", "-safe", "0", "-i", path_str(&list)?].map(String::from).to_vec();
    if let Some((audio, a0)) = &sound {
        // The concat input starts at 0 with the head's first picture (v0);
        // the sound goes where it is relative to that.
        args.extend(["-itsoffset".into(), format!("{:.6}", a0 - v0), "-i".into(), path_str(audio)?.to_string()]);
    }
    args.extend(["-map", "0:v:0"].map(String::from));
    if sound.is_some() {
        args.extend(["-map", "1:a:0"].map(String::from));
    }
    args.extend(["-c", "copy", "-t", &duration, "-avoid_negative_ts", "make_zero"].map(String::from));
    if is_mp4_family(output_path) {
        if let Some(ts) = params.timescale.filter(|t| *t > 0) {
            args.extend(["-video_track_timescale".into(), ts.to_string()]);
        }
    }
    args.extend(["-y".into(), path_str(output_path)?.to_string()]);
    run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    non_empty(output_path)?;
    // The join: the head, the switch to the copied part, and a little after.
    decodes_cleanly(output_path, Some(head_len + 3.0)).await?;

    let out = starts(output_path).await?;
    let by_video = out.video.map(|v| v0 - v);
    let by_audio = match (&sound, out.audio) {
        (Some((_, a0)), Some(a)) => Some(a0 - a),
        _ => None,
    };
    if let (Some(v), Some(a)) = (by_video, by_audio) {
        if (v - a).abs() > 0.05 {
            return Err(format!("the joined picture and sound are {:.3}s apart", v - a));
        }
    }
    by_audio.or(by_video).ok_or_else(|| "the joined file has no streams".into())
}

/// How a trim was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutMethod {
    /// Stream copy from the keyframe at or before the start.
    Keyframe,
    /// The start re-encoded up to the next keyframe, stream copy after
    /// (H.264 with closed GOPs only).
    Smart,
}

/// A finished trim.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    /// The original's time (relative to its start, as the transcript
    /// counts) that the trimmed file's 0 shows, measured through the sound,
    /// since captions and chapters follow the sound.
    pub start: f64,
    pub method: CutMethod,
}

/// Trim to `start_time`..`end_time` (relative) as accurately as stream
/// copying allows, and say where the result actually starts.
///
/// A start within SNAP_SECS after a keyframe is cut at that keyframe.
/// Otherwise the start is cut exactly (see `smart_cut`); if that fails it
/// falls back to the keyframe. `on_step` hears progress (0..1) and what's
/// happening.
pub async fn trim_accurate(
    video_path: &Path,
    start_time: f64,
    end_time: f64,
    output_path: &Path,
    on_step: &(dyn Fn(f32, &str) + Send + Sync),
) -> Result<Cut, String> {
    check_times(start_time, end_time)?;
    on_step(0.0, "Finding the nearest keyframe…");
    let st = file_start(video_path).await?;
    let keyframe = keyframe_before(video_path, st, start_time).await?.min(start_time);
    if start_time - keyframe > SNAP_SECS {
        let work = output_path.with_extension("smartcut");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| format!("Failed to create {}: {}", work.display(), e))?;
        let cut = smart_cut(video_path, st, keyframe, start_time, end_time, output_path, &work, on_step).await;
        let _ = std::fs::remove_dir_all(&work);
        match cut {
            Ok(start) => {
                on_step(1.0, "Done");
                return Ok(Cut { start, method: CutMethod::Smart });
            }
            Err(e) => {
                eprintln!("[trim] Exact cut failed, cutting at the keyframe instead: {}", e);
                let _ = std::fs::remove_file(output_path);
            }
        }
    }
    on_step(0.5, "Copying…");
    // A keyframe at 0 needs no seek at all.
    let from = (keyframe > 0.0).then_some(keyframe);
    copy_cut(video_path, st, from, end_time, output_path).await?;
    // A stream copy moves every packet by the same amount, so the keyframe
    // it starts on says by how much.
    let first_key = match from {
        Some(k) => k,
        None => packets(video_path, "v:0", "%+#120").await?.into_iter().find(|p| p.key).map_or(0.0, |p| p.pts - st),
    };
    let out = starts(output_path).await?;
    let start = match out.video_key {
        Some(k) => first_key - k,
        None => from.unwrap_or(0.0) - out.audio.unwrap_or(0.0),
    }
    .max(0.0);
    on_step(1.0, "Done");
    Ok(Cut { start, method: CutMethod::Keyframe })
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
    fn only_h264_is_re_encoded() {
        let p = |codec: &str| VideoParams { codec: codec.into(), pix_fmt: Some("yuv420p".into()), width: 640, height: 360, ..Default::default() };
        let args = head_encoder(&VideoParams { profile: Some("High".into()), ..p("h264") }).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|w| w == ["-profile:v", "high"]));
        assert!(args.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]));
        assert!(args.windows(2).any(|w| w == ["-s", "640x360"]));
        assert!(head_encoder(&p("hevc")).is_none());
        assert!(head_encoder(&p("mpeg4")).is_none());
        assert!(head_encoder(&p("prores")).is_none());
    }

    #[test]
    fn nal_unit_types_are_read_from_the_trace() {
        let log = "[trace_headers @ 0x1] 3           nal_unit_type                                           00111 = 7\n\
                   [trace_headers @ 0x1] Sequence Parameter Set\n\
                   [trace_headers @ 0x1] 3           nal_unit_type                                           00101 = 5\n";
        assert_eq!(nal_types(log), vec![7, 5]);
    }

    fn has_encoder(name: &str) -> bool {
        let out = crate::tools::std_command("ffmpeg").args(["-hide_banner", "-encoders"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().any(|w| w == name)
    }

    /// ffmpeg and libx264 are there (see `ffmpeg_for_tests`); without
    /// libx264 the test is skipped, or fails under REQUIRE_FFMPEG=1.
    fn x264_for_tests() -> bool {
        if !crate::tools::ffmpeg_for_tests() {
            return false;
        }
        if has_encoder("libx264") {
            return true;
        }
        if std::env::var("REQUIRE_FFMPEG").is_ok_and(|v| v == "1") {
            panic!("ffmpeg has no libx264 (REQUIRE_FFMPEG=1)");
        }
        eprintln!("skipping: ffmpeg has no libx264");
        false
    }

    /// The sound of every test clip: a 30 ms beep at k + 0.02·k² seconds
    /// (k = 0, 1, 2, …), silence between. The gaps all differ, so two
    /// beeps say exactly where a piece of it came from.
    const BEEPS: &str = "aevalsrc=st(0\\,floor((sqrt(1+0.08*t)-1)/0.04))\\;if(lt(t-ld(0)-0.02*ld(0)*ld(0)\\,0.03)\\,sin(2*PI*1000*t)\\,0):s=48000";

    /// A 40-second test clip `name` (test pattern and BEEPS): `video` are
    /// the encoder arguments, `out` any output options before the name.
    fn fixture(dir: &Path, name: &str, video: &[&str], out: &[&str]) -> std::path::PathBuf {
        let path = dir.join(name);
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=30"])
            .args(["-f", "lavfi", "-i", BEEPS])
            .args(["-t", "40"])
            .args(video)
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .args(out)
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "made {name}");
        path
    }

    /// x264 with a keyframe every `gop` seconds (closed GOPs).
    fn x264(gop: u32) -> Vec<String> {
        let frames = (gop * 30).to_string();
        ["-c:v", "libx264", "-g", &frames, "-keyint_min", &frames, "-sc_threshold", "0"].map(String::from).to_vec()
    }

    /// A 40-second clip with a keyframe every `gop` seconds.
    fn clip(dir: &Path, gop: u32) -> std::path::PathBuf {
        let args = x264(gop);
        fixture(dir, &format!("gop{}.mp4", gop), &args.iter().map(String::as_str).collect::<Vec<_>>(), &[])
    }

    /// When each beep starts, from the file's start (as a player counts).
    fn onsets(path: &Path) -> Vec<f64> {
        let out = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-nostats", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-af", "silencedetect=n=-30dB:d=0.3", "-f", "null", "-"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter_map(|l| l.split("silence_end: ").nth(1)?.split_whitespace().next()?.parse().ok())
            .collect()
    }

    /// The time in `src` that `out`'s 0 really is, from where its beeps
    /// fall in `src`: the one shift that lines up its first beeps with the
    /// original's.
    fn actual_start(src: &Path, out: &Path) -> f64 {
        let (src, out) = (onsets(src), onsets(out));
        assert!(out.len() >= 3, "beeps in the cut: {out:?}");
        let fits = |shift: f64| out.iter().take(4).all(|o| src.iter().any(|s| (s - (o + shift)).abs() < 0.01));
        let shifts: Vec<f64> = src.iter().map(|s| s - out[0]).filter(|s| fits(*s)).collect();
        assert_eq!(shifts.len(), 1, "one way to line up {out:?} with {src:?}");
        shifts[0]
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

    /// Trim `src` and check the result: it decodes end to end, and the
    /// start it reports is where it really starts (±0.05 s), going by the
    /// sound. Returns the cut.
    async fn trim_checked(src: &Path, start: f64, end: f64, out: &Path) -> Cut {
        let cut = trim_accurate(src, start, end, out, &|_, _| {}).await.unwrap();
        assert!(decodes_cleanly(out), "{} plays without errors", out.display());
        let actual = actual_start(src, out);
        println!("{}: cut at {start}, reported {:.3} ({:?}), actual {actual:.3}", src.display(), cut.start, cut.method);
        assert!((cut.start - actual).abs() <= 0.05, "reported {} but it starts at {actual}", cut.start);
        cut
    }

    #[tokio::test]
    async fn start_near_a_keyframe_snaps_to_it() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 2);
        let out = tmp.path().join("trimmed.mp4");
        assert_eq!(keyframe_at_or_before(&src, 13.3).await.unwrap(), 12.0);
        let cut = trim_checked(&src, 13.3, 30.0, &out).await;
        assert_eq!(cut.method, CutMethod::Keyframe);
        assert!((cut.start - 12.0).abs() < 0.1, "snapped to the keyframe, got {}", cut.start);
        assert!((duration_of(&out) - 18.0).abs() < 0.15, "{}", duration_of(&out));
    }

    #[tokio::test]
    async fn start_far_from_a_keyframe_is_cut_exactly() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 10);
        let out = tmp.path().join("trimmed.mp4");
        assert_eq!(keyframe_at_or_before(&src, 13.37).await.unwrap(), 10.0);
        let cut = trim_checked(&src, 13.37, 35.0, &out).await;
        assert_eq!(cut.method, CutMethod::Smart, "closed-GOP H.264 is cut exactly");
        assert!((cut.start - 13.37).abs() < 0.05, "{}", cut.start);
        assert!((duration_of(&out) - 21.63).abs() < 0.15, "{}", duration_of(&out));
        // The original's keyframe at 20 s is where it should be if the cut
        // began at 13.37: 6.63 s in, give or take a frame.
        let kf = keyframes(&out);
        assert!(kf[0] < 0.1, "starts on a keyframe: {kf:?}");
        assert!(kf.iter().any(|k| (k - (20.0 - 13.37)).abs() < 0.1), "{kf:?}");
        assert!(!tmp.path().join("trimmed.smartcut").exists(), "work files are cleaned up");
    }

    #[tokio::test]
    async fn a_short_cut_inside_one_gop_is_re_encoded_whole() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = clip(tmp.path(), 10);
        let out = tmp.path().join("trimmed.mov");
        let cut = trim_checked(&src, 23.5, 30.0, &out).await;
        assert!((cut.start - 23.5).abs() < 0.05, "{}", cut.start);
        assert!((duration_of(&out) - 6.5).abs() < 0.15, "{}", duration_of(&out));
    }

    /// Camcorder MTS/TS files start seconds in: times are counted from the
    /// file's start, not its timestamps.
    #[tokio::test]
    async fn a_transport_stream_that_starts_late_is_cut_where_asked() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let args = x264(10);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let src = fixture(tmp.path(), "camera.ts", &args, &["-output_ts_offset", "6.4", "-f", "mpegts"]);
        assert!(file_start(&src).await.unwrap() > 6.0, "the fixture starts late");
        // Its keyframes are at 0, 10, 20 and 30 s from its start.
        assert!((keyframe_at_or_before(&src, 13.37).await.unwrap() - 10.0).abs() < 0.05);

        let exact = trim_checked(&src, 13.37, 30.0, &tmp.path().join("exact.ts")).await;
        assert_eq!(exact.method, CutMethod::Smart);
        assert!((exact.start - 13.37).abs() < 0.05, "{}", exact.start);
        let snapped = trim_checked(&src, 11.0, 30.0, &tmp.path().join("snapped.ts")).await;
        assert!((snapped.start - 10.0).abs() < 0.1, "{}", snapped.start);
        let from_0 = trim_checked(&src, 0.5, 20.0, &tmp.path().join("from0.ts")).await;
        assert!(from_0.start.abs() < 0.05, "{}", from_0.start);
    }

    /// An MP4 cut by stream copy has an edit list: frames before its 0.
    #[tokio::test]
    async fn an_mp4_with_an_edit_list_is_cut_where_asked() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let whole = clip(tmp.path(), 5);
        let src = tmp.path().join("edited.mp4");
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-ss", "3.3", "-i"])
            .arg(&whole)
            .args(["-c", "copy"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success());
        // Keyframes at 1.7, 6.7, 11.7, …; the one at -3.3 is hidden.
        let from_0 = trim_checked(&src, 0.5, 20.0, &tmp.path().join("from0.mp4")).await;
        assert!(from_0.start.abs() < 0.05, "{}", from_0.start);
        let snapped = trim_checked(&src, 12.5, 30.0, &tmp.path().join("snapped.mp4")).await;
        assert!((snapped.start - 11.7).abs() < 0.1, "{}", snapped.start);
        let exact = trim_checked(&src, 10.0, 30.0, &tmp.path().join("exact.mp4")).await;
        assert_eq!(exact.method, CutMethod::Smart);
        assert!((exact.start - 10.0).abs() < 0.05, "{}", exact.start);
    }

    /// Open-GOP H.264: the keyframe after the start isn't an IDR picture,
    /// so the cut snaps to the keyframe before it.
    #[tokio::test]
    async fn open_gop_h264_snaps_to_a_keyframe() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut args = x264(10);
        args.extend(["-x264-params", "open-gop=1"].map(String::from));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let src = fixture(tmp.path(), "open.mp4", &args, &[]);
        let st = file_start(&src).await.unwrap();
        assert!(!is_idr(&src, st, 20.0).await.unwrap(), "an open-GOP keyframe");
        let cut = trim_checked(&src, 13.37, 35.0, &tmp.path().join("trimmed.mp4")).await;
        assert_eq!(cut.method, CutMethod::Keyframe);
        assert!((cut.start - 10.0).abs() < 0.1, "{}", cut.start);

        let closed = clip(tmp.path(), 10);
        assert!(is_idr(&closed, 0.0, 20.0).await.unwrap(), "a closed-GOP keyframe");
    }

    /// HEVC is always cut at a keyframe.
    #[tokio::test]
    async fn hevc_snaps_to_a_keyframe() {
        if !x264_for_tests() {
            return;
        }
        if !has_encoder("libx265") {
            eprintln!("skipping: ffmpeg has no libx265");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let args = ["-c:v", "libx265", "-x265-params", "keyint=300:min-keyint=300:scenecut=0:log-level=error"];
        let src = fixture(tmp.path(), "hevc.mp4", &args, &[]);
        let cut = trim_checked(&src, 13.37, 35.0, &tmp.path().join("trimmed.mp4")).await;
        assert_eq!(cut.method, CutMethod::Keyframe);
        assert!((cut.start - 10.0).abs() < 0.1, "{}", cut.start);
    }

    /// The transcript counts from the file's start even when the sound
    /// starts later than the picture.
    #[tokio::test]
    async fn extracted_audio_keeps_the_original_timing() {
        if !x264_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("late.mp4");
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30"])
            .args(["-itsoffset", "0.5", "-f", "lavfi", "-i", BEEPS, "-t", "20", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success());
        let audio = tmp.path().join("audio.ogg");
        extract_audio(&src, &audio).await.unwrap();
        let (a, b) = (onsets(&src), onsets(&audio));
        assert!(a.len() >= 3);
        for (x, y) in a.iter().zip(b.iter().skip_while(|t| **t < a[0] - 0.1)) {
            assert!((x - y).abs() < 0.01, "{a:?} vs {b:?}");
        }
    }
}
