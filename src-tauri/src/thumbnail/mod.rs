mod color;
mod crop;
mod debug;
mod frames;
mod grading;
mod logo;
mod scoring;
mod yolo;

pub use crop::CropRect;

use image::RgbImage;
use scoring::{score_detection, DetectionStatus, ScoredDetection};
use std::path::{Path, PathBuf};

const NUM_CANDIDATES: u32 = 60;

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

/// Load an image from disk as an `RgbImage`.
fn load_image(path: &Path) -> Result<RgbImage, String> {
    image::open(path)
        .map(|i| i.to_rgb8())
        .map_err(|e| format!("Failed to load image {}: {}", path.display(), e))
}

/// Save an `RgbImage` as JPEG with quality 92.
fn save_jpeg(img: &RgbImage, path: &Path) -> Result<(), String> {
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92);
    img.write_with_encoder(encoder)
        .map_err(|e| format!("JPEG encode error: {}", e))?;
    std::fs::write(path, &buf).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

/// Core pipeline: detect → score → pick best → crop → grade → logo → save.
///
/// Returns `(final_image_path, debug_image_path_if_any)`.
async fn run_pipeline(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    output_dir: &Path,
    output_stem: &str,
    logo_path: Option<&str>,
    count: u32,
) -> Result<Vec<String>, String> {
    let model_path = find_model_path()?;

    // Decide where to put candidate frames
    let temp_dir_holder;
    let frame_dir = if count > 1 {
        let d = output_dir.join("candidates");
        std::fs::create_dir_all(&d)
            .map_err(|e| format!("Failed to create candidates dir: {}", e))?;
        d
    } else {
        temp_dir_holder = tempfile::tempdir()
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;
        temp_dir_holder.path().to_path_buf()
    };

    // Step 1: Extract candidate frames
    let frame_data =
        frames::extract_candidate_frames(video_path, start_time, end_time, NUM_CANDIDATES, &frame_dir).await?;

    // Step 2: Run YOLO + scoring on each frame
    let mut all_scored: Vec<(PathBuf, ScoredDetection)> = Vec::new();

    for (timestamp, frame_path) in &frame_data {
        let img = match load_image(frame_path) {
            Ok(i) => i,
            Err(_) => continue,
        };

        let detections = match yolo::detect(&model_path, &img) {
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

    // Stats
    let valid_count = all_scored
        .iter()
        .filter(|(_, s)| s.status == DetectionStatus::Valid)
        .count();
    let not_facing = all_scored
        .iter()
        .filter(|(_, s)| s.status == DetectionStatus::NotFacing)
        .count();
    let sitting = all_scored
        .iter()
        .filter(|(_, s)| s.status == DetectionStatus::Sitting)
        .count();
    println!(
        "[thumbnail] Detection summary: {} valid, {} not facing, {} sitting",
        valid_count, not_facing, sitting
    );

    // Filter to valid detections only, sort by score desc
    let mut valid: Vec<(PathBuf, ScoredDetection)> = all_scored
        .iter()
        .filter(|(_, s)| s.status == DetectionStatus::Valid)
        .cloned()
        .collect();
    valid.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));

    // Select top-N from distinct frames
    let mut selected: Vec<(PathBuf, ScoredDetection)> = Vec::new();
    let mut used_frames = std::collections::HashSet::new();

    for (path, scored) in &valid {
        if selected.len() >= count as usize {
            break;
        }
        if used_frames.contains(path) {
            continue;
        }
        selected.push((path.clone(), scored.clone()));
        used_frames.insert(path.clone());
    }

    // Fallback: no valid detections
    if selected.is_empty() {
        println!("[thumbnail] No person detected, using center crop of middle frame");
        let mid = &frame_data[frame_data.len() / 2];
        let img = load_image(&mid.1)?;
        let fallback_crop = crop::calculate_fallback_crop(img.width(), img.height());

        let mut cropped = crop_image(&img, &fallback_crop);
        grading::apply_color_grading(&mut cropped);
        if let Some(lp) = logo_path {
            logo::apply_logo_overlay(&mut cropped, lp);
        }

        let out_path = output_dir.join(format!("{}_thumbnail.jpg", output_stem));
        save_jpeg(&cropped, &out_path)?;

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

        // Save raw crop for editor use (multi-thumbnail mode)
        if count > 1 {
            let raw_cropped = crop_image(&img, &person_crop);
            let raw_path = output_dir.join(format!("thumbnail_option_{}_raw.jpg", idx + 1));
            save_jpeg(&raw_cropped, &raw_path)?;
        }

        // Generate debug image
        let detections_for_frame: Vec<&ScoredDetection> = all_scored
            .iter()
            .filter(|(p, _)| p == frame_path)
            .map(|(_, s)| s)
            .collect();

        let debug_img = debug::render_debug(
            &img,
            &detections_for_frame
                .iter()
                .map(|s| (*s).clone())
                .collect::<Vec<_>>(),
            Some(scored),
            &person_crop,
        );

        let debug_name = if count > 1 {
            format!("thumbnail_option_{}_debug.jpg", idx + 1)
        } else {
            format!("{}_thumbnail_debug.jpg", output_stem)
        };
        let debug_path = output_dir.join(&debug_name);
        save_jpeg(&debug_img, &debug_path)?;

        // Crop → grade → logo
        let mut result = crop_image(&img, &person_crop);
        grading::apply_color_grading(&mut result);
        if let Some(lp) = logo_path {
            logo::apply_logo_overlay(&mut result, lp);
        }

        let out_name = if count > 1 {
            format!("thumbnail_option_{}.jpg", idx + 1)
        } else {
            format!("{}_thumbnail.jpg", output_stem)
        };
        let out_path = output_dir.join(&out_name);
        save_jpeg(&result, &out_path)?;

        let abs = out_path
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize: {}", e))?;
        output_paths.push(abs.to_string_lossy().to_string());

        println!(
            "[thumbnail] Generated thumbnail {}/{}: score={:.3}",
            idx + 1,
            selected.len(),
            scored.score
        );
    }

    Ok(output_paths)
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

/// Generate a single best thumbnail for a video.
pub async fn generate_thumbnail(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    title: &str,
    output_dir: &str,
    logo_path: Option<&str>,
) -> Result<String, String> {
    let video_path_obj = Path::new(video_path);
    if !video_path_obj.exists() {
        return Err(format!("Video file not found: {}", video_path));
    }

    println!("[thumbnail] Generating thumbnail for: {}", title);

    let video_stem = video_path_obj
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("thumbnail");

    let effective_output_dir: PathBuf = if output_dir.is_empty() {
        dirs::video_dir()
            .map(|p| p.join("sermon-cut"))
            .unwrap_or_else(|| video_path_obj.parent().unwrap_or(Path::new(".")).to_path_buf())
    } else {
        PathBuf::from(output_dir)
    };

    let thumbnail_dir = effective_output_dir.join("thumbnails");
    std::fs::create_dir_all(&thumbnail_dir)
        .map_err(|e| format!("Failed to create thumbnails directory: {}", e))?;

    let paths =
        run_pipeline(video_path, start_time, end_time, &thumbnail_dir, video_stem, logo_path, 1)
            .await?;

    paths
        .into_iter()
        .next()
        .ok_or_else(|| "No thumbnail generated".to_string())
}

/// Generate multiple thumbnail options for review.
pub async fn generate_thumbnail_options(
    video_path: &str,
    start_time: f64,
    end_time: f64,
    title: &str,
    output_dir: &str,
    count: u32,
    logo_path: Option<&str>,
) -> Result<Vec<String>, String> {
    let video_path_obj = Path::new(video_path);
    if !video_path_obj.exists() {
        return Err(format!("Video file not found: {}", video_path));
    }

    println!(
        "[thumbnail] Generating {} thumbnail options for: {}",
        count, title
    );

    let effective_output_dir: PathBuf = if output_dir.is_empty() {
        dirs::video_dir()
            .map(|p| p.join("sermon-cut"))
            .unwrap_or_else(|| video_path_obj.parent().unwrap_or(Path::new(".")).to_path_buf())
    } else {
        PathBuf::from(output_dir)
    };

    let thumbnail_dir = effective_output_dir.join("thumbnails");
    std::fs::create_dir_all(&thumbnail_dir)
        .map_err(|e| format!("Failed to create thumbnails directory: {}", e))?;

    let video_stem = video_path_obj
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("thumbnail");

    run_pipeline(
        video_path,
        start_time,
        end_time,
        &thumbnail_dir,
        video_stem,
        logo_path,
        count,
    )
    .await
}

/// Process a custom image as a thumbnail with crop, color grading, and logo overlay.
pub async fn process_custom_thumbnail(
    source_path: &str,
    output_dir: &str,
    crop_rect: &CropRect,
    apply_color_grading_flag: bool,
    logo_path: Option<&str>,
) -> Result<String, String> {
    let source = Path::new(source_path);
    if !source.exists() {
        return Err(format!("Source image not found: {}", source_path));
    }

    let effective_output_dir = if output_dir.is_empty() {
        dirs::video_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sermon-cut")
    } else {
        PathBuf::from(output_dir)
    };

    let thumbnail_dir = effective_output_dir.join("thumbnails");
    std::fs::create_dir_all(&thumbnail_dir)
        .map_err(|e| format!("Failed to create thumbnails directory: {}", e))?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let output_path = thumbnail_dir.join(format!("custom_thumbnail_{}.jpg", timestamp));

    println!(
        "[thumbnail] Processing custom thumbnail from: {}",
        source_path
    );

    let img = load_image(source)?;

    // Clamp crop rect to image bounds
    let clamped = CropRect {
        x: crop_rect.x.max(0).min(img.width() as i32 - 1),
        y: crop_rect.y.max(0).min(img.height() as i32 - 1),
        width: crop_rect
            .width
            .max(1)
            .min(img.width() as i32 - crop_rect.x.max(0)),
        height: crop_rect
            .height
            .max(1)
            .min(img.height() as i32 - crop_rect.y.max(0)),
    };

    println!(
        "[thumbnail] Cropping: x={}, y={}, w={}, h={}",
        clamped.x, clamped.y, clamped.width, clamped.height
    );

    let mut result = crop_image(&img, &clamped);

    if apply_color_grading_flag {
        println!("[thumbnail] Applying color grading");
        grading::apply_color_grading(&mut result);
    }

    if let Some(lp) = logo_path {
        println!("[thumbnail] Applying logo overlay from: {}", lp);
        logo::apply_logo_overlay(&mut result, lp);
    }

    save_jpeg(&result, &output_path)?;
    println!(
        "[thumbnail] Saved custom thumbnail to: {}",
        output_path.display()
    );

    let absolute_path = output_path
        .canonicalize()
        .map_err(|e| format!("Failed to get absolute path: {}", e))?;

    Ok(absolute_path.to_string_lossy().to_string())
}
