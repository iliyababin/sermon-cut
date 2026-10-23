//! YouTube's custom thumbnail limits: at most 2 MB, and 1280×720 is all it
//! shows. Anything we hand to YouTube (or export) goes through here.

use super::decode::{self, Background};
use image::{ImageFormat, RgbImage};
use std::borrow::Cow;

pub const MAX_WIDTH: u32 = 1280;
pub const MAX_HEIGHT: u32 = 720;
/// YouTube's limit is 2 MB; stay a little under it.
pub const MAX_BYTES: usize = 2_000_000;
/// Tried in order until the JPEG fits.
const QUALITIES: [u8; 7] = [92, 85, 78, 70, 60, 50, 40];

/// `bytes` as a JPEG YouTube accepts: upright, sRGB, opaque, scaled down to
/// fit 1280×720 (never up), quality stepped down until it's under 2 MB. A
/// JPEG that is all of that already is returned untouched.
pub fn fit_for_youtube(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let format = image::guess_format(bytes).map_err(|e| format!("Unrecognised image: {}", e))?;
    let d = decode::decode(bytes).map_err(|e| format!("Failed to decode image: {}", e))?;
    if format == ImageFormat::Jpeg
        && d.upright
        && d.icc.as_deref().is_none_or(decode::is_srgb_profile)
        && !d.image.color().has_alpha()
        && d.image.width() <= MAX_WIDTH
        && d.image.height() <= MAX_HEIGHT
        && bytes.len() < MAX_BYTES
    {
        return Ok(bytes.to_vec());
    }
    let shrunk = shrink_dynamic(&d.image);
    encode_for_youtube(&decode::to_srgb_rgb8(&shrunk, d.icc.as_deref(), Background::White))
}

/// The size `w`×`h` scaled down to fit 1280×720, if it's bigger.
fn fitted(w: u32, h: u32) -> Option<(u32, u32)> {
    if w <= MAX_WIDTH && h <= MAX_HEIGHT {
        return None;
    }
    let scale = (MAX_WIDTH as f64 / w as f64).min(MAX_HEIGHT as f64 / h as f64);
    Some((
        ((w as f64 * scale).round() as u32).clamp(1, MAX_WIDTH),
        ((h as f64 * scale).round() as u32).clamp(1, MAX_HEIGHT),
    ))
}

/// `img` scaled down to fit 1280×720 (never up). Area averaging: fast on a
/// 24 MP photo, and clean when shrinking.
pub fn shrink(img: &RgbImage) -> Cow<'_, RgbImage> {
    match fitted(img.width(), img.height()) {
        Some((w, h)) => Cow::Owned(image::imageops::thumbnail(img, w, h)),
        None => Cow::Borrowed(img),
    }
}

/// `shrink` for any pixel type (alpha kept for flattening later).
pub fn shrink_dynamic(img: &image::DynamicImage) -> Cow<'_, image::DynamicImage> {
    match fitted(img.width(), img.height()) {
        Some((w, h)) => Cow::Owned(match img {
            image::DynamicImage::ImageRgb8(rgb) => image::imageops::thumbnail(rgb, w, h).into(),
            other => image::imageops::thumbnail(&other.to_rgba8(), w, h).into(),
        }),
        None => Cow::Borrowed(img),
    }
}

/// Encode `img` as a JPEG within YouTube's limits (see `fit_for_youtube`).
pub fn encode_for_youtube(img: &RgbImage) -> Result<Vec<u8>, String> {
    let img = shrink(img);
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
    fn a_rotated_or_wide_gamut_jpeg_is_re_encoded_upright_and_srgb() {
        use super::decode::tests::{jpeg_with_orientation, red_over_blue};
        let rotated = jpeg_with_orientation(&red_over_blue(), 6);
        let out = fit_for_youtube(&rotated).unwrap();
        assert_ne!(out, rotated);
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (32, 64));

        let mut p3_jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut p3_jpeg, 90);
        use image::ImageEncoder;
        encoder.set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap()).unwrap();
        let red = RgbImage::from_pixel(16, 16, Rgb([200, 60, 60]));
        encoder.write_image(red.as_raw(), 16, 16, image::ExtendedColorType::Rgb8).unwrap();
        let out = fit_for_youtube(&p3_jpeg).unwrap();
        assert_ne!(out, p3_jpeg);
        let p = image::load_from_memory(&out).unwrap().to_rgb8().get_pixel(8, 8).0;
        assert!(p[0] > 210 && p[1] < 55, "{p:?}");

        // Tagged sRGB is fine as it is.
        let mut srgb_jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut srgb_jpeg, 90);
        encoder.set_icc_profile(moxcms::ColorProfile::new_srgb().encode().unwrap()).unwrap();
        encoder.write_image(red.as_raw(), 16, 16, image::ExtendedColorType::Rgb8).unwrap();
        assert_eq!(fit_for_youtube(&srgb_jpeg).unwrap(), srgb_jpeg);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(fit_for_youtube(b"not an image").is_err());
    }
}
