//! Output helpers.
//!
//! Convenience functions for converting a `DynamicImage` into the
//! formats consumers need. Not a pipeline step — called by the
//! consumer after the pipeline returns.

use image::DynamicImage;

use crate::renderer::error::RenderError;

/// Convert to RGBA pixel bytes. Returns `(bytes, width, height)`.
///
/// Ready for `GdkMemoryTexture::new()`.
pub fn to_rgba(img: &DynamicImage) -> (Vec<u8>, u32, u32) {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    (rgba.into_raw(), w, h)
}

/// Encode as WebP bytes.
///
/// Ready for writing to the thumbnail disk cache.
pub fn to_webp(img: &DynamicImage) -> Result<Vec<u8>, RenderError> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::WebP)
        .map_err(|e| RenderError::EncodeFailed(e.to_string()))?;
    Ok(buf.into_inner())
}

/// Encode as JPEG bytes at the given quality (1–100).
///
/// Phase C uses this to produce the rendered output that gets stacked
/// alongside the original on Immich. The caller is expected to inject
/// the Moments XMP APP1 segment afterwards via
/// [`super::xmp::inject_xmp`].
///
/// Blocking — call from `spawn_blocking`. JPEG compression can take
/// 50–500 ms for a large photo.
pub fn to_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, RenderError> {
    use image::codecs::jpeg::JpegEncoder;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut buf = Vec::with_capacity((w * h) as usize);
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| RenderError::EncodeFailed(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn test_image() -> DynamicImage {
        let mut img = RgbaImage::new(4, 4);
        for px in img.pixels_mut() {
            *px = image::Rgba([200, 100, 50, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn to_rgba_correct_dimensions() {
        let img = test_image();
        let (bytes, w, h) = to_rgba(&img);
        assert_eq!(w, 4);
        assert_eq!(h, 4);
        assert_eq!(bytes.len(), (4 * 4 * 4) as usize);
    }

    #[test]
    fn to_rgba_preserves_pixel_values() {
        let img = test_image();
        let (bytes, _, _) = to_rgba(&img);
        // First pixel: R=200, G=100, B=50, A=255
        assert_eq!(bytes[0], 200);
        assert_eq!(bytes[1], 100);
        assert_eq!(bytes[2], 50);
        assert_eq!(bytes[3], 255);
    }

    #[test]
    fn to_jpeg_produces_valid_jpeg() {
        let img = test_image();
        let bytes = to_jpeg(&img, 85).unwrap();
        // JPEG magic: FF D8 FF.
        assert!(bytes.len() > 4);
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF]);
        // Ends with EOI marker FF D9.
        assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn to_webp_produces_valid_bytes() {
        let img = test_image();
        let bytes = to_webp(&img).unwrap();
        assert!(!bytes.is_empty());
        // WebP magic: RIFF....WEBP
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }
}
