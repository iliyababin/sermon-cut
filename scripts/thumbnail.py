#!/usr/bin/env python3
"""
Standalone thumbnail generator using YOLOv8 Pose.
For use with the Clipper Desktop app.

Usage:
    python thumbnail.py --video /path/to/video.mp4 --start 00:30:00 --end 01:00:00 --title "Sermon Title" --output /path/to/thumbnail.jpg
"""

import argparse
import sys
from pathlib import Path
import subprocess


def parse_time_to_seconds(time_str: str) -> int:
    """Convert HH:MM:SS or MM:SS to seconds."""
    parts = time_str.split(":")
    if len(parts) == 3:
        return int(parts[0]) * 3600 + int(parts[1]) * 60 + int(parts[2])
    elif len(parts) == 2:
        return int(parts[0]) * 60 + int(parts[1])
    return int(parts[0])


def get_blur_score(img):
    """Calculate image sharpness using Laplacian variance."""
    import cv2
    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    laplacian = cv2.Laplacian(gray, cv2.CV_64F)
    return laplacian.var()


def normalize_blur_score(blur_variance, min_blur=50, max_blur=500):
    """Convert blur variance to 0-1 score."""
    if blur_variance <= min_blur:
        return 0.0
    elif blur_variance >= max_blur:
        return 1.0
    else:
        return (blur_variance - min_blur) / (max_blur - min_blur)


def estimate_facing_direction(keypoints):
    """
    Estimate which direction a person is facing based on keypoint visibility.
    Returns: (direction, confidence)
    """
    NOSE, LEFT_EYE, RIGHT_EYE, LEFT_EAR, RIGHT_EAR = 0, 1, 2, 3, 4

    nose = keypoints[NOSE]
    left_eye = keypoints[LEFT_EYE]
    right_eye = keypoints[RIGHT_EYE]
    left_ear = keypoints[LEFT_EAR]
    right_ear = keypoints[RIGHT_EAR]

    VISIBLE = 0.5
    nose_visible = nose[2] > VISIBLE
    left_eye_visible = left_eye[2] > VISIBLE
    right_eye_visible = right_eye[2] > VISIBLE
    left_ear_visible = left_ear[2] > VISIBLE
    right_ear_visible = right_ear[2] > VISIBLE

    # Back-facing (no face features)
    if not nose_visible and not left_eye_visible and not right_eye_visible:
        return 'back', 0.8

    # Front-facing (both eyes visible)
    if left_eye_visible and right_eye_visible and nose_visible:
        eye_spread = abs(left_eye[0] - right_eye[0])
        if eye_spread > 10:
            if left_ear_visible and right_ear_visible:
                return 'front', 0.95
            elif not left_ear_visible and not right_ear_visible:
                return 'front', 0.9
            else:
                # One ear visible = slightly turned
                if left_ear_visible:
                    return 'front-right', 0.7
                else:
                    return 'front-left', 0.7

    # Profile detection
    left_features = sum([left_eye_visible, left_ear_visible])
    right_features = sum([right_eye_visible, right_ear_visible])

    if left_features > right_features:
        return 'right', 0.7
    elif right_features > left_features:
        return 'left', 0.7

    if nose_visible:
        return 'front', 0.5

    return 'unknown', 0.3


def estimate_head_pitch(keypoints):
    """
    Estimate head pitch (looking up/down) based on nose position relative to eyes.
    Returns: (pitch_ratio, looking_down, can_estimate)
    - pitch_ratio: positive = looking down, negative = looking up
    - looking_down: bool, True if pitch exceeds threshold
    - can_estimate: bool, True if we have enough keypoints
    """
    NOSE, LEFT_EYE, RIGHT_EYE = 0, 1, 2

    nose = keypoints[NOSE]
    left_eye = keypoints[LEFT_EYE]
    right_eye = keypoints[RIGHT_EYE]

    VISIBLE = 0.5
    nose_visible = nose[2] > VISIBLE
    left_eye_visible = left_eye[2] > VISIBLE
    right_eye_visible = right_eye[2] > VISIBLE

    # Need nose and at least one eye to estimate pitch
    if not nose_visible or (not left_eye_visible and not right_eye_visible):
        return 0, False, False

    # Calculate average eye Y position
    if left_eye_visible and right_eye_visible:
        avg_eye_y = (left_eye[1] + right_eye[1]) / 2
        eye_distance = abs(left_eye[0] - right_eye[0])
    elif left_eye_visible:
        avg_eye_y = left_eye[1]
        eye_distance = 30  # Estimate
    else:
        avg_eye_y = right_eye[1]
        eye_distance = 30  # Estimate

    # Normalize by eye distance (or use a minimum to avoid division issues)
    eye_distance = max(eye_distance, 20)

    # Calculate pitch ratio: positive = nose below eyes = looking down
    # In image coordinates, Y increases downward
    pitch_ratio = (nose[1] - avg_eye_y) / eye_distance

    return pitch_ratio, pitch_ratio > 0.6, True


def draw_skeleton(img, keypoints, color=(0, 255, 0), thickness=2):
    """
    Draw pose skeleton on image.
    Keypoints are in COCO format (17 keypoints).
    """
    import cv2

    # COCO skeleton connections
    skeleton = [
        (0, 1), (0, 2),  # nose to eyes
        (1, 3), (2, 4),  # eyes to ears
        (5, 6),  # shoulders
        (5, 7), (7, 9),  # left arm
        (6, 8), (8, 10),  # right arm
        (5, 11), (6, 12),  # shoulders to hips
        (11, 12),  # hips
        (11, 13), (13, 15),  # left leg
        (12, 14), (14, 16),  # right leg
    ]

    # Different colors for different body parts
    limb_colors = {
        'face': (255, 200, 100),     # light blue for face
        'arm_left': (100, 255, 100),  # green for left arm
        'arm_right': (100, 100, 255), # red for right arm
        'torso': (255, 255, 100),     # cyan for torso
        'leg_left': (100, 255, 255),  # yellow for left leg
        'leg_right': (255, 100, 255), # magenta for right leg
    }

    def get_limb_color(i, j):
        if i <= 4 or j <= 4:
            return limb_colors['face']
        if i in [5, 7, 9] and j in [5, 7, 9]:
            return limb_colors['arm_left']
        if i in [6, 8, 10] and j in [6, 8, 10]:
            return limb_colors['arm_right']
        if (i in [5, 6] and j in [11, 12]) or (i == 5 and j == 6) or (i == 11 and j == 12):
            return limb_colors['torso']
        if i in [11, 13, 15] and j in [11, 13, 15]:
            return limb_colors['leg_left']
        if i in [12, 14, 16] and j in [12, 14, 16]:
            return limb_colors['leg_right']
        return color

    # Draw skeleton lines
    for (i, j) in skeleton:
        if keypoints[i][2] > 0.3 and keypoints[j][2] > 0.3:
            pt1 = (int(keypoints[i][0]), int(keypoints[i][1]))
            pt2 = (int(keypoints[j][0]), int(keypoints[j][1]))
            limb_color = get_limb_color(i, j)
            cv2.line(img, pt1, pt2, limb_color, thickness)

    # Draw keypoints
    for idx, kp in enumerate(keypoints):
        if kp[2] > 0.3:  # confidence threshold
            x, y = int(kp[0]), int(kp[1])
            # Larger circles for important joints
            radius = 6 if idx in [0, 5, 6, 11, 12] else 4
            cv2.circle(img, (x, y), radius, (0, 255, 255), -1)  # yellow filled
            cv2.circle(img, (x, y), radius, (0, 0, 0), 1)  # black outline


