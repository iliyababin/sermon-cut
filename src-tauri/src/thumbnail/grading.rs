use image::RgbImage;

use super::color;

/// Apply cinematic colour grading in-place: CLAHE → teal/orange → vibrance → vignette.
pub fn apply_color_grading(img: &mut RgbImage) {
    let (w, h) = (img.width() as usize, img.height() as usize);

    // --- Convert to LAB ---
    let mut l_ch = vec![0u8; w * h];
    let mut a_ch = vec![0u8; w * h];
    let mut b_ch = vec![0u8; w * h];

    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x as u32, y as u32);
            let (l, a, b) = color::rgb_to_lab(p[0], p[1], p[2]);
            let idx = y * w + x;
            l_ch[idx] = l;
            a_ch[idx] = a;
            b_ch[idx] = b;
        }
    }

    // --- Stage 1: CLAHE on L channel ---
    clahe_inplace(&mut l_ch, w, h, 2.0, 8);

    // L = L * 1.02 + 2
    for v in l_ch.iter_mut() {
        let f = *v as f32 * 1.02 + 2.0;
        *v = f.clamp(0.0, 255.0) as u8;
    }

    // --- Stage 2: Teal / orange colour shift in LAB ---
    for i in 0..(w * h) {
        let l_norm = l_ch[i] as f32 / 255.0;
        let shadow_mask = 1.0 - l_norm;
        let highlight_mask = l_norm;

        let a_f = a_ch[i] as f32 - shadow_mask * 4.0 + highlight_mask * 3.0;
        let b_f = b_ch[i] as f32 - shadow_mask * 6.0 + highlight_mask * 5.0;
        a_ch[i] = a_f.clamp(0.0, 255.0) as u8;
        b_ch[i] = b_f.clamp(0.0, 255.0) as u8;
    }

    // --- Convert LAB → RGB ---
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let (r, g, b) = color::lab_to_rgb(l_ch[idx], a_ch[idx], b_ch[idx]);
            img.put_pixel(x as u32, y as u32, image::Rgb([r, g, b]));
        }
    }

    // --- Stage 3: Vibrance boost in HSV ---
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x as u32, y as u32);
            let (hh, ss, vv) = color::rgb_to_hsv(p[0], p[1], p[2]);
            let s_f = ss as f32;
            let saturation_boost = 1.12_f32;
            let new_s = s_f * (1.0 + (1.0 - s_f / 255.0) * (saturation_boost - 1.0) * 0.5);
            let new_s = new_s.clamp(0.0, 255.0) as u8;
            let (r, g, b) = color::hsv_to_rgb(hh, new_s, vv);
            img.put_pixel(x as u32, y as u32, image::Rgb([r, g, b]));
        }
    }

    // --- Stage 4: Vignette ---
    apply_vignette(img);
}

/// Gaussian vignette: dims edges, range [0.75, 1.0].
fn apply_vignette(img: &mut RgbImage) {
    let (w, h) = (img.width() as usize, img.height() as usize);

    // 1-D Gaussian kernels
    let sigma_x = w as f64 * 0.7;
    let sigma_y = h as f64 * 0.7;

    let kernel_x: Vec<f64> = (0..w)
        .map(|x| {
            let d = x as f64 - w as f64 / 2.0;
            (-d * d / (2.0 * sigma_x * sigma_x)).exp()
        })
        .collect();

    let kernel_y: Vec<f64> = (0..h)
        .map(|y| {
            let d = y as f64 - h as f64 / 2.0;
            (-d * d / (2.0 * sigma_y * sigma_y)).exp()
        })
        .collect();

    // Find max of outer product to normalise
    let mut max_val = 0.0_f64;
    for ky in &kernel_y {
        for kx in &kernel_x {
            let v = ky * kx;
            if v > max_val {
                max_val = v;
            }
        }
    }
    if max_val <= 0.0 {
        return;
    }

    for y in 0..h {
        let ky = kernel_y[y];
        for x in 0..w {
            let v = (ky * kernel_x[x]) / max_val;
            let factor = (v * 0.25 + 0.75) as f32; // [0.75, 1.0]
            let p = img.get_pixel(x as u32, y as u32);
            let r = (p[0] as f32 * factor).min(255.0) as u8;
            let g = (p[1] as f32 * factor).min(255.0) as u8;
            let b = (p[2] as f32 * factor).min(255.0) as u8;
            img.put_pixel(x as u32, y as u32, image::Rgb([r, g, b]));
        }
    }
}

