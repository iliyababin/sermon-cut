mod color;
mod crop;
mod decode;
mod fit;
mod frames;
mod grading;
mod scoring;
mod yolo;

pub use crop::CropRect;
pub use fit::fit_for_youtube;

use image::RgbImage;
use scoring::{score_detection, DetectionStatus, ScoredDetection};
use std::path::{Path, PathBuf};

const NUM_CANDIDATES: u32 = 100;

/// Resolve the bundled ONNX model path.
/// In dev mode it's at `src-tauri/resources/yolov8n-pose.onnx`.
/// In production, Tauri bundles it via `resources/*`.
fn find_model_path() -> Result<PathBuf, String> {
    // Try the Cargo manifest dir (dev builds)
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/yolov8n-pose.onnx");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    // Try next to the executable (bundled)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join("resources/yolov8n-pose.onnx");
            if bundled.exists() {
                return Ok(bundled);
            }
            // macOS bundle
            let mac_bundled = dir.join("../Resources/resources/yolov8n-pose.onnx");
            if mac_bundled.exists() {
                return Ok(mac_bundled);
            }
        }
    }

    Err("Could not find yolov8n-pose.onnx model file".to_string())
}

/// Load a video frame from disk as an upright sRGB `RgbImage`.
fn load_image(path: &Path) -> Result<RgbImage, String> {
    let d = decode::decode_file(path)?;
    Ok(decode::to_srgb_rgb8(&d.image, d.icc.as_deref(), decode::Background::Black))
}

/// Save an `RgbImage` as JPEG with quality 92.
fn save_jpeg(img: &RgbImage, path: &Path) -> Result<(), String> {
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92);
    img.write_with_encoder(encoder)
        .map_err(|e| format!("JPEG encode error: {}", e))?;
    std::fs::write(path, &buf).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

/// Save image bytes (a template render from the webview) as a JPEG within
/// YouTube's thumbnail limits.
pub fn save_for_youtube(bytes: &[u8], output_path: &Path) -> Result<(), String> {
    let jpeg = fit_for_youtube(bytes)?;
    std::fs::write(output_path, &jpeg).map_err(|e| format!("Failed to write {}: {}", output_path.display(), e))
}

/// Core pipeline: extract frames → detect → score → pick best → crop →
/// grade → save. No logo: the review screen's template draws it. Frame
/// extraction is async (ffmpeg); the rest is CPU work and runs on a
/// blocking thread.
async fn run_pipeline(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    output_dir: &Path,
    output_stem: &str,
    count: u32,
) -> Result<Vec<String>, String> {
    let model_path = find_model_path()?;

    // Candidate frames are only needed during this run, so keep them in a
    // temp dir that's deleted when we return.
    let temp_dir_holder = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let frame_dir = temp_dir_holder.path().to_path_buf();

    let frame_data =
        frames::extract_candidate_frames(video_path, start_time, end_time, NUM_CANDIDATES, &frame_dir).await?;

    let output_dir = output_dir.to_path_buf();
    let output_stem = output_stem.to_string();
    let result = tokio::task::spawn_blocking(move || {
        pick_and_save(&model_path, &frame_data, &output_dir, &output_stem, count)
    })
    .await
    .map_err(|e| format!("Thumbnail task failed: {}", e))?;
    drop(temp_dir_holder);
    result
}

