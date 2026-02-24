/// Crop rectangle for thumbnail processing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CropRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

const ASPECT_RATIO: f64 = 16.0 / 9.0;
const ANCHOR_X: f64 = 0.70;
const TORSO_CUTOFF: f64 = 0.65;
const HEADROOM: f64 = 0.10;

/// Calculate a 16:9 crop centered on a detected person's upper body.
pub fn calculate_person_crop(
    bbox_x: f64,
    bbox_y: f64,
    bbox_w: f64,
    bbox_h: f64,
    img_w: u32,
    img_h: u32,
) -> CropRect {
    let img_w = img_w as f64;
    let img_h = img_h as f64;

    let y_bottom = bbox_y + bbox_h * TORSO_CUTOFF;
    let visible_height = bbox_h * TORSO_CUTOFF;
    let mut h_crop = visible_height * (1.0 + HEADROOM);
    let mut w_crop = h_crop * ASPECT_RATIO;
    let mut y_start = y_bottom - h_crop;
    let cx_person = bbox_x + bbox_w / 2.0;
    let mut x_start = cx_person - w_crop * ANCHOR_X;

    // Clamp to image bounds
    x_start = x_start.max(0.0).min(img_w - w_crop);
    y_start = y_start.max(0.0).min(img_h - h_crop);

    // If crop exceeds image, fit to image
    if w_crop > img_w || h_crop > img_h {
        if img_w / img_h > ASPECT_RATIO {
            h_crop = img_h;
            w_crop = h_crop * ASPECT_RATIO;
        } else {
            w_crop = img_w;
            h_crop = w_crop / ASPECT_RATIO;
        }
        x_start = (img_w - w_crop) / 2.0;
        y_start = (img_h - h_crop) / 2.0;
    }

    let mut x = x_start as i32;
    let mut y = y_start as i32;
    let mut w = w_crop as i32;
    let mut h = h_crop as i32;

    // Fallback for zero/negative dimensions
    if w <= 0 || h <= 0 {
        x = 0;
        y = 0;
        h = img_h as i32;
        w = (h as f64 * ASPECT_RATIO) as i32;
        if w > img_w as i32 {
            w = img_w as i32;
            h = (w as f64 / ASPECT_RATIO) as i32;
        }
    }

    CropRect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Fallback center crop when no person is detected.
pub fn calculate_fallback_crop(img_w: u32, img_h: u32) -> CropRect {
    let img_w = img_w as f64;
    let img_h = img_h as f64;

    let (w, h) = if img_w / img_h > ASPECT_RATIO {
        let h = img_h;
        let w = h * ASPECT_RATIO;
        (w, h)
    } else {
        let w = img_w;
        let h = w / ASPECT_RATIO;
        (w, h)
    };

    CropRect {
        x: ((img_w - w) / 2.0) as i32,
        y: ((img_h - h) / 2.0) as i32,
        width: w as i32,
        height: h as i32,
    }
}
