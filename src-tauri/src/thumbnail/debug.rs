use image::{Rgb, RgbImage};
use imageproc::drawing;
use imageproc::rect::Rect;

use super::crop::CropRect;
use super::scoring::{DetectionStatus, ScoredDetection};

/// COCO skeleton bone pairs.
const SKELETON: [(usize, usize); 16] = [
    (0, 1), (0, 2),   // nose → eyes
    (1, 3), (2, 4),   // eyes → ears
    (5, 6),            // shoulders
    (5, 7), (7, 9),   // left arm
    (6, 8), (8, 10),  // right arm
    (5, 11), (6, 12), // shoulders → hips
    (11, 12),          // hips
    (11, 13), (13, 15), // left leg
    (12, 14), (14, 16), // right leg
];

fn limb_color(i: usize, j: usize) -> Rgb<u8> {
    if i <= 4 || j <= 4 {
        Rgb([100, 200, 255]) // face: light blue
    } else if [5, 7, 9].contains(&i) && [5, 7, 9].contains(&j) {
        Rgb([100, 255, 100]) // left arm: green
    } else if [6, 8, 10].contains(&i) && [6, 8, 10].contains(&j) {
        Rgb([255, 100, 100]) // right arm: red
    } else if (i == 5 && j == 6)
        || (i == 11 && j == 12)
        || ([5, 6].contains(&i) && [11, 12].contains(&j))
    {
        Rgb([100, 255, 255]) // torso: cyan
    } else if [11, 13, 15].contains(&i) && [11, 13, 15].contains(&j) {
        Rgb([255, 255, 100]) // left leg: yellow
    } else if [12, 14, 16].contains(&i) && [12, 14, 16].contains(&j) {
        Rgb([255, 100, 255]) // right leg: magenta
    } else {
        Rgb([0, 255, 0])
    }
}

/// Render a debug image with bounding boxes, skeletons, labels, and crop rect.
pub fn render_debug(
    base_img: &RgbImage,
    detections: &[ScoredDetection],
    best_det: Option<&ScoredDetection>,
    crop: &CropRect,
) -> RgbImage {
    let mut img = base_img.clone();

    for det in detections {
        let is_winner = best_det
            .map(|b| std::ptr::eq(det, b))
            .unwrap_or(false)
            || best_det.map(|b| {
                (det.detection.x - b.detection.x).abs() < 1.0
                    && (det.detection.y - b.detection.y).abs() < 1.0
            }).unwrap_or(false);

        let color = match det.status {
            DetectionStatus::NotFacing => Rgb([255, 0, 0]),   // red
            DetectionStatus::Sitting => Rgb([255, 165, 0]),   // orange
            _ if is_winner => Rgb([0, 255, 0]),               // green
            _ => Rgb([255, 255, 0]),                          // yellow
        };

        let label = match det.status {
            DetectionStatus::NotFacing => format!("NOT FACING {}", det.details),
            DetectionStatus::Sitting => format!("SITTING {}", det.details),
            _ if is_winner => format!("WINNER {}", det.details),
            _ => det.details.clone(),
        };

        // Bounding box
        let bx = det.detection.x.max(0.0) as i32;
        let by = det.detection.y.max(0.0) as i32;
        let bw = (det.detection.w as u32).min(img.width().saturating_sub(bx as u32));
        let bh = (det.detection.h as u32).min(img.height().saturating_sub(by as u32));

        if bw > 0 && bh > 0 {
            draw_hollow_rect(&mut img, bx, by, bw, bh, color, 3);
        }

        // Skeleton
        draw_skeleton(&mut img, &det.detection.keypoints, is_winner);

        // Label (simplified - just draw text at top of bbox)
        draw_label(&mut img, bx, by, &label, color);
    }

    // Crop rectangle in blue
    draw_hollow_rect(
        &mut img,
        crop.x.max(0),
        crop.y.max(0),
        crop.width as u32,
        crop.height as u32,
        Rgb([0, 100, 255]),
        3,
    );

    img
}

fn draw_skeleton(img: &mut RgbImage, kps: &[[f32; 3]; 17], thick: bool) {
    let threshold = 0.3;

    // Draw bones
    for &(i, j) in &SKELETON {
        if kps[i][2] > threshold && kps[j][2] > threshold {
            let color = limb_color(i, j);
            let p1 = (kps[i][0] as f32, kps[i][1] as f32);
            let p2 = (kps[j][0] as f32, kps[j][1] as f32);
            drawing::draw_line_segment_mut(img, p1, p2, color);
            if thick {
                // Thicken by drawing offset lines
                drawing::draw_line_segment_mut(img, (p1.0 + 1.0, p1.1), (p2.0 + 1.0, p2.1), color);
                drawing::draw_line_segment_mut(img, (p1.0, p1.1 + 1.0), (p2.0, p2.1 + 1.0), color);
            }
        }
    }

    // Draw keypoints
    let kp_color = Rgb([0, 255, 255]); // yellow
    for (idx, kp) in kps.iter().enumerate() {
        if kp[2] > threshold {
            let radius = if [0, 5, 6, 11, 12].contains(&idx) { 5 } else { 3 };
            let cx = kp[0] as i32;
            let cy = kp[1] as i32;
            drawing::draw_filled_circle_mut(img, (cx, cy), radius, kp_color);
            drawing::draw_hollow_circle_mut(img, (cx, cy), radius, Rgb([0, 0, 0]));
        }
    }
}

fn draw_hollow_rect(img: &mut RgbImage, x: i32, y: i32, w: u32, h: u32, color: Rgb<u8>, thickness: i32) {
    for t in 0..thickness {
        let rx = (x - t).max(0);
        let ry = (y - t).max(0);
        let rw = (w as i32 + t * 2).max(1) as u32;
        let rh = (h as i32 + t * 2).max(1) as u32;
        if rx >= 0 && ry >= 0 {
            let rect = Rect::at(rx, ry).of_size(rw.min(img.width().saturating_sub(rx as u32)), rh.min(img.height().saturating_sub(ry as u32)));
            drawing::draw_hollow_rect_mut(img, rect, color);
        }
    }
}

fn draw_label(img: &mut RgbImage, x: i32, y: i32, label: &str, bg_color: Rgb<u8>) {
    // Simple label: draw a colored bar with the text
    let char_w = 7;
    let char_h = 12;
    let text_w = label.len() as i32 * char_w + 4;
    let bar_x = x.max(0) as u32;
    let bar_y = (y - char_h - 4).max(0) as u32;

    // Draw background bar
    for by in bar_y..((bar_y + char_h as u32 + 4).min(img.height())) {
        for bx in bar_x..((bar_x + text_w as u32).min(img.width())) {
            img.put_pixel(bx, by, bg_color);
        }
    }

    // Draw text characters using ab_glyph would be better, but for debug images
    // a simple approach is fine. Use imageproc's draw_text if a font is available.
    // For now, we just have the colored bar as a visual indicator.
    // The label info is already printed to stdout for debugging.
}