def apply_color_grading(img):
    """
    Apply cinematic color grading to an image.
    Returns the color graded image.
    """
    import cv2
    import numpy as np

    # Color grading
    lab = cv2.cvtColor(img, cv2.COLOR_BGR2LAB)
    l, a, b = cv2.split(lab)
    clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
    l = clahe.apply(l)
    l = np.clip(l.astype(np.float32) * 1.02 + 2, 0, 255).astype(np.uint8)

    # Teal/orange color grade
    a = a.astype(np.float32)
    b = b.astype(np.float32)
    l_norm = l.astype(np.float32) / 255.0
    shadow_mask = 1.0 - l_norm
    highlight_mask = l_norm
    a = a - (shadow_mask * 4) + (highlight_mask * 3)
    b = b - (shadow_mask * 6) + (highlight_mask * 5)
    a = np.clip(a, 0, 255).astype(np.uint8)
    b = np.clip(b, 0, 255).astype(np.uint8)
    graded = cv2.merge([l, a, b])
    result = cv2.cvtColor(graded, cv2.COLOR_LAB2BGR)

    # Vibrance boost
    hsv = cv2.cvtColor(result, cv2.COLOR_BGR2HSV).astype(np.float32)
    h, s, v = cv2.split(hsv)
    saturation_boost = 1.12
    s = s * (1 + (1 - s / 255) * (saturation_boost - 1) * 0.5)
    s = np.clip(s, 0, 255)
    hsv = cv2.merge([h, s, v])
    result = cv2.cvtColor(hsv.astype(np.uint8), cv2.COLOR_HSV2BGR)

    # Subtle vignette
    rows, cols = result.shape[:2]
    X = cv2.getGaussianKernel(cols, cols * 0.7)
    Y = cv2.getGaussianKernel(rows, rows * 0.7)
    vignette = Y * X.T
    vignette = vignette / vignette.max()
    vignette = (vignette * 0.25 + 0.75)
    for i in range(3):
        result[:, :, i] = (result[:, :, i] * vignette).astype(np.uint8)

    return result


def apply_logo_overlay(img, logo_path):
    """
    Apply logo overlay with gradient to an image.
    Returns the image with logo overlay.
    """
    import cv2
    import numpy as np
    from PIL import Image, ImageDraw

    img_pil = Image.fromarray(cv2.cvtColor(img, cv2.COLOR_BGR2RGB))
    draw = ImageDraw.Draw(img_pil, 'RGBA')
    img_height, img_width = img.shape[:2]

    padding = 150
    logo_size = int(min(img_width, img_height) * 0.26)

    logo = None
    logo_w, logo_h = 0, 0
    if logo_path and Path(logo_path).exists():
        try:
            logo = Image.open(logo_path).convert("RGBA")
            aspect = logo.width / logo.height
            if aspect >= 1:
                logo_w = logo_size
                logo_h = int(logo_size / aspect)
            else:
                logo_h = logo_size
                logo_w = int(logo_size * aspect)
            logo = logo.resize((logo_w, logo_h), Image.LANCZOS)
        except Exception as e:
            print(f"[WARN] Could not load logo: {e}")

    if logo:
        content_h = logo_h
        start_y = (img_height - content_h) // 2
        content_w = logo_w

        # Slanted gradient (scaled to image size)
        slant_amount = int(img_width * 0.18)
        fade_length = int(img_width * 0.36)
        solid_region = int(img_width * 0.08)

        y_coords = np.arange(img_height).reshape(-1, 1)
        x_coords = np.arange(img_width).reshape(1, -1)
        fade_starts = (solid_region + (y_coords / img_height) * slant_amount).astype(np.float32)
        alpha = np.zeros((img_height, img_width), dtype=np.float32)
        alpha = np.where(x_coords < fade_starts, 220, alpha)
        mask = (x_coords >= fade_starts) & (x_coords < fade_starts + fade_length)
        progress = np.clip((x_coords - fade_starts) / fade_length, 0, 1)
        alpha = np.where(mask, 220 * (1 - progress), alpha)

        gradient_arr = np.zeros((img_height, img_width, 4), dtype=np.uint8)
        gradient_arr[:, :, 3] = alpha.astype(np.uint8)
        gradient = Image.fromarray(gradient_arr, 'RGBA')
        img_pil.paste(gradient, (0, 0), gradient)

        center_x = (content_w + padding * 2) // 2
        logo_y = start_y
        logo_x = center_x - logo_w // 2
        img_pil.paste(logo, (logo_x, logo_y), logo)

    return cv2.cvtColor(np.array(img_pil), cv2.COLOR_RGB2BGR)


