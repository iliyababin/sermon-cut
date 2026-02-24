use super::yolo::PoseDetection;
use image::RgbImage;

// COCO keypoint indices
const NOSE: usize = 0;
const LEFT_EYE: usize = 1;
const RIGHT_EYE: usize = 2;
const LEFT_EAR: usize = 3;
const RIGHT_EAR: usize = 4;
const LEFT_SHOULDER: usize = 5;
const RIGHT_SHOULDER: usize = 6;
const LEFT_ELBOW: usize = 7;
const RIGHT_ELBOW: usize = 8;
const LEFT_WRIST: usize = 9;
const RIGHT_WRIST: usize = 10;
const LEFT_HIP: usize = 11;
const RIGHT_HIP: usize = 12;

const VISIBLE: f32 = 0.5;
const MIN_CONF: f32 = 0.3;

/// Facing direction of a detected person.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Facing {
    Front,
    FrontLeft,
    FrontRight,
    Left,
    Right,
    Back,
    Unknown,
}

impl Facing {
    pub fn is_facing_camera(self) -> bool {
        matches!(self, Facing::Front | Facing::FrontLeft | Facing::FrontRight)
    }

    pub fn label(self) -> &'static str {
        match self {
            Facing::Front => "front",
            Facing::FrontLeft => "front-left",
            Facing::FrontRight => "front-right",
            Facing::Left => "left",
            Facing::Right => "right",
            Facing::Back => "back",
            Facing::Unknown => "unknown",
        }
    }
}

/// Estimate which direction a person is facing.
pub fn estimate_facing_direction(kps: &[[f32; 3]; 17]) -> (Facing, f32) {
    let nose = kps[NOSE];
    let left_eye = kps[LEFT_EYE];
    let right_eye = kps[RIGHT_EYE];
    let left_ear = kps[LEFT_EAR];
    let right_ear = kps[RIGHT_EAR];

    let nose_vis = nose[2] > VISIBLE;
    let le_vis = left_eye[2] > VISIBLE;
    let re_vis = right_eye[2] > VISIBLE;
    let lea_vis = left_ear[2] > VISIBLE;
    let rea_vis = right_ear[2] > VISIBLE;

    // Back-facing (no face features visible)
    if !nose_vis && !le_vis && !re_vis {
        return (Facing::Back, 0.8);
    }

    // Front-facing (both eyes + nose visible)
    if le_vis && re_vis && nose_vis {
        let eye_spread = (left_eye[0] - right_eye[0]).abs();
        if eye_spread > 10.0 {
            if lea_vis && rea_vis {
                return (Facing::Front, 0.95);
            } else if !lea_vis && !rea_vis {
                return (Facing::Front, 0.9);
            } else if lea_vis {
                return (Facing::FrontRight, 0.7);
            } else {
                return (Facing::FrontLeft, 0.7);
            }
        }
    }

    // Profile detection
    let left_features = le_vis as i32 + lea_vis as i32;
    let right_features = re_vis as i32 + rea_vis as i32;

    if left_features > right_features {
        return (Facing::Right, 0.7);
    } else if right_features > left_features {
        return (Facing::Left, 0.7);
    }

    if nose_vis {
        return (Facing::Front, 0.5);
    }

    (Facing::Unknown, 0.3)
}

/// Estimate head pitch: positive → looking down, negative → looking up.
/// Returns (pitch_ratio, looking_down, can_estimate).
pub fn estimate_head_pitch(kps: &[[f32; 3]; 17]) -> (f32, bool, bool) {
    let nose = kps[NOSE];
    let left_eye = kps[LEFT_EYE];
    let right_eye = kps[RIGHT_EYE];

    let nose_vis = nose[2] > VISIBLE;
    let le_vis = left_eye[2] > VISIBLE;
    let re_vis = right_eye[2] > VISIBLE;

    if !nose_vis || (!le_vis && !re_vis) {
        return (0.0, false, false);
    }

    let (avg_eye_y, mut eye_distance) = if le_vis && re_vis {
        (
            (left_eye[1] + right_eye[1]) / 2.0,
            (left_eye[0] - right_eye[0]).abs(),
        )
    } else if le_vis {
        (left_eye[1], 30.0)
    } else {
        (right_eye[1], 30.0)
    };

    eye_distance = eye_distance.max(20.0);
    let pitch_ratio = (nose[1] - avg_eye_y) / eye_distance;

    (pitch_ratio, pitch_ratio > 0.6, true)
}

