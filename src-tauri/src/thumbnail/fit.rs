//! YouTube's custom thumbnail limits: at most 2 MB, and 1280×720 is all it
//! shows. Anything we hand to YouTube (or export) goes through here.

use image::{ImageFormat, RgbImage};

pub const MAX_WIDTH: u32 = 1280;
pub const MAX_HEIGHT: u32 = 720;
/// YouTube's limit is 2 MB; stay a little under it.
pub const MAX_BYTES: usize = 2_000_000;
/// Tried in order until the JPEG fits.
const QUALITIES: [u8; 7] = [92, 85, 78, 70, 60, 50, 40];

/// `bytes` as a JPEG YouTube accepts: scaled down to fit 1280×720 (never
/// up), quality stepped down until it's under 2 MB. A JPEG already within
/// the limits is returned untouched.
pub fn fit_for_youtube(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let format = image::guess_format(bytes).map_err(|e| format!("Unrecognised image: {}", e))?;
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("Failed to decode image: {}", e))?;
    if format == ImageFormat::Jpeg
        && img.width() <= MAX_WIDTH
        && img.height() <= MAX_HEIGHT
        && bytes.len() < MAX_BYTES
    {
        return Ok(bytes.to_vec());
    }
    encode_for_youtube(&img.to_rgb8())
}

/// Encode `img` as a JPEG within YouTube's limits (see `fit_for_youtube`).
pub fn encode_for_youtube(img: &RgbImage) -> Result<Vec<u8>, String> {
    let scaled;
    let img = if img.width() > MAX_WIDTH || img.height() > MAX_HEIGHT {
        let scale = (MAX_WIDTH as f64 / img.width() as f64).min(MAX_HEIGHT as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).clamp(1, MAX_WIDTH);
        let h = ((img.height() as f64 * scale).round() as u32).clamp(1, MAX_HEIGHT);
        // Area averaging: fast on a 24 MP photo, and clean when shrinking.
        scaled = image::imageops::thumbnail(img, w, h);
        &scaled
    } else {
        img
    };
    for quality in QUALITIES {
        let mut buf = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        img.write_with_encoder(encoder)
            .map_err(|e| format!("JPEG encode error: {}", e))?;
        if buf.len() < MAX_BYTES {
            return Ok(buf);
        }
    }
    Err("The thumbnail is too detailed to fit YouTube's 2 MB limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// Deterministic noise: the worst case for JPEG size.
    fn noisy(w: u32, h: u32) -> RgbImage {
        let mut s: u32 = 0x1234_5678;
        RgbImage::from_fn(w, h, |_, _| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            Rgb([s as u8, (s >> 8) as u8, (s >> 16) as u8])
        })
    }

    fn png(img: &RgbImage) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn huge_noisy_image_is_scaled_and_squeezed() {
        let bytes = png(&noisy(6000, 4000));
        assert!(bytes.len() > MAX_BYTES);
        let out = fit_for_youtube(&bytes).unwrap();
        assert!(out.len() < MAX_BYTES, "{} bytes", out.len());
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::Jpeg);
        let img = image::load_from_memory(&out).unwrap();
        // 3:2 fitted inside 1280×720.
        assert_eq!((img.width(), img.height()), (1080, 720));
    }

    #[test]
    fn small_jpeg_is_left_untouched() {
        let mut jpeg = Vec::new();
        noisy(320, 180)
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80))
            .unwrap();
        assert_eq!(fit_for_youtube(&jpeg).unwrap(), jpeg);
    }

    #[test]
    fn small_png_becomes_a_same_size_jpeg() {
        let out = fit_for_youtube(&png(&noisy(1280, 720))).unwrap();
        let img = image::load_from_memory_with_format(&out, ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1280, 720));
        assert!(out.len() < MAX_BYTES);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(fit_for_youtube(b"not an image").is_err());
    }
}