def process_custom_thumbnail(
    source_path: str,
    output_path: str,
    crop_x: int,
    crop_y: int,
    crop_width: int,
    crop_height: int,
    apply_grading: bool = True,
    logo_path: str = None
) -> str:
    """
    Process a custom thumbnail image with optional crop, color grading, and logo overlay.

    Args:
        source_path: Path to the source image
        output_path: Path to save the processed thumbnail
        crop_x: X coordinate of crop rectangle (in source image coordinates)
        crop_y: Y coordinate of crop rectangle
        crop_width: Width of crop rectangle
        crop_height: Height of crop rectangle
        apply_grading: Whether to apply color grading
        logo_path: Optional path to logo for overlay

    Returns:
        Path to the saved thumbnail
    """
    import cv2

    print(f"[INFO] Processing custom thumbnail from: {source_path}")

    # Load source image
    img = cv2.imread(source_path)
    if img is None:
        raise ValueError(f"Could not load image: {source_path}")

    img_h, img_w = img.shape[:2]

    # Clamp crop rectangle to image bounds
    crop_x = max(0, min(crop_x, img_w - 1))
    crop_y = max(0, min(crop_y, img_h - 1))
    crop_width = max(1, min(crop_width, img_w - crop_x))
    crop_height = max(1, min(crop_height, img_h - crop_y))

    print(f"[INFO] Cropping: x={crop_x}, y={crop_y}, w={crop_width}, h={crop_height}")

    # Crop the image
    cropped = img[crop_y:crop_y + crop_height, crop_x:crop_x + crop_width]

    if cropped.size == 0:
        raise ValueError("Crop resulted in empty image")

    # Apply color grading if requested
    if apply_grading:
        print("[INFO] Applying color grading")
        result = apply_color_grading(cropped)
    else:
        result = cropped

    # Apply logo overlay if path provided
    if logo_path:
        print(f"[INFO] Applying logo overlay from: {logo_path}")
        result = apply_logo_overlay(result, logo_path)

    # Save the result
    cv2.imwrite(output_path, result, [cv2.IMWRITE_JPEG_QUALITY, 92])
    print(f"[INFO] Saved custom thumbnail to: {output_path}")

    return output_path


def calculate_gesture_score(keypoints):
    """
    Calculate gesture/dynamism score based on arm positions.
    Returns: (score, details)
    """
    import numpy as np

    LEFT_SHOULDER, RIGHT_SHOULDER = 5, 6
    LEFT_ELBOW, RIGHT_ELBOW = 7, 8
    LEFT_WRIST, RIGHT_WRIST = 9, 10
    LEFT_HIP, RIGHT_HIP = 11, 12

    l_shoulder = keypoints[LEFT_SHOULDER]
    r_shoulder = keypoints[RIGHT_SHOULDER]
    l_elbow = keypoints[LEFT_ELBOW]
    r_elbow = keypoints[RIGHT_ELBOW]
    l_wrist = keypoints[LEFT_WRIST]
    r_wrist = keypoints[RIGHT_WRIST]
    l_hip = keypoints[LEFT_HIP]
    r_hip = keypoints[RIGHT_HIP]

    min_conf = 0.3
    has_shoulders = l_shoulder[2] > min_conf and r_shoulder[2] > min_conf
    has_left_arm = l_elbow[2] > min_conf and l_wrist[2] > min_conf
    has_right_arm = r_elbow[2] > min_conf and r_wrist[2] > min_conf
    has_hips = l_hip[2] > min_conf and r_hip[2] > min_conf

    if not has_shoulders or (not has_left_arm and not has_right_arm):
        return 0.5, {}

    shoulder_width = abs(r_shoulder[0] - l_shoulder[0])
    if shoulder_width < 10:
        shoulder_width = 100

    if has_hips:
        torso_height = abs((l_hip[1] + r_hip[1]) / 2 - (l_shoulder[1] + r_shoulder[1]) / 2)
    else:
        torso_height = shoulder_width * 1.5

    shoulder_y = (l_shoulder[1] + r_shoulder[1]) / 2

    # Arms raised score
    arms_raised = 0
    if has_left_arm:
        left_raise = (shoulder_y - l_wrist[1]) / torso_height
        arms_raised += max(0, min(1, left_raise + 0.2))
    if has_right_arm:
        right_raise = (shoulder_y - r_wrist[1]) / torso_height
        arms_raised += max(0, min(1, right_raise + 0.2))
    arms_raised = arms_raised / 2 if (has_left_arm and has_right_arm) else arms_raised
    arms_raised = min(1, arms_raised)

    # Arm spread score
    arm_spread = 0
    if has_left_arm and has_right_arm:
        wrist_spread = abs(r_wrist[0] - l_wrist[0])
        arm_spread = min(1, wrist_spread / (shoulder_width * 2.5))
    elif has_left_arm or has_right_arm:
        wrist = l_wrist if has_left_arm else r_wrist
        shoulder = l_shoulder if has_left_arm else r_shoulder
        extension = abs(wrist[0] - shoulder[0])
        arm_spread = min(1, extension / (shoulder_width * 1.5))

    # Gesturing score
    gesturing = 0
    if has_hips:
        hip_y = (l_hip[1] + r_hip[1]) / 2
        hip_x_left = l_hip[0]
        hip_x_right = r_hip[0]

        if has_left_arm:
            y_dist = abs(l_wrist[1] - hip_y) / torso_height
            x_dist = abs(l_wrist[0] - hip_x_left) / shoulder_width
            gesturing += min(1, max(y_dist, x_dist))
        if has_right_arm:
            y_dist = abs(r_wrist[1] - hip_y) / torso_height
            x_dist = abs(r_wrist[0] - hip_x_right) / shoulder_width
            gesturing += min(1, max(y_dist, x_dist))
        gesturing = gesturing / 2 if (has_left_arm and has_right_arm) else gesturing
    else:
        gesturing = arms_raised

    gesturing = min(1, gesturing)

    score = (arms_raised * 0.4) + (gesturing * 0.35) + (arm_spread * 0.25)
    return score, {'arms_raised': arms_raised, 'arm_spread': arm_spread, 'gesturing': gesturing}