/// The blocking part of the pipeline, over already-extracted frames.
fn pick_and_save(
    model_path: &Path,
    frame_data: &[(f64, PathBuf)],
    output_dir: &Path,
    output_stem: &str,
    count: u32,
) -> Result<Vec<String>, String> {
    if frame_data.is_empty() {
        return Err("No frames could be extracted for thumbnails".into());
    }

    // Run YOLO + scoring on each frame
    let mut all_scored: Vec<(PathBuf, ScoredDetection)> = Vec::new();

    for (timestamp, frame_path) in frame_data {
        let img = match load_image(frame_path) {
            Ok(i) => i,
            Err(_) => continue,
        };

        let detections = match yolo::detect(model_path, &img) {
            Ok(d) => d,
            Err(e) => {
                println!("[thumbnail] YOLO error on frame at {:.0}s: {}", timestamp, e);
                continue;
            }
        };

        for det in &detections {
            let scored = score_detection(det, &img);
            all_scored.push((frame_path.clone(), scored));
        }
    }

    let count_of = |status| all_scored.iter().filter(|(_, s)| s.status == status).count();
    println!(
        "[thumbnail] Detection summary: {} valid, {} not facing, {} sitting",
        count_of(DetectionStatus::Valid),
        count_of(DetectionStatus::NotFacing),
        count_of(DetectionStatus::Sitting)
    );

    // Filter to valid detections only, sort by score desc
    let mut valid: Vec<(PathBuf, ScoredDetection)> = all_scored
        .into_iter()
        .filter(|(_, s)| s.status == DetectionStatus::Valid)
        .collect();
    valid.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));

    // Select top-N from distinct frames
    let mut selected: Vec<(PathBuf, ScoredDetection)> = Vec::new();
    let mut used_frames = std::collections::HashSet::new();

    for (path, scored) in valid {
        if selected.len() >= count as usize {
            break;
        }
        if used_frames.insert(path.clone()) {
            selected.push((path, scored));
        }
    }

    // Fallback: no valid detections
    if selected.is_empty() {
        println!("[thumbnail] No person detected, using center crop of middle frame");
        let mid = &frame_data[frame_data.len() / 2];
        let img = load_image(&mid.1)?;
        let fallback_crop = crop::calculate_fallback_crop(img.width(), img.height());

        let cropped = crop_image(&img, &fallback_crop);
        // Crop-only variant for the template overlays (see the main loop).
        if count > 1 {
            let raw_path = output_dir.join(format!("{}thumbnail_option_1_raw.jpg", output_stem));
            save_jpeg(&cropped, &raw_path)?;
        }
        let graded = shrunk_and_graded(&cropped);

        let out_path = output_dir.join(format!("{}thumbnail_option_1.jpg", output_stem));
        save_jpeg(&graded, &out_path)?;

        let abs = out_path
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize: {}", e))?;
        return Ok(vec![abs.to_string_lossy().to_string()]);
    }

    // Process each selected detection
    let mut output_paths = Vec::new();

    for (idx, (frame_path, scored)) in selected.iter().enumerate() {
        let img = load_image(frame_path)?;
        let det = &scored.detection;

        let person_crop = crop::calculate_person_crop(
            det.x as f64,
            det.y as f64,
            det.w as f64,
            det.h as f64,
            img.width(),
            img.height(),
        );

        // Cropped once. The plain crop, at full size for zooming in the
        // editor, is what the template draws over (multi-thumbnail mode).
        let cropped = crop_image(&img, &person_crop);
        if count > 1 {
            let raw_path = output_dir.join(format!("{}thumbnail_option_{}_raw.jpg", output_stem, idx + 1));
            save_jpeg(&cropped, &raw_path)?;
        }
        let result = shrunk_and_graded(&cropped);

        let out_name = if count > 1 {
            format!("{}thumbnail_option_{}.jpg", output_stem, idx + 1)
        } else {
            format!("{}thumbnail.jpg", output_stem)
        };
        let out_path = output_dir.join(&out_name);
        save_jpeg(&result, &out_path)?;

        let abs = out_path
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize: {}", e))?;
        output_paths.push(abs.to_string_lossy().to_string());

        println!(
            "[thumbnail] Generated thumbnail {}/{}: score={:.3} {}",
            idx + 1,
            selected.len(),
            scored.score,
            scored.details
        );
    }

    Ok(output_paths)
}

/// `img` scaled down to YouTube's 1280×720, then colour graded: grading is
/// per pixel, so it's done on the smaller image.
fn shrunk_and_graded(img: &RgbImage) -> RgbImage {
    let mut out = fit::shrink(img).into_owned();
    grading::apply_color_grading(&mut out);
    out
}

/// Crop an `RgbImage` according to a `CropRect`.
fn crop_image(img: &RgbImage, rect: &CropRect) -> RgbImage {
    let x = rect.x.max(0) as u32;
    let y = rect.y.max(0) as u32;
    let w = (rect.width as u32).min(img.width().saturating_sub(x));
    let h = (rect.height as u32).min(img.height().saturating_sub(y));

    if w == 0 || h == 0 {
        return img.clone();
    }

    image::imageops::crop_imm(img, x, y, w, h).to_image()
}

// =====================================================================
// Public API — same signatures as the old thumbnail.rs
// =====================================================================

/// Generate thumbnail options for review, written straight into `output_dir`
/// as thumbnail_option_N.jpg (plus _raw and _debug variants).
pub async fn generate_thumbnail_options(
    video_path: &Path,
    start_time: f64,
    end_time: f64,
    title: &str,
    output_dir: &Path,
    count: u32,
) -> Result<Vec<String>, String> {
    if !video_path.exists() {
        return Err(format!("Video file not found: {}", video_path.display()));
    }
    let video_path = video_path.to_str().ok_or("video path contains invalid UTF-8")?;

    println!(
        "[thumbnail] Generating {} thumbnail options for: {}",
        count, title
    );

    run_pipeline(
        video_path,
        start_time,
        end_time,
        output_dir,
        "",
        count,
    )
    .await
}

/// Process a custom image as a thumbnail: crop, and optionally colour grade.
/// No logo: the template draws it. CPU work, so on a blocking thread.
pub async fn process_custom_thumbnail(
    source_path: impl AsRef<Path>,
    output_dir: &Path,
    crop_rect: &CropRect,
    apply_color_grading_flag: bool,
) -> Result<String, String> {
    let (source, output_dir, crop_rect) = (source_path.as_ref().to_path_buf(), output_dir.to_path_buf(), crop_rect.clone());
    tokio::task::spawn_blocking(move || custom_thumbnail(&source, &output_dir, &crop_rect, apply_color_grading_flag))
        .await
        .map_err(|e| format!("Thumbnail task failed: {}", e))?
}

