//! Reading a picked image the way the webview shows it, so a crop drawn
//! there cuts the same pixels here.

use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};
use std::io::Cursor;
use std::path::Path;

/// What transparent pixels become.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// Logos and graphics: transparent means paper.
    White,
    /// Video frames (which never have alpha).
    Black,
}

/// An image as the webview shows it: upright (EXIF orientation applied),
/// with its ICC profile, if any, kept for `to_srgb_rgb8`.
pub struct Decoded {
    pub image: DynamicImage,
    pub icc: Option<Vec<u8>>,
    /// The file had no EXIF orientation, or "upright" (1).
    pub upright: bool,
}

pub fn decode_file(path: &Path) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to load image {}: {}", path.display(), e))?;
    decode(&bytes).map_err(|e| format!("Failed to load image {}: {}", path.display(), e))
}

/// Decode `bytes`, turned upright. The webview draws an image rotated by its
/// EXIF orientation, so crop coordinates from it are in that space.
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = decoder.icc_profile().ok().flatten().filter(|p| !p.is_empty());
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let upright = orientation == image::metadata::Orientation::NoTransforms;
    image.apply_orientation(orientation);
    Ok(Decoded { image, icc, upright })
}

/// `image` as 8-bit sRGB without alpha: converted from the ICC profile `icc`
/// (e.g. Display P3 from an iPhone) when there is one and it can be read,
/// and any transparency flattened onto `background`. An unreadable or
/// non-RGB profile leaves the values as they are, read as sRGB.
pub fn to_srgb_rgb8(image: &DynamicImage, icc: Option<&[u8]>, background: Background) -> RgbImage {
    if !image.color().has_alpha() {
        let mut rgb = image.to_rgb8();
        if let Some(icc) = icc {
            convert_to_srgb(&mut rgb, moxcms::Layout::Rgb, icc);
        }
        return rgb;
    }
    let mut rgba = image.to_rgba8();
    if let Some(icc) = icc {
        convert_to_srgb(&mut rgba, moxcms::Layout::Rgba, icc);
    }
    let bg = match background {
        Background::White => 255.0,
        Background::Black => 0.0,
    };
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = p[3] as f32 / 255.0;
        let mix = |c: u8| (c as f32 * a + bg * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// Whether an ICC profile describes sRGB (same primaries and white point),
/// so pixels in it need no conversion.
pub fn is_srgb_profile(icc: &[u8]) -> bool {
    let Ok(p) = moxcms::ColorProfile::new_from_slice(icc) else { return false };
    let s = moxcms::ColorProfile::new_srgb();
    let near = |a: moxcms::Xyzd, b: moxcms::Xyzd| (a.x - b.x).abs() < 2e-3 && (a.y - b.y).abs() < 2e-3 && (a.z - b.z).abs() < 2e-3;
    p.color_space == moxcms::DataColorSpace::Rgb
        && near(p.red_colorant, s.red_colorant)
        && near(p.green_colorant, s.green_colorant)
        && near(p.blue_colorant, s.blue_colorant)
        && near(p.white_point, s.white_point)
}

/// Convert pixels (laid out as `layout`) from `icc`'s colour space to sRGB
/// in place.
fn convert_to_srgb(pixels: &mut [u8], layout: moxcms::Layout, icc: &[u8]) {
    use moxcms::{ColorProfile, DataColorSpace, TransformOptions};
    let Ok(source) = ColorProfile::new_from_slice(icc) else {
        log::warn!("Unreadable colour profile; using the colours as they are");
        return;
    };
    if source.color_space != DataColorSpace::Rgb {
        return;
    }
    let srgb = ColorProfile::new_srgb();
    let transform = match source.create_transform_8bit(layout, &srgb, layout, TransformOptions::default()) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Can't convert the colour profile ({:?}); using the colours as they are", e);
            return;
        }
    };
    let src = pixels.to_vec();
    if let Err(e) = transform.transform(&src, pixels) {
        log::warn!("Colour conversion failed ({:?}); using the colours as they are", e);
        pixels.copy_from_slice(&src);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::{ImageEncoder, Rgb, Rgba};

    /// A JPEG of `img` carrying an EXIF orientation tag.
    pub fn jpeg_with_orientation(img: &RgbImage, orientation: u16) -> Vec<u8> {
        let mut jpeg = Vec::new();
        img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)).unwrap();
        // APP1 "Exif": a big-endian TIFF header and one IFD entry, 0x0112
        // (Orientation), type SHORT, count 1.
        let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        exif.extend_from_slice(&orientation.to_be_bytes());
        exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let len = (exif.len() + 2) as u16;
        let mut out = jpeg[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    /// 64×32: top half red, bottom half blue.
    pub fn red_over_blue() -> RgbImage {
        RgbImage::from_fn(64, 32, |_, y| if y < 16 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) })
    }

    pub fn is_close(p: Rgb<u8>, want: [u8; 3]) -> bool {
        p.0.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() <= 24)
    }

    #[test]
    fn exif_orientation_turns_the_image_upright() {
        // Orientation 6: shown turned 90° clockwise, so the top (red) edge
        // ends up on the right.
        let d = decode(&jpeg_with_orientation(&red_over_blue(), 6)).unwrap();
        assert!(!d.upright);
        let img = to_srgb_rgb8(&d.image, None, Background::White);
        assert_eq!((img.width(), img.height()), (32, 64));
        assert!(is_close(*img.get_pixel(28, 32), [255, 0, 0]), "{:?}", img.get_pixel(28, 32));
        assert!(is_close(*img.get_pixel(3, 32), [0, 0, 255]), "{:?}", img.get_pixel(3, 32));

        let upright = decode(&jpeg_with_orientation(&red_over_blue(), 1)).unwrap();
        assert!(upright.upright);
        assert_eq!(upright.image.width(), 64);
    }

    fn png(img: &image::RgbaImage, icc: Option<Vec<u8>>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
        if let Some(icc) = icc {
            encoder.set_icc_profile(icc).unwrap();
        }
        encoder.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8).unwrap();
        out
    }

    #[test]
    fn transparency_is_flattened_onto_the_background() {
        // A transparent logo with one opaque black pixel and one half-red.
        let mut logo = image::RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
        logo.put_pixel(1, 1, Rgba([0, 0, 0, 255]));
        logo.put_pixel(2, 2, Rgba([255, 0, 0, 128]));
        let d = decode(&png(&logo, None)).unwrap();
        let white = to_srgb_rgb8(&d.image, d.icc.as_deref(), Background::White);
        assert_eq!(*white.get_pixel(0, 0), Rgb([255, 255, 255]));
        assert_eq!(*white.get_pixel(1, 1), Rgb([0, 0, 0]));
        assert_eq!(*white.get_pixel(2, 2), Rgb([255, 127, 127]));
        let black = to_srgb_rgb8(&d.image, None, Background::Black);
        assert_eq!(*black.get_pixel(0, 0), Rgb([0, 0, 0]));
        assert_eq!(*black.get_pixel(2, 2), Rgb([128, 0, 0]));
    }

    #[test]
    fn display_p3_is_converted_to_srgb() {
        let p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        assert!(!is_srgb_profile(&p3));
        assert!(is_srgb_profile(&moxcms::ColorProfile::new_srgb().encode().unwrap()));
        let img = image::RgbaImage::from_pixel(4, 4, Rgba([200, 60, 60, 255]));
        let d = decode(&png(&img, Some(p3))).unwrap();
        assert!(d.icc.is_some());
        let out = to_srgb_rgb8(&d.image, d.icc.as_deref(), Background::White);
        let p = out.get_pixel(0, 0).0;
        // The same red is more saturated in sRGB's smaller gamut.
        assert!(p[0] > 210 && p[1] < 55, "{p:?}");
        // Without a profile the values are kept.
        assert_eq!(to_srgb_rgb8(&d.image, None, Background::White).get_pixel(0, 0).0, [200, 60, 60]);
        // A profile that can't be read leaves them too.
        assert_eq!(to_srgb_rgb8(&d.image, Some(b"junk"), Background::White).get_pixel(0, 0).0, [200, 60, 60]);
    }
}