def generate_thumbnail(video_path: str, start_time: str, end_time: str, title: str,
                       output_path: str, debug_path: str = None, logo_path: str = None):
    """
    Generate AI-powered thumbnail using YOLOv8 Pose for person detection.
    Uses facing direction, gesture scoring, and blur detection.
    """
    import cv2
    import numpy as np
    from ultralytics import YOLO
    from PIL import Image, ImageDraw
    import tempfile
    import os

    # Tuned parameters
    CONFIDENCE_THRESHOLD = 0.5
    MIN_ASPECT_RATIO = 1.15
    REQUIRE_FACING = True
    FACING_BONUS = 1.3
    GESTURE_BONUS = 1.5
    PITCH_THRESHOLD = 0.6
    PITCH_PENALTY = 0.6
    TORSO_CUTOFF = 0.65
    ANCHOR_X = 0.70
    OUTPUT_WIDTH = 1920
    OUTPUT_HEIGHT = 1080
    NUM_CANDIDATES = 60

    print(f"[INFO] Generating thumbnail for: {title}")

    start_sec = parse_time_to_seconds(start_time)
    end_sec = parse_time_to_seconds(end_time)
    duration = end_sec - start_sec

    offset = min(300, duration * 0.2)
    sample_start = start_sec + offset
    sample_end = start_sec + min(duration * 0.6, 900)
    sample_duration = sample_end - sample_start
    interval = sample_duration / NUM_CANDIDATES

    print(f"[INFO] Sampling frames from {offset/60:.1f}min to {(sample_end - start_sec)/60:.1f}min into sermon")

    # Use temp directory for frames
    temp_dir = tempfile.mkdtemp()

    # Extract frames for analysis
    frames = []
    for i in range(NUM_CANDIDATES):
        timestamp = sample_start + (i * interval)
        frame_path = os.path.join(temp_dir, f"frame_{i}.png")

        # Use PNG for lossless frame extraction at native resolution
        cmd = ["ffmpeg", "-ss", str(timestamp), "-i", video_path, "-vframes", "1", "-y", frame_path]
        result = subprocess.run(cmd, capture_output=True)

        if result.returncode == 0 and Path(frame_path).exists():
            frames.append((timestamp, frame_path))

    if not frames:
        print("[ERROR] No frames extracted for thumbnail")
        return False

    print(f"[INFO] Extracted {len(frames)} frames for thumbnail selection")

    # Load YOLOv8 Pose model (will download if not present)
    model = YOLO('yolov8n-pose.pt')

    # Track all detections
    all_detections = []
    best_frame_path = None
    best_bbox = None
    best_score = 0
    best_keypoints = None

    for timestamp, frame_path in frames:
        img = cv2.imread(frame_path)
        if img is None:
            continue

        img_h, img_w = img.shape[:2]

        # Run YOLO Pose detection
        results = model(img, conf=CONFIDENCE_THRESHOLD, verbose=False)

        for result in results:
            if result.boxes is not None and result.keypoints is not None:
                boxes = result.boxes
                keypoints_data = result.keypoints.data

                for i in range(len(boxes)):
                    box = boxes[i]
                    kps = keypoints_data[i].cpu().numpy()

                    conf = float(box.conf[0])
                    x1, y1, x2, y2 = [int(v) for v in box.xyxy[0].tolist()]
                    w = x2 - x1
                    h = y2 - y1
                    aspect_ratio = h / w if w > 0 else 0

                    # Estimate facing direction
                    facing, facing_conf = estimate_facing_direction(kps)
                    is_facing_camera = facing in ['front', 'front-left', 'front-right']

                    # Skip if not facing camera
                    if REQUIRE_FACING and not is_facing_camera:
                        all_detections.append((frame_path, (x1, y1, w, h), 0, "not_facing", f"facing={facing}", kps))
                        continue

                    # Skip sitting people
                    if aspect_ratio < MIN_ASPECT_RATIO:
                        all_detections.append((frame_path, (x1, y1, w, h), 0, "sitting", f"ar={aspect_ratio:.2f}", kps))
                        continue

                    # Calculate blur score
                    upper_body_h = int(h * TORSO_CUTOFF)
                    person_crop = img[y1:y1+upper_body_h, x1:x2]
                    if person_crop.size > 0:
                        blur_variance = get_blur_score(person_crop)
                        blur_score = normalize_blur_score(blur_variance)
                        sharpness_bonus = 0.5 + (blur_score * 0.5)
                    else:
                        sharpness_bonus = 0.75

                    # Standing bonus
                    if aspect_ratio > 1.5:
                        standing_bonus = 1.0
                    elif aspect_ratio > 1.3:
                        standing_bonus = 0.8
                    else:
                        standing_bonus = 0.5

                    # Center bonus
                    box_cx = (x1 + x2) / 2
                    center_dist = abs(box_cx - img_w / 2) / (img_w / 2)
                    center_bonus = 1 - center_dist * 0.3

                    # Facing bonus
                    facing_score = FACING_BONUS if is_facing_camera else 1.0

                    # Gesture scoring
                    gesture_score, gesture_details = calculate_gesture_score(kps)
                    gest_bonus = 1.0 + (GESTURE_BONUS - 1.0) * gesture_score

                    # Head pitch scoring (only for direct front facing)
                    pitch_ratio, _, can_estimate_pitch = estimate_head_pitch(kps)
                    pitch_bonus = 1.0
                    if can_estimate_pitch:
                        if facing == 'front':
                            # Direct facing - use normal threshold
                            if pitch_ratio > PITCH_THRESHOLD:
                                pitch_bonus = PITCH_PENALTY
                        elif facing in ['front-left', 'front-right']:
                            # Angled facing - use higher threshold and gentler penalty
                            if pitch_ratio > PITCH_THRESHOLD + 0.2:
                                pitch_bonus = PITCH_PENALTY + 0.2

                    # Final score
                    score = conf * standing_bonus * center_bonus * facing_score * gest_bonus * sharpness_bonus * pitch_bonus

                    details = f"s={score:.2f} ar={aspect_ratio:.1f} face={facing} gest={gesture_score:.2f} pitch={pitch_ratio:.2f}"

                    all_detections.append((frame_path, (x1, y1, w, h), score, "valid", details, kps))

                    if score > best_score:
                        best_score = score
                        best_frame_path = frame_path
                        best_bbox = (x1, y1, w, h)
                        best_keypoints = kps

    # Summary stats
    valid_count = sum(1 for d in all_detections if d[3] == "valid")
    not_facing_count = sum(1 for d in all_detections if d[3] == "not_facing")
    sitting_count = sum(1 for d in all_detections if d[3] == "sitting")
    print(f"[INFO] Detection summary: {valid_count} valid, {not_facing_count} not facing, {sitting_count} sitting")

    if best_frame_path is None:
        print("[WARN] No person detected, using center crop of middle frame")
        best_frame_path = frames[len(frames) // 2][1]
        img = cv2.imread(best_frame_path)
        img_h, img_w = img.shape[:2]
        best_bbox = (img_w // 4, img_h // 8, img_w // 2, int(img_h * 0.75))

    print(f"[INFO] Best frame selected with score {best_score:.3f}")

    img = cv2.imread(best_frame_path)
    img_h, img_w = img.shape[:2]
    x_person, y_person, w_person, h_person = best_bbox

    # Cropping calculations
    ASPECT_RATIO = 16 / 9
    y_bottom = y_person + (h_person * TORSO_CUTOFF)
    visible_height = h_person * TORSO_CUTOFF
    HEADROOM = 0.10
    h_crop = visible_height * (1 + HEADROOM)
    w_crop = h_crop * ASPECT_RATIO
    y_start = y_bottom - h_crop
    cx_person = x_person + w_person / 2
    x_start = cx_person - (w_crop * ANCHOR_X)

    # Clamp to bounds
    x_start = max(0, min(x_start, img_w - w_crop))
    y_start = max(0, min(y_start, img_h - h_crop))

    if w_crop > img_w or h_crop > img_h:
        if img_w / img_h > ASPECT_RATIO:
            h_crop = img_h
            w_crop = h_crop * ASPECT_RATIO
        else:
            w_crop = img_w
            h_crop = w_crop / ASPECT_RATIO
        x_start = (img_w - w_crop) / 2
        y_start = (img_h - h_crop) / 2

    x_start, y_start, w_crop, h_crop = int(x_start), int(y_start), int(w_crop), int(h_crop)

    if w_crop <= 0 or h_crop <= 0:
        x_start, y_start = 0, 0
        h_crop = img_h
        w_crop = int(h_crop * ASPECT_RATIO)
        if w_crop > img_w:
            w_crop = img_w
            h_crop = int(w_crop / ASPECT_RATIO)

    # Generate debug image if requested
    if debug_path:
        debug_img = img.copy()
        for det_frame_path, det_bbox, det_score, det_status, det_details, det_kps in all_detections:
            if det_frame_path != best_frame_path:
                continue

            dx, dy, dw, dh = det_bbox

            if det_status == "not_facing":
                color = (0, 0, 255)
                label = f"NOT FACING {det_details}"
            elif det_status == "sitting":
                color = (0, 165, 255)
                label = f"SITTING {det_details}"
            elif det_bbox == best_bbox:
                color = (0, 255, 0)
                label = f"WINNER {det_details}"
            else:
                color = (0, 255, 255)
                label = det_details

            # Draw bounding box
            cv2.rectangle(debug_img, (dx, dy), (dx + dw, dy + dh), color, 3)

            # Draw pose skeleton
            if det_kps is not None:
                skeleton_thickness = 3 if det_bbox == best_bbox else 2
                draw_skeleton(debug_img, det_kps, color, skeleton_thickness)

            # Draw label
            font = cv2.FONT_HERSHEY_SIMPLEX
            font_scale = 0.6
            thickness = 2
            (text_w, text_h), _ = cv2.getTextSize(label, font, font_scale, thickness)
            cv2.rectangle(debug_img, (dx, dy - text_h - 10), (dx + text_w + 4, dy), color, -1)
            cv2.putText(debug_img, label, (dx + 2, dy - 5), font, font_scale, (0, 0, 0), thickness)

        cv2.rectangle(debug_img, (x_start, y_start), (x_start + w_crop, y_start + h_crop), (255, 0, 0), 3)
        cv2.imwrite(debug_path, debug_img, [cv2.IMWRITE_JPEG_QUALITY, 85])
        print(f"[INFO] Debug image saved to {debug_path}")

    # Crop (keep native resolution)
    cropped = img[y_start:y_start+h_crop, x_start:x_start+w_crop]
    if cropped.size == 0:
        cropped = img

    print(f"[INFO] Cropped to {cropped.shape[1]}x{cropped.shape[0]} (native resolution)")
    resized = cropped  # Keep native resolution

    # Color grading
    lab = cv2.cvtColor(resized, cv2.COLOR_BGR2LAB)
    l, a, b = cv2.split(lab)
    clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
    l = clahe.apply(l)
    l = np.clip(l.astype(np.float32) * 1.02 + 2, 0, 255).astype(np.uint8)

    # Teal/orange color grade
    a = a.astype(np.float32)
    b = b.astype(np.float32)
    l_norm = l.astype(np.float32) / 255.0
    shadow_mask = 1.0 - l_norm
    highlight_mask = l_norm
    a = a - (shadow_mask * 4) + (highlight_mask * 3)
    b = b - (shadow_mask * 6) + (highlight_mask * 5)
    a = np.clip(a, 0, 255).astype(np.uint8)
    b = np.clip(b, 0, 255).astype(np.uint8)
    graded = cv2.merge([l, a, b])
    resized = cv2.cvtColor(graded, cv2.COLOR_LAB2BGR)

    # Vibrance boost
    hsv = cv2.cvtColor(resized, cv2.COLOR_BGR2HSV).astype(np.float32)
    h, s, v = cv2.split(hsv)
    saturation_boost = 1.12
    s = s * (1 + (1 - s / 255) * (saturation_boost - 1) * 0.5)
    s = np.clip(s, 0, 255)
    hsv = cv2.merge([h, s, v])
    resized = cv2.cvtColor(hsv.astype(np.uint8), cv2.COLOR_HSV2BGR)

    # Subtle vignette
    rows, cols = resized.shape[:2]
    X = cv2.getGaussianKernel(cols, cols * 0.7)
    Y = cv2.getGaussianKernel(rows, rows * 0.7)
    vignette = Y * X.T
    vignette = vignette / vignette.max()
    vignette = (vignette * 0.25 + 0.75)
    for i in range(3):
        resized[:, :, i] = (resized[:, :, i] * vignette).astype(np.uint8)

    # Logo overlay with gradient
    img_pil = Image.fromarray(cv2.cvtColor(resized, cv2.COLOR_BGR2RGB))
    draw = ImageDraw.Draw(img_pil, 'RGBA')
    img_height, img_width = resized.shape[:2]

    padding = 150
    logo_size = int(min(img_width, img_height) * 0.26)  # Scale logo based on image size

    logo = None
    logo_w, logo_h = 0, 0
    if logo_path and Path(logo_path).exists():
        try:
            logo = Image.open(logo_path).convert("RGBA")
            aspect = logo.width / logo.height
            if aspect >= 1:
                logo_w = logo_size
                logo_h = int(logo_size / aspect)
            else:
                logo_h = logo_size
                logo_w = int(logo_size * aspect)
            logo = logo.resize((logo_w, logo_h), Image.LANCZOS)
        except Exception as e:
            print(f"[WARN] Could not load logo: {e}")

    if logo:
        content_h = logo_h
        start_y = (img_height - content_h) // 2
        content_w = logo_w

        # Slanted gradient (scaled to image size)
        slant_amount = int(img_width * 0.18)
        fade_length = int(img_width * 0.36)
        solid_region = int(img_width * 0.08)

        y_coords = np.arange(img_height).reshape(-1, 1)
        x_coords = np.arange(img_width).reshape(1, -1)
        fade_starts = (solid_region + (y_coords / img_height) * slant_amount).astype(np.float32)
        alpha = np.zeros((img_height, img_width), dtype=np.float32)
        alpha = np.where(x_coords < fade_starts, 220, alpha)
        mask = (x_coords >= fade_starts) & (x_coords < fade_starts + fade_length)
        progress = np.clip((x_coords - fade_starts) / fade_length, 0, 1)
        alpha = np.where(mask, 220 * (1 - progress), alpha)

        gradient_arr = np.zeros((img_height, img_width, 4), dtype=np.uint8)
        gradient_arr[:, :, 3] = alpha.astype(np.uint8)
        gradient = Image.fromarray(gradient_arr, 'RGBA')
        img_pil.paste(gradient, (0, 0), gradient)

        center_x = (content_w + padding * 2) // 2
        logo_y = start_y
        logo_x = center_x - logo_w // 2
        img_pil.paste(logo, (logo_x, logo_y), logo)

    final_img = cv2.cvtColor(np.array(img_pil), cv2.COLOR_RGB2BGR)
    cv2.imwrite(output_path, final_img, [cv2.IMWRITE_JPEG_QUALITY, 92])

    # Clean up temp frame files
    import shutil
    shutil.rmtree(temp_dir, ignore_errors=True)

    print(f"[INFO] Generated thumbnail saved to {output_path}")
    return True


def generate_thumbnail_options(video_path: str, start_time: str, end_time: str, title: str,
                                output_dir: str, count: int = 10, logo_path: str = None):
    """
    Generate multiple thumbnail options by selecting top scoring frames.
    Returns list of generated thumbnail paths.
    """
    import cv2
    import numpy as np
    from ultralytics import YOLO
    from PIL import Image, ImageDraw
    import tempfile
    import os

    # Same parameters as generate_thumbnail
    CONFIDENCE_THRESHOLD = 0.5
    MIN_ASPECT_RATIO = 1.15
    REQUIRE_FACING = True
    FACING_BONUS = 1.3
    GESTURE_BONUS = 1.5
    PITCH_THRESHOLD = 0.6
    PITCH_PENALTY = 0.6
    TORSO_CUTOFF = 0.65
    ANCHOR_X = 0.70
    OUTPUT_WIDTH = 1920
    OUTPUT_HEIGHT = 1080
    NUM_CANDIDATES = 60

    print(f"[INFO] Generating {count} thumbnail options for: {title}")

    start_sec = parse_time_to_seconds(start_time)
    end_sec = parse_time_to_seconds(end_time)
    duration = end_sec - start_sec

    offset = min(300, duration * 0.2)
    sample_start = start_sec + offset
    sample_end = start_sec + min(duration * 0.6, 900)
    sample_duration = sample_end - sample_start
    interval = sample_duration / NUM_CANDIDATES

    print(f"[INFO] Sampling frames from {offset/60:.1f}min to {(sample_end - start_sec)/60:.1f}min into sermon")

    # Save candidate frames to a permanent folder for manual tuning
    output_dir_path = Path(output_dir)
    candidates_dir = output_dir_path / "candidates"
    candidates_dir.mkdir(parents=True, exist_ok=True)

    frames = []
    for i in range(NUM_CANDIDATES):
        timestamp = sample_start + (i * interval)
        frame_path = str(candidates_dir / f"frame_{i:03d}_{int(timestamp)}s.png")

        # Use PNG for lossless frame extraction at native resolution
        cmd = ["ffmpeg", "-ss", str(timestamp), "-i", video_path, "-vframes", "1", "-y", frame_path]
        result = subprocess.run(cmd, capture_output=True)

        if result.returncode == 0 and Path(frame_path).exists():
            frames.append((timestamp, frame_path))

    if not frames:
        print("[ERROR] No frames extracted for thumbnail")
        return []

    print(f"[INFO] Extracted {len(frames)} frames for thumbnail selection")

    model = YOLO('yolov8n-pose.pt')

    # Collect all valid detections with their scores
    valid_detections = []

    for timestamp, frame_path in frames:
        img = cv2.imread(frame_path)
        if img is None:
            continue

        img_h, img_w = img.shape[:2]

        results = model(img, conf=CONFIDENCE_THRESHOLD, verbose=False)

        for result in results:
            if result.boxes is not None and result.keypoints is not None:
                boxes = result.boxes
                keypoints_data = result.keypoints.data

                for i in range(len(boxes)):
                    box = boxes[i]
                    kps = keypoints_data[i].cpu().numpy()

                    conf = float(box.conf[0])
                    x1, y1, x2, y2 = [int(v) for v in box.xyxy[0].tolist()]
                    w = x2 - x1
                    h = y2 - y1
                    aspect_ratio = h / w if w > 0 else 0

                    facing, facing_conf = estimate_facing_direction(kps)
                    is_facing_camera = facing in ['front', 'front-left', 'front-right']

                    if REQUIRE_FACING and not is_facing_camera:
                        continue

                    if aspect_ratio < MIN_ASPECT_RATIO:
                        continue

                    upper_body_h = int(h * TORSO_CUTOFF)
                    person_crop = img[y1:y1+upper_body_h, x1:x2]
                    if person_crop.size > 0:
                        blur_variance = get_blur_score(person_crop)
                        blur_score = normalize_blur_score(blur_variance)
                        sharpness_bonus = 0.5 + (blur_score * 0.5)
                    else:
                        sharpness_bonus = 0.75

                    if aspect_ratio > 1.5:
                        standing_bonus = 1.0
                    elif aspect_ratio > 1.3:
                        standing_bonus = 0.8
                    else:
                        standing_bonus = 0.5

                    box_cx = (x1 + x2) / 2
                    center_dist = abs(box_cx - img_w / 2) / (img_w / 2)
                    center_bonus = 1 - center_dist * 0.3

                    facing_score = FACING_BONUS if is_facing_camera else 1.0

                    gesture_score, gesture_details = calculate_gesture_score(kps)
                    gest_bonus = 1.0 + (GESTURE_BONUS - 1.0) * gesture_score

                    # Head pitch scoring (only for direct front facing)
                    pitch_ratio, _, can_estimate_pitch = estimate_head_pitch(kps)
                    pitch_bonus = 1.0
                    if can_estimate_pitch:
                        if facing == 'front':
                            if pitch_ratio > PITCH_THRESHOLD:
                                pitch_bonus = PITCH_PENALTY
                        elif facing in ['front-left', 'front-right']:
                            if pitch_ratio > PITCH_THRESHOLD + 0.2:
                                pitch_bonus = PITCH_PENALTY + 0.2

                    score = conf * standing_bonus * center_bonus * facing_score * gest_bonus * sharpness_bonus * pitch_bonus

                    valid_detections.append({
                        'frame_path': frame_path,
                        'img': img.copy(),
                        'bbox': (x1, y1, w, h),
                        'score': score,
                        'keypoints': kps,
                        'img_shape': (img_h, img_w)
                    })

    if not valid_detections:
        print("[WARN] No valid detections found")
        print(f"[INFO] Candidate frames saved to: {candidates_dir}")
        return []

    # Sort by score and get top candidates
    valid_detections.sort(key=lambda x: x['score'], reverse=True)

    # Filter to get diverse frames (avoid very similar frames)
    selected = []
    used_frames = set()

    for det in valid_detections:
        if len(selected) >= count:
            break
        # Skip if we already have a detection from this frame
        if det['frame_path'] in used_frames:
            continue
        selected.append(det)
        used_frames.add(det['frame_path'])

    print(f"[INFO] Selected {len(selected)} diverse thumbnail candidates")

    # Generate thumbnails for selected frames
    output_paths = []

    for idx, det in enumerate(selected):
        img = det['img']
        img_h, img_w = det['img_shape']
        x_person, y_person, w_person, h_person = det['bbox']

        ASPECT_RATIO = 16 / 9
        y_bottom = y_person + (h_person * TORSO_CUTOFF)
        visible_height = h_person * TORSO_CUTOFF
        HEADROOM = 0.10
        h_crop = visible_height * (1 + HEADROOM)
        w_crop = h_crop * ASPECT_RATIO
        y_start = y_bottom - h_crop
        cx_person = x_person + w_person / 2
        x_start = cx_person - (w_crop * ANCHOR_X)

        x_start = max(0, min(x_start, img_w - w_crop))
        y_start = max(0, min(y_start, img_h - h_crop))

        if w_crop > img_w or h_crop > img_h:
            if img_w / img_h > ASPECT_RATIO:
                h_crop = img_h
                w_crop = h_crop * ASPECT_RATIO
            else:
                w_crop = img_w
                h_crop = w_crop / ASPECT_RATIO
            x_start = (img_w - w_crop) / 2
            y_start = (img_h - h_crop) / 2

        x_start, y_start, w_crop, h_crop = int(x_start), int(y_start), int(w_crop), int(h_crop)

        if w_crop <= 0 or h_crop <= 0:
            x_start, y_start = 0, 0
            h_crop = img_h
            w_crop = int(h_crop * ASPECT_RATIO)
            if w_crop > img_w:
                w_crop = img_w
                h_crop = int(w_crop / ASPECT_RATIO)

        cropped = img[y_start:y_start+h_crop, x_start:x_start+w_crop]
        if cropped.size == 0:
            cropped = img

        # Save RAW cropped image (no color grading, no logo) for editor use
        raw_path = output_dir_path / f"thumbnail_option_{idx + 1}_raw.jpg"
        cv2.imwrite(str(raw_path), cropped, [cv2.IMWRITE_JPEG_QUALITY, 95])

        # Generate debug image for this option
        debug_img = img.copy()
        # Draw the bounding box
        cv2.rectangle(debug_img, (x_person, y_person), (x_person + w_person, y_person + h_person), (0, 255, 0), 3)
        # Draw pose skeleton
        if det['keypoints'] is not None:
            draw_skeleton(debug_img, det['keypoints'], (0, 255, 0), 3)
        # Draw crop rectangle
        cv2.rectangle(debug_img, (x_start, y_start), (x_start + w_crop, y_start + h_crop), (255, 0, 0), 3)
        # Add score label
        label = f"Score: {det['score']:.2f}"
        font = cv2.FONT_HERSHEY_SIMPLEX
        cv2.putText(debug_img, label, (x_person + 5, y_person + 25), font, 0.8, (0, 255, 0), 2)
        # Save debug image
        debug_path = output_dir_path / f"thumbnail_option_{idx + 1}_debug.jpg"
        cv2.imwrite(str(debug_path), debug_img, [cv2.IMWRITE_JPEG_QUALITY, 85])

        resized = cropped  # Keep native resolution

        # Apply color grading
        lab = cv2.cvtColor(resized, cv2.COLOR_BGR2LAB)
        l, a, b = cv2.split(lab)
        clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
        l = clahe.apply(l)
        l = np.clip(l.astype(np.float32) * 1.02 + 2, 0, 255).astype(np.uint8)

        a = a.astype(np.float32)
        b = b.astype(np.float32)
        l_norm = l.astype(np.float32) / 255.0
        shadow_mask = 1.0 - l_norm
        highlight_mask = l_norm
        a = a - (shadow_mask * 4) + (highlight_mask * 3)
        b = b - (shadow_mask * 6) + (highlight_mask * 5)
        a = np.clip(a, 0, 255).astype(np.uint8)
        b = np.clip(b, 0, 255).astype(np.uint8)
        graded = cv2.merge([l, a, b])
        resized = cv2.cvtColor(graded, cv2.COLOR_LAB2BGR)

        hsv = cv2.cvtColor(resized, cv2.COLOR_BGR2HSV).astype(np.float32)
        h, s, v = cv2.split(hsv)
        saturation_boost = 1.12
        s = s * (1 + (1 - s / 255) * (saturation_boost - 1) * 0.5)
        s = np.clip(s, 0, 255)
        hsv = cv2.merge([h, s, v])
        resized = cv2.cvtColor(hsv.astype(np.uint8), cv2.COLOR_HSV2BGR)

        rows, cols = resized.shape[:2]
        X = cv2.getGaussianKernel(cols, cols * 0.7)
        Y = cv2.getGaussianKernel(rows, rows * 0.7)
        vignette = Y * X.T
        vignette = vignette / vignette.max()
        vignette = (vignette * 0.25 + 0.75)
        for i in range(3):
            resized[:, :, i] = (resized[:, :, i] * vignette).astype(np.uint8)

        # Logo overlay
        img_pil = Image.fromarray(cv2.cvtColor(resized, cv2.COLOR_BGR2RGB))
        draw = ImageDraw.Draw(img_pil, 'RGBA')
        img_height, img_width = resized.shape[:2]

        padding = 150
        logo_size = int(min(img_width, img_height) * 0.26)  # Scale logo based on image size

        logo = None
        logo_w, logo_h = 0, 0
        if logo_path and Path(logo_path).exists():
            try:
                logo = Image.open(logo_path).convert("RGBA")
                aspect = logo.width / logo.height
                if aspect >= 1:
                    logo_w = logo_size
                    logo_h = int(logo_size / aspect)
                else:
                    logo_h = logo_size
                    logo_w = int(logo_size * aspect)
                logo = logo.resize((logo_w, logo_h), Image.LANCZOS)
            except Exception as e:
                print(f"[WARN] Could not load logo: {e}")

        if logo:
            content_h = logo_h
            start_y = (img_height - content_h) // 2
            content_w = logo_w

            slant_amount = int(img_width * 0.18)
            fade_length = int(img_width * 0.36)
            solid_region = int(img_width * 0.08)

            y_coords = np.arange(img_height).reshape(-1, 1)
            x_coords = np.arange(img_width).reshape(1, -1)
            fade_starts = (solid_region + (y_coords / img_height) * slant_amount).astype(np.float32)
            alpha = np.zeros((img_height, img_width), dtype=np.float32)
            alpha = np.where(x_coords < fade_starts, 220, alpha)
            mask = (x_coords >= fade_starts) & (x_coords < fade_starts + fade_length)
            progress = np.clip((x_coords - fade_starts) / fade_length, 0, 1)
            alpha = np.where(mask, 220 * (1 - progress), alpha)

            gradient_arr = np.zeros((img_height, img_width, 4), dtype=np.uint8)
            gradient_arr[:, :, 3] = alpha.astype(np.uint8)
            gradient = Image.fromarray(gradient_arr, 'RGBA')
            img_pil.paste(gradient, (0, 0), gradient)

            center_x = (content_w + padding * 2) // 2
            logo_y = start_y
            logo_x = center_x - logo_w // 2
            img_pil.paste(logo, (logo_x, logo_y), logo)

        final_img = cv2.cvtColor(np.array(img_pil), cv2.COLOR_RGB2BGR)
        output_path = output_dir_path / f"thumbnail_option_{idx + 1}.jpg"
        cv2.imwrite(str(output_path), final_img, [cv2.IMWRITE_JPEG_QUALITY, 92])
        output_paths.append(str(output_path))
        print(f"[INFO] Generated thumbnail option {idx + 1}: {output_path}")

    print(f"[INFO] Generated {len(output_paths)} thumbnail options")
    print(f"[INFO] Candidate frames saved to: {candidates_dir}")
    return output_paths


def main():
    # Check for old-style arguments BEFORE parsing (for backwards compatibility)
    # Old style: --video ... --start ... --end ... --output ...
    # New style: generate --video ... OR custom --source ...
    use_old_style = '--video' in sys.argv and sys.argv[1] != 'generate' and sys.argv[1] != 'custom'

    if use_old_style:
        # Use old-style parser for backwards compatibility
        parser = argparse.ArgumentParser(description='Generate AI-powered thumbnail using YOLOv8 Pose')
        parser.add_argument('--video', required=True, help='Path to input video file')
        parser.add_argument('--start', required=True, help='Start time (HH:MM:SS or MM:SS)')
        parser.add_argument('--end', required=True, help='End time (HH:MM:SS or MM:SS)')
        parser.add_argument('--title', default='', help='Video title (for logging)')
        parser.add_argument('--output', required=True, help='Output thumbnail path')
        parser.add_argument('--debug', help='Optional debug image output path')
        parser.add_argument('--logo', help='Optional logo image path for overlay')
        parser.add_argument('--count', type=int, default=1, help='Number of thumbnail options to generate')
        args = parser.parse_args()
        args.command = 'generate'
    else:
        # Use new-style parser with subcommands
        parser = argparse.ArgumentParser(description='Thumbnail generation tools')
        subparsers = parser.add_subparsers(dest='command', help='Command to run')

        # Generate command (original functionality)
        gen_parser = subparsers.add_parser('generate', help='Generate AI-powered thumbnail from video')
        gen_parser.add_argument('--video', required=True, help='Path to input video file')
        gen_parser.add_argument('--start', required=True, help='Start time (HH:MM:SS or MM:SS)')
        gen_parser.add_argument('--end', required=True, help='End time (HH:MM:SS or MM:SS)')
        gen_parser.add_argument('--title', default='', help='Video title (for logging)')
        gen_parser.add_argument('--output', required=True, help='Output thumbnail path (or directory if --count > 1)')
        gen_parser.add_argument('--debug', help='Optional debug image output path')
        gen_parser.add_argument('--logo', help='Optional logo image path for overlay')
        gen_parser.add_argument('--count', type=int, default=1, help='Number of thumbnail options to generate')

        # Process custom thumbnail command
        custom_parser = subparsers.add_parser('custom', help='Process a custom image as thumbnail')
        custom_parser.add_argument('--source', required=True, help='Path to source image')
        custom_parser.add_argument('--output', required=True, help='Output thumbnail path')
        custom_parser.add_argument('--crop-x', type=int, default=0, help='Crop X coordinate')
        custom_parser.add_argument('--crop-y', type=int, default=0, help='Crop Y coordinate')
        custom_parser.add_argument('--crop-width', type=int, required=True, help='Crop width')
        custom_parser.add_argument('--crop-height', type=int, required=True, help='Crop height')
        custom_parser.add_argument('--no-grading', action='store_true', help='Disable color grading')
        custom_parser.add_argument('--logo', help='Optional logo image path for overlay')

        args = parser.parse_args()

        if args.command is None:
            parser.print_help()
            sys.exit(1)

    if args.command == 'generate':
        if not Path(args.video).exists():
            print(f"[ERROR] Video file not found: {args.video}")
            sys.exit(1)

        if args.count > 1:
            output_dir = Path(args.output).parent if Path(args.output).suffix else Path(args.output)
            output_dir.mkdir(parents=True, exist_ok=True)

            paths = generate_thumbnail_options(
                video_path=args.video,
                start_time=args.start,
                end_time=args.end,
                title=args.title,
                output_dir=str(output_dir),
                count=args.count,
                logo_path=args.logo
            )

            if paths:
                import json
                print(f"[OUTPUT] {json.dumps(paths)}")
                sys.exit(0)
            else:
                sys.exit(1)
        else:
            success = generate_thumbnail(
                video_path=args.video,
                start_time=args.start,
                end_time=args.end,
                title=args.title,
                output_path=args.output,
                debug_path=args.debug,
                logo_path=args.logo
            )
            sys.exit(0 if success else 1)

    elif args.command == 'custom':
        if not Path(args.source).exists():
            print(f"[ERROR] Source image not found: {args.source}")
            sys.exit(1)

        try:
            output_path = process_custom_thumbnail(
                source_path=args.source,
                output_path=args.output,
                crop_x=args.crop_x,
                crop_y=args.crop_y,
                crop_width=args.crop_width,
                crop_height=args.crop_height,
                apply_grading=not args.no_grading,
                logo_path=args.logo
            )
            print(f"[OUTPUT] {output_path}")
            sys.exit(0)
        except Exception as e:
            print(f"[ERROR] {e}")
            sys.exit(1)


if __name__ == '__main__':
    main()