/// Calculate gesture/dynamism score from arm positions (0.0–1.0).
pub fn calculate_gesture_score(kps: &[[f32; 3]; 17]) -> f32 {
    let l_shoulder = kps[LEFT_SHOULDER];
    let r_shoulder = kps[RIGHT_SHOULDER];
    let l_elbow = kps[LEFT_ELBOW];
    let r_elbow = kps[RIGHT_ELBOW];
    let l_wrist = kps[LEFT_WRIST];
    let r_wrist = kps[RIGHT_WRIST];
    let l_hip = kps[LEFT_HIP];
    let r_hip = kps[RIGHT_HIP];

    let has_shoulders = l_shoulder[2] > MIN_CONF && r_shoulder[2] > MIN_CONF;
    let has_left_arm = l_elbow[2] > MIN_CONF && l_wrist[2] > MIN_CONF;
    let has_right_arm = r_elbow[2] > MIN_CONF && r_wrist[2] > MIN_CONF;
    let has_hips = l_hip[2] > MIN_CONF && r_hip[2] > MIN_CONF;

    if !has_shoulders || (!has_left_arm && !has_right_arm) {
        return 0.5;
    }

    let mut shoulder_width = (r_shoulder[0] - l_shoulder[0]).abs();
    if shoulder_width < 10.0 {
        shoulder_width = 100.0;
    }

    let torso_height = if has_hips {
        ((l_hip[1] + r_hip[1]) / 2.0 - (l_shoulder[1] + r_shoulder[1]) / 2.0).abs()
    } else {
        shoulder_width * 1.5
    };

    let shoulder_y = (l_shoulder[1] + r_shoulder[1]) / 2.0;

    // Arms raised score
    let mut arms_raised = 0.0_f32;
    if has_left_arm {
        let left_raise = (shoulder_y - l_wrist[1]) / torso_height;
        arms_raised += (left_raise + 0.2).clamp(0.0, 1.0);
    }
    if has_right_arm {
        let right_raise = (shoulder_y - r_wrist[1]) / torso_height;
        arms_raised += (right_raise + 0.2).clamp(0.0, 1.0);
    }
    if has_left_arm && has_right_arm {
        arms_raised /= 2.0;
    }
    arms_raised = arms_raised.min(1.0);

    // Arm spread score
    let arm_spread = if has_left_arm && has_right_arm {
        let wrist_spread = (r_wrist[0] - l_wrist[0]).abs();
        (wrist_spread / (shoulder_width * 2.5)).min(1.0)
    } else if has_left_arm || has_right_arm {
        let wrist = if has_left_arm { l_wrist } else { r_wrist };
        let shoulder = if has_left_arm { l_shoulder } else { r_shoulder };
        let extension = (wrist[0] - shoulder[0]).abs();
        (extension / (shoulder_width * 1.5)).min(1.0)
    } else {
        0.0
    };

    // Gesturing score
    let mut gesturing = if has_hips {
        let hip_y = (l_hip[1] + r_hip[1]) / 2.0;
        let hip_x_left = l_hip[0];
        let hip_x_right = r_hip[0];

        let mut g = 0.0_f32;
        if has_left_arm {
            let y_dist = (l_wrist[1] - hip_y).abs() / torso_height;
            let x_dist = (l_wrist[0] - hip_x_left).abs() / shoulder_width;
            g += y_dist.max(x_dist).min(1.0);
        }
        if has_right_arm {
            let y_dist = (r_wrist[1] - hip_y).abs() / torso_height;
            let x_dist = (r_wrist[0] - hip_x_right).abs() / shoulder_width;
            g += y_dist.max(x_dist).min(1.0);
        }
        if has_left_arm && has_right_arm {
            g / 2.0
        } else {
            g
        }
    } else {
        arms_raised
    };
    gesturing = gesturing.min(1.0);

    arms_raised * 0.4 + gesturing * 0.35 + arm_spread * 0.25
}

