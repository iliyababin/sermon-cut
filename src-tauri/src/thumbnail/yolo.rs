use image::RgbImage;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// A detected person with bounding box, confidence, and 17 COCO keypoints.
#[derive(Debug, Clone)]
pub struct PoseDetection {
    /// Bounding box: (x, y, width, height) in original image coordinates.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Detection confidence.
    pub conf: f32,
    /// 17 COCO keypoints, each [x, y, confidence].
    pub keypoints: [[f32; 3]; 17],
}

static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();

/// Initialise (or return cached) ONNX Runtime session for the YOLOv8-Pose model.
fn get_session(model_path: &Path) -> Result<&'static Mutex<Session>, String> {
    if let Some(s) = SESSION.get() {
        return Ok(s);
    }
    println!("[yolo] Loading ONNX model from {}", model_path.display());
    let session = Session::builder()
        .map_err(|e| format!("Failed to create session builder: {}", e))?
        .with_intra_threads(4)
        .map_err(|e| format!("Failed to set threads: {}", e))?
        .commit_from_file(model_path)
        .map_err(|e| format!("Failed to load ONNX model: {}", e))?;
    let _ = SESSION.set(Mutex::new(session));
    Ok(SESSION.get().unwrap())
}

/// Preprocess an image for YOLOv8: letterbox resize to 640×640, normalise, CHW layout.
///
/// Returns the input tensor and the (scale, pad_x, pad_y) needed to map back.
fn preprocess(img: &RgbImage) -> (Array4<f32>, f32, f32, f32) {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let target = 640.0_f32;

    let scale = (target / iw).min(target / ih);
    let new_w = (iw * scale).round() as u32;
    let new_h = (ih * scale).round() as u32;
    let pad_x = (target as u32 - new_w) as f32 / 2.0;
    let pad_y = (target as u32 - new_h) as f32 / 2.0;

    let resized = image::imageops::resize(img, new_w, new_h, image::imageops::FilterType::Triangle);

    // Build CHW f32 tensor with gray (114) letterbox padding
    let mut data = Array4::<f32>::from_elem((1, 3, 640, 640), 114.0 / 255.0);
    let px = pad_x.round() as u32;
    let py = pad_y.round() as u32;

    for y in 0..new_h {
        for x in 0..new_w {
            let p = resized.get_pixel(x, y);
            let ty = (py + y) as usize;
            let tx = (px + x) as usize;
            if ty < 640 && tx < 640 {
                data[[0, 0, ty, tx]] = p[0] as f32 / 255.0;
                data[[0, 1, ty, tx]] = p[1] as f32 / 255.0;
                data[[0, 2, ty, tx]] = p[2] as f32 / 255.0;
            }
        }
    }

    (data, scale, pad_x, pad_y)
}

/// Run YOLOv8-Pose inference on an image.
///
/// `model_path` is the path to `yolov8n-pose.onnx`.
pub fn detect(model_path: &Path, img: &RgbImage) -> Result<Vec<PoseDetection>, String> {
    let session_mutex = get_session(model_path)?;
    let mut session = session_mutex
        .lock()
        .map_err(|e| format!("Session lock poisoned: {}", e))?;
    let (input, scale, pad_x, pad_y) = preprocess(img);

    let input_tensor = Tensor::from_array(input)
        .map_err(|e| format!("Failed to create input tensor: {}", e))?;

    let outputs = session
        .run(ort::inputs![input_tensor])
        .map_err(|e| format!("Inference failed: {}", e))?;

    // Output shape: [1, 56, 8400]
    let output = &outputs[0];
    let (shape, data) = output
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("Failed to extract output tensor: {}", e))?;
    let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
    if dims.len() != 3 || dims[1] != 56 {
        return Err(format!(
            "Unexpected output shape {:?}, expected [1, 56, 8400]",
            dims
        ));
    }

    let num_detections = dims[2];

    // Data is in [1, 56, 8400] layout (row-major), we need to access [attr][det_idx]
    // data[batch * 56 * 8400 + attr * 8400 + det_idx]
    let conf_threshold = 0.5_f32;
    let mut detections = Vec::new();

    for i in 0..num_detections {
        let conf = data[4 * num_detections + i];
        if conf < conf_threshold {
            continue;
        }

        let cx = data[0 * num_detections + i];
        let cy = data[1 * num_detections + i];
        let w = data[2 * num_detections + i];
        let h = data[3 * num_detections + i];

        // Unscale from letterboxed 640×640 back to original image coords
        let x1 = (cx - w / 2.0 - pad_x) / scale;
        let y1 = (cy - h / 2.0 - pad_y) / scale;
        let bw = w / scale;
        let bh = h / scale;

        let mut keypoints = [[0.0_f32; 3]; 17];
        for k in 0..17 {
            let base = (5 + k * 3) * num_detections + i;
            let kx = (data[base] - pad_x) / scale;
            let ky = (data[base + num_detections] - pad_y) / scale;
            let kc = data[base + 2 * num_detections];
            keypoints[k] = [kx, ky, kc];
        }

        detections.push(PoseDetection {
            x: x1,
            y: y1,
            w: bw,
            h: bh,
            conf,
            keypoints,
        });
    }

    // Non-maximum suppression (IoU 0.45)
    nms(&mut detections, 0.45);

    Ok(detections)
}

/// Greedy NMS: sort by confidence, suppress boxes with IoU > threshold.
fn nms(detections: &mut Vec<PoseDetection>, iou_threshold: f32) {
    detections.sort_by(|a, b| b.conf.partial_cmp(&a.conf).unwrap_or(std::cmp::Ordering::Equal));

    let mut keep = Vec::with_capacity(detections.len());
    let mut suppressed = vec![false; detections.len()];

    for i in 0..detections.len() {
        if suppressed[i] {
            continue;
        }
        keep.push(i);
        for j in (i + 1)..detections.len() {
            if suppressed[j] {
                continue;
            }
            if iou(&detections[i], &detections[j]) > iou_threshold {
                suppressed[j] = true;
            }
        }
    }

    let kept: Vec<PoseDetection> = keep.into_iter().map(|i| detections[i].clone()).collect();
    *detections = kept;
}

fn iou(a: &PoseDetection, b: &PoseDetection) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);

    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let area_a = a.w * a.h;
    let area_b = b.w * b.h;
    let union = area_a + area_b - inter;

    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}