fn custom_thumbnail(source: &Path, output_dir: &Path, crop_rect: &CropRect, grade: bool) -> Result<String, String> {
    if !source.exists() {
        return Err(format!("Source image not found: {}", source.display()));
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let output_path = output_dir.join(format!("custom_thumbnail_{}.jpg", timestamp));

    println!("[thumbnail] Processing custom thumbnail from: {}", source.display());

    // Upright, as the editor showed it: the crop is in that space.
    let decoded = decode::decode_file(source)?;
    let img = &decoded.image;

    // Clamp the crop to the image.
    let (w, h) = (img.width() as i32, img.height() as i32);
    let x = crop_rect.x.clamp(0, (w - 1).max(0));
    let y = crop_rect.y.clamp(0, (h - 1).max(0));
    let clamped = CropRect { x, y, width: crop_rect.width.clamp(1, (w - x).max(1)), height: crop_rect.height.clamp(1, (h - y).max(1)) };
    println!(
        "[thumbnail] Cropping: x={}, y={}, w={}, h={}",
        clamped.x, clamped.y, clamped.width, clamped.height
    );

    // Cropped once, scaled down to YouTube's size (a phone photo crop is far
    // bigger), then sRGB with a transparent logo or graphic on white, then
    // graded: the per-pixel work is done on the small image.
    let cropped = img.crop_imm(clamped.x as u32, clamped.y as u32, clamped.width as u32, clamped.height as u32);
    let shrunk = fit::shrink_dynamic(&cropped);
    let mut result = decode::to_srgb_rgb8(&shrunk, decoded.icc.as_deref(), decode::Background::White);
    if grade {
        grading::apply_color_grading(&mut result);
    }

    let jpeg = fit::encode_for_youtube(&result)?;
    std::fs::write(&output_path, &jpeg).map_err(|e| format!("Failed to write {}: {}", output_path.display(), e))?;
    println!("[thumbnail] Saved custom thumbnail to: {}", output_path.display());

    let absolute_path = output_path
        .canonicalize()
        .map_err(|e| format!("Failed to get absolute path: {}", e))?;
    Ok(absolute_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::decode::tests::{is_close, jpeg_with_orientation, red_over_blue};
    use super::*;

    fn crop(x: i32, y: i32, width: i32, height: i32) -> CropRect {
        CropRect { x, y, width, height }
    }

    #[tokio::test]
    async fn a_rotated_photo_is_cropped_where_the_editor_showed_it() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("photo.jpg");
        // Shown turned 90° clockwise: 32 wide, 64 tall, blue on the left.
        std::fs::write(&src, jpeg_with_orientation(&red_over_blue(), 6)).unwrap();
        let out = process_custom_thumbnail(src.to_str().unwrap(), tmp.path(), &crop(0, 0, 12, 64), false).await.unwrap();
        let img = image::open(&out).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (12, 64));
        for (x, y) in [(2, 2), (6, 32), (10, 60)] {
            assert!(is_close(*img.get_pixel(x, y), [0, 0, 255]), "({x},{y}) {:?}", img.get_pixel(x, y));
        }
    }

    #[tokio::test]
    async fn a_transparent_logo_is_cropped_onto_white() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("logo.png");
        let mut logo = image::RgbaImage::from_pixel(32, 18, image::Rgba([0, 0, 0, 0]));
        for x in 8..24 {
            for y in 4..14 {
                logo.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        logo.save(&src).unwrap();
        let out = process_custom_thumbnail(src.to_str().unwrap(), tmp.path(), &crop(0, 0, 32, 18), false).await.unwrap();
        let img = image::open(&out).unwrap().to_rgb8();
        assert!(is_close(*img.get_pixel(1, 1), [255, 255, 255]), "{:?}", img.get_pixel(1, 1));
        assert!(is_close(*img.get_pixel(16, 9), [0, 0, 0]), "{:?}", img.get_pixel(16, 9));
    }

    #[tokio::test]
    async fn a_short_clip_still_gets_a_thumbnail() {
        if !crate::tools::ffmpeg_for_tests() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("clip.mp4");
        let status = crate::tools::std_command("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=25", "-t", "8"])
            .args(["-c:v", "mpeg4"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(status.success());
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let options = generate_thumbnail_options(&video, 0.0, 8.0, "T", &out, 10).await.unwrap();
        assert_eq!(options.len(), 1, "one frame from a short clip");
        let option = image::open(&options[0]).unwrap();
        assert!(option.width() <= 1280 && option.height() <= 720);
        // The plain crop stays full size, for zooming in the editor.
        let raw = image::open(out.join("thumbnail_option_1_raw.jpg")).unwrap();
        assert!(raw.width() > 1280, "{}", raw.width());
    }

    #[tokio::test]
    async fn a_big_crop_is_scaled_to_youtube_size() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.png");
        image::RgbImage::from_pixel(4000, 3000, image::Rgb([90, 120, 150])).save(&src).unwrap();
        let out = process_custom_thumbnail(src.to_str().unwrap(), tmp.path(), &crop(0, 0, 3200, 1800), true).await.unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (1280, 720));
    }
}
