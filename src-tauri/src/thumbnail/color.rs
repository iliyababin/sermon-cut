/// RGB ↔ LAB and RGB ↔ HSV conversions.
///
/// LAB uses OpenCV-compatible ranges: L [0,255], a [0,255], b [0,255]
/// where 128 is the neutral point for a and b.
/// HSV uses OpenCV-compatible ranges: H [0,180), S [0,255], V [0,255].

/// Convert sRGB [0,255] to CIE-LAB (OpenCV ranges: L 0-255, a 0-255, b 0-255).
pub fn rgb_to_lab(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    // sRGB to linear RGB
    let r_lin = srgb_to_linear(r as f64 / 255.0);
    let g_lin = srgb_to_linear(g as f64 / 255.0);
    let b_lin = srgb_to_linear(b as f64 / 255.0);

    // Linear RGB to XYZ (D65 reference white)
    let x = r_lin * 0.4124564 + g_lin * 0.3575761 + b_lin * 0.1804375;
    let y = r_lin * 0.2126729 + g_lin * 0.7151522 + b_lin * 0.0721750;
    let z = r_lin * 0.0193339 + g_lin * 0.1191920 + b_lin * 0.9503041;

    // D65 reference white
    let xn = 0.950456;
    let yn = 1.0;
    let zn = 1.088754;

    let fx = lab_f(x / xn);
    let fy = lab_f(y / yn);
    let fz = lab_f(z / zn);

    let l_star = 116.0 * fy - 16.0; // [0, 100]
    let a_star = 500.0 * (fx - fy); // roughly [-128, 127]
    let b_star = 200.0 * (fy - fz); // roughly [-128, 127]

    // Map to OpenCV ranges: L -> [0,255], a,b -> [0,255] with 128 as zero
    let l_out = (l_star * 255.0 / 100.0).clamp(0.0, 255.0) as u8;
    let a_out = (a_star + 128.0).clamp(0.0, 255.0) as u8;
    let b_out = (b_star + 128.0).clamp(0.0, 255.0) as u8;

    (l_out, a_out, b_out)
}

/// Convert CIE-LAB (OpenCV ranges) back to sRGB [0,255].
pub fn lab_to_rgb(l: u8, a: u8, b: u8) -> (u8, u8, u8) {
    let l_star = l as f64 * 100.0 / 255.0;
    let a_star = a as f64 - 128.0;
    let b_star = b as f64 - 128.0;

    let fy = (l_star + 16.0) / 116.0;
    let fx = a_star / 500.0 + fy;
    let fz = fy - b_star / 200.0;

    let xn = 0.950456;
    let yn = 1.0;
    let zn = 1.088754;

    let x = xn * lab_f_inv(fx);
    let y = yn * lab_f_inv(fy);
    let z = zn * lab_f_inv(fz);

    // XYZ to linear RGB
    let r_lin = x * 3.2404542 + y * -1.5371385 + z * -0.4985314;
    let g_lin = x * -0.9692660 + y * 1.8760108 + z * 0.0415560;
    let b_lin = x * 0.0556434 + y * -0.2040259 + z * 1.0572252;

    let r = (linear_to_srgb(r_lin) * 255.0).clamp(0.0, 255.0) as u8;
    let g = (linear_to_srgb(g_lin) * 255.0).clamp(0.0, 255.0) as u8;
    let b = (linear_to_srgb(b_lin) * 255.0).clamp(0.0, 255.0) as u8;

    (r, g, b)
}

/// Convert RGB [0,255] to HSV (OpenCV ranges: H [0,180), S [0,255], V [0,255]).
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let rf = r as f64 / 255.0;
    let gf = g as f64 / 255.0;
    let bf = b as f64 / 255.0;

    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;

    let v = max;

    let s = if max == 0.0 { 0.0 } else { delta / max };

    let h = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };

    let h = if h < 0.0 { h + 360.0 } else { h };

    // OpenCV: H in [0,180), S in [0,255], V in [0,255]
    let h_out = (h / 2.0).clamp(0.0, 179.0) as u8;
    let s_out = (s * 255.0).clamp(0.0, 255.0) as u8;
    let v_out = (v * 255.0).clamp(0.0, 255.0) as u8;

    (h_out, s_out, v_out)
}

/// Convert HSV (OpenCV ranges) back to RGB [0,255].
pub fn hsv_to_rgb(h: u8, s: u8, v: u8) -> (u8, u8, u8) {
    let h_deg = h as f64 * 2.0; // [0, 360)
    let sf = s as f64 / 255.0;
    let vf = v as f64 / 255.0;

    let c = vf * sf;
    let x = c * (1.0 - ((h_deg / 60.0) % 2.0 - 1.0).abs());
    let m = vf - c;

    let (r1, g1, b1) = if h_deg < 60.0 {
        (c, x, 0.0)
    } else if h_deg < 120.0 {
        (x, c, 0.0)
    } else if h_deg < 180.0 {
        (0.0, c, x)
    } else if h_deg < 240.0 {
        (0.0, x, c)
    } else if h_deg < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    let r = ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8;
    let g = ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8;
    let b = ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8;

    (r, g, b)
}

// --- helpers ---

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn lab_f(t: f64) -> f64 {
    let delta: f64 = 6.0 / 29.0;
    if t > delta * delta * delta {
        t.cbrt()
    } else {
        t / (3.0 * delta * delta) + 4.0 / 29.0
    }
}

fn lab_f_inv(t: f64) -> f64 {
    let delta: f64 = 6.0 / 29.0;
    if t > delta {
        t * t * t
    } else {
        3.0 * delta * delta * (t - 4.0 / 29.0)
    }
}