/// Calculate image blur score for the upper body region via Laplacian variance.
pub fn calculate_blur_score(img: &RgbImage, det: &PoseDetection) -> f32 {
    let torso_cutoff = 0.65_f32;
    let x1 = (det.x.max(0.0)) as u32;
    let y1 = (det.y.max(0.0)) as u32;
    let x2 = ((det.x + det.w) as u32).min(img.width());
    let upper_h = (det.h * torso_cutoff) as u32;
    let y2 = ((det.y as u32) + upper_h).min(img.height());

    if x2 <= x1 || y2 <= y1 {
        return 0.75; // default sharpness when crop is invalid
    }

    // Convert ROI to grayscale and compute Laplacian variance
    let w = (x2 - x1) as usize;
    let h = (y2 - y1) as usize;
    if w < 3 || h < 3 {
        return 0.75;
    }

    let mut gray = vec![0.0_f64; w * h];
    for iy in 0..h {
        for ix in 0..w {
            let p = img.get_pixel(x1 + ix as u32, y1 + iy as u32);
            gray[iy * w + ix] = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
        }
    }

    // 3×3 Laplacian kernel [0,1,0; 1,-4,1; 0,1,0]
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let count = ((h - 2) * (w - 2)) as f64;

    for iy in 1..(h - 1) {
        for ix in 1..(w - 1) {
            let lap = -4.0 * gray[iy * w + ix]
                + gray[(iy - 1) * w + ix]
                + gray[(iy + 1) * w + ix]
                + gray[iy * w + ix - 1]
                + gray[iy * w + ix + 1];
            sum += lap;
            sum_sq += lap * lap;
        }
    }

    let variance = if count > 0.0 {
        (sum_sq / count) - (sum / count).powi(2)
    } else {
        0.0
    };

    // Normalise: [50, 500] → [0, 1]
    let normalized = ((variance - 50.0) / 450.0).clamp(0.0, 1.0) as f32;
    // Sharpness bonus: [0.5, 1.0]
    0.5 + normalized * 0.5
}

/// Scoring constants.
const MIN_ASPECT_RATIO: f32 = 1.15;
pub const FACING_BONUS: f32 = 1.3;
pub const GESTURE_BONUS: f32 = 1.5;
pub const PITCH_THRESHOLD: f32 = 0.6;
pub const PITCH_PENALTY: f32 = 0.6;

/// Detailed scoring result for a single detection.
#[derive(Debug, Clone)]
pub struct ScoredDetection {
    pub detection: PoseDetection,
    pub score: f32,
    pub status: DetectionStatus,
    pub details: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetectionStatus {
    Valid,
    NotFacing,
    Sitting,
}

/// Score a single detection against the full image.
pub fn score_detection(det: &PoseDetection, img: &RgbImage) -> ScoredDetection {
    let img_w = img.width() as f32;
    let aspect_ratio = if det.w > 0.0 { det.h / det.w } else { 0.0 };

    let (facing, _) = estimate_facing_direction(&det.keypoints);
    if !facing.is_facing_camera() {
        return ScoredDetection {
            detection: det.clone(),
            score: 0.0,
            status: DetectionStatus::NotFacing,
            details: format!("facing={}", facing.label()),
        };
    }

    if aspect_ratio < MIN_ASPECT_RATIO {
        return ScoredDetection {
            detection: det.clone(),
            score: 0.0,
            status: DetectionStatus::Sitting,
            details: format!("ar={:.2}", aspect_ratio),
        };
    }

    let sharpness_bonus = calculate_blur_score(img, det);

    let standing_bonus = if aspect_ratio > 1.5 {
        1.0
    } else if aspect_ratio > 1.3 {
        0.8
    } else {
        0.5
    };

    let box_cx = det.x + det.w / 2.0;
    let center_dist = (box_cx - img_w / 2.0).abs() / (img_w / 2.0);
    let center_bonus = 1.0 - center_dist * 0.3;

    let facing_score = if facing.is_facing_camera() {
        FACING_BONUS
    } else {
        1.0
    };

    let gesture_score = calculate_gesture_score(&det.keypoints);
    let gest_bonus = 1.0 + (GESTURE_BONUS - 1.0) * gesture_score;

    let (pitch_ratio, _, can_estimate_pitch) = estimate_head_pitch(&det.keypoints);
    let pitch_bonus = if can_estimate_pitch {
        if facing == Facing::Front && pitch_ratio > PITCH_THRESHOLD {
            PITCH_PENALTY
        } else if matches!(facing, Facing::FrontLeft | Facing::FrontRight)
            && pitch_ratio > PITCH_THRESHOLD + 0.2
        {
            PITCH_PENALTY + 0.2
        } else {
            1.0
        }
    } else {
        1.0
    };

    let score = det.conf
        * standing_bonus
        * center_bonus
        * facing_score
        * gest_bonus
        * sharpness_bonus
        * pitch_bonus;

    let details = format!(
        "s={:.2} ar={:.1} face={} gest={:.2} pitch={:.2}",
        score,
        aspect_ratio,
        facing.label(),
        gesture_score,
        pitch_ratio
    );

    ScoredDetection {
        detection: det.clone(),
        score,
        status: DetectionStatus::Valid,
        details,
    }
}