// ----- CLAHE implementation -----

/// Contrast-Limited Adaptive Histogram Equalisation on a single channel.
fn clahe_inplace(data: &mut [u8], width: usize, height: usize, clip_limit: f64, grid_size: usize) {
    if width == 0 || height == 0 || grid_size == 0 {
        return;
    }

    let tile_w = width / grid_size;
    let tile_h = height / grid_size;
    if tile_w == 0 || tile_h == 0 {
        return;
    }

    let num_tiles_x = grid_size;
    let num_tiles_y = grid_size;

    // Build CDF lookup for each tile
    let mut cdfs = vec![vec![0u8; 256]; num_tiles_x * num_tiles_y];

    for ty in 0..num_tiles_y {
        for tx in 0..num_tiles_x {
            let x0 = tx * tile_w;
            let y0 = ty * tile_h;
            let x1 = if tx == num_tiles_x - 1 { width } else { x0 + tile_w };
            let y1 = if ty == num_tiles_y - 1 { height } else { y0 + tile_h };

            let tile_pixels = (x1 - x0) * (y1 - y0);
            if tile_pixels == 0 {
                // Identity mapping
                for i in 0..256 {
                    cdfs[ty * num_tiles_x + tx][i] = i as u8;
                }
                continue;
            }

            // Histogram
            let mut hist = [0u32; 256];
            for iy in y0..y1 {
                for ix in x0..x1 {
                    hist[data[iy * width + ix] as usize] += 1;
                }
            }

            // Clip and redistribute
            let clip_count = (clip_limit * tile_pixels as f64 / 256.0) as u32;
            let clip_count = clip_count.max(1);
            let mut excess = 0u32;
            for h in hist.iter_mut() {
                if *h > clip_count {
                    excess += *h - clip_count;
                    *h = clip_count;
                }
            }
            let redist = excess / 256;
            let residual = (excess % 256) as usize;
            for (i, h) in hist.iter_mut().enumerate() {
                *h += redist;
                if i < residual {
                    *h += 1;
                }
            }

            // CDF
            let mut cdf = [0u32; 256];
            cdf[0] = hist[0];
            for i in 1..256 {
                cdf[i] = cdf[i - 1] + hist[i];
            }
            let cdf_min = cdf.iter().copied().find(|&v| v > 0).unwrap_or(0);
            let denom = tile_pixels.saturating_sub(cdf_min as usize).max(1) as f64;

            let lut = &mut cdfs[ty * num_tiles_x + tx];
            for i in 0..256 {
                lut[i] = ((cdf[i].saturating_sub(cdf_min) as f64 / denom) * 255.0)
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }

    // Apply with bilinear interpolation between adjacent tile CDFs
    for y in 0..height {
        let fy = (y as f64 / tile_h as f64 - 0.5).clamp(0.0, (num_tiles_y - 1) as f64);
        let ty0 = fy.floor() as usize;
        let ty1 = (ty0 + 1).min(num_tiles_y - 1);
        let wy = fy - ty0 as f64;

        for x in 0..width {
            let fx = (x as f64 / tile_w as f64 - 0.5).clamp(0.0, (num_tiles_x - 1) as f64);
            let tx0 = fx.floor() as usize;
            let tx1 = (tx0 + 1).min(num_tiles_x - 1);
            let wx = fx - tx0 as f64;

            let val = data[y * width + x] as usize;

            let c00 = cdfs[ty0 * num_tiles_x + tx0][val] as f64;
            let c10 = cdfs[ty0 * num_tiles_x + tx1][val] as f64;
            let c01 = cdfs[ty1 * num_tiles_x + tx0][val] as f64;
            let c11 = cdfs[ty1 * num_tiles_x + tx1][val] as f64;

            let interp = c00 * (1.0 - wx) * (1.0 - wy)
                + c10 * wx * (1.0 - wy)
                + c01 * (1.0 - wx) * wy
                + c11 * wx * wy;

            data[y * width + x] = interp.clamp(0.0, 255.0) as u8;
        }
    }
}
