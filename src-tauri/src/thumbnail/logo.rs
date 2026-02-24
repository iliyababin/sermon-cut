use image::RgbImage;
use std::path::Path;

/// Apply logo overlay with slanted gradient to the left side of the image.
pub fn apply_logo_overlay(img: &mut RgbImage, logo_path: &str) {
    let path = Path::new(logo_path);
    if !path.exists() {
        println!("[logo] Logo file not found: {}", logo_path);
        return;
    }

    let logo_img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            println!("[logo] Could not load logo: {}", e);
            return;
        }
    };

    let img_w = img.width();
    let img_h = img.height();

    let padding = 150u32;
    let logo_size = ((img_w.min(img_h) as f64) * 0.26) as u32;

    // Resize logo preserving aspect ratio
    let aspect = logo_img.width() as f64 / logo_img.height() as f64;
    let (logo_w, logo_h) = if aspect >= 1.0 {
        (logo_size, (logo_size as f64 / aspect) as u32)
    } else {
        ((logo_size as f64 * aspect) as u32, logo_size)
    };

    if logo_w == 0 || logo_h == 0 {
        return;
    }

    let resized_logo = image::imageops::resize(
        &logo_img,
        logo_w,
        logo_h,
        image::imageops::FilterType::Lanczos3,
    );

    // --- Composite gradient overlay ---
    let slant_amount = (img_w as f64 * 0.18) as f64;
    let fade_length = (img_w as f64 * 0.36) as f64;
    let solid_region = (img_w as f64 * 0.08) as f64;

    for y in 0..img_h {
        let y_frac = y as f64 / img_h as f64;
        let fade_start = solid_region + y_frac * slant_amount;

        for x in 0..img_w {
            let xf = x as f64;
            let alpha = if xf < fade_start {
                220.0
            } else if xf < fade_start + fade_length {
                let progress = (xf - fade_start) / fade_length;
                220.0 * (1.0 - progress)
            } else {
                0.0
            };

            if alpha > 0.0 {
                let a = (alpha / 255.0) as f32;
                let p = img.get_pixel(x, y);
                // Composite black overlay at given alpha
                let r = (p[0] as f32 * (1.0 - a)).min(255.0) as u8;
                let g = (p[1] as f32 * (1.0 - a)).min(255.0) as u8;
                let b = (p[2] as f32 * (1.0 - a)).min(255.0) as u8;
                img.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
    }

    // --- Composite logo ---
    let content_w = logo_w;
    let start_y = (img_h.saturating_sub(logo_h)) / 2;
    let center_x = (content_w + padding * 2) / 2;
    let logo_x = center_x.saturating_sub(logo_w / 2);
    let logo_y = start_y;

    for ly in 0..logo_h {
        for lx in 0..logo_w {
            let dx = logo_x + lx;
            let dy = logo_y + ly;
            if dx >= img_w || dy >= img_h {
                continue;
            }
            let lp = resized_logo.get_pixel(lx, ly);
            let la = lp[3] as f32 / 255.0;
            if la <= 0.0 {
                continue;
            }
            let bg = img.get_pixel(dx, dy);
            let r = (lp[0] as f32 * la + bg[0] as f32 * (1.0 - la)).min(255.0) as u8;
            let g = (lp[1] as f32 * la + bg[1] as f32 * (1.0 - la)).min(255.0) as u8;
            let b = (lp[2] as f32 * la + bg[2] as f32 * (1.0 - la)).min(255.0) as u8;
            img.put_pixel(dx, dy, image::Rgb([r, g, b]));
        }
    }
}
