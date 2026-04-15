//! EXIF orientation correction.
//!
//! Pipeline step: reads the EXIF orientation tag from the source file
//! and rotates/flips the image to match. Skips video files (no EXIF)
//! and HEIC/HEIF (libheif applies orientation during decode).

use std::path::Path;
use std::sync::Arc;

use image::DynamicImage;

use crate::renderer::format::FormatRegistry;
use crate::library::metadata::exif::extract_exif;

/// Full orientation pipeline step: read EXIF, skip when inappropriate, apply.
///
/// Skips for:
/// - Video files (no EXIF orientation)
/// - HEIC/HEIF (libheif applies orientation during decode — applying
///   again would double-rotate)
pub fn orient(path: &Path, img: DynamicImage, formats: &Arc<FormatRegistry>) -> DynamicImage {
    if formats.is_video_by_magic(path) || formats.is_heif_by_magic(path) {
        return img;
    }

    let orientation = extract_exif(path).orientation.unwrap_or(1);
    apply_orientation(img, orientation)
}

/// Rotate/flip `img` to match the EXIF orientation tag value (1–8).
// TODO: Make private once importer/thumbnail.rs uses the pipeline.
pub fn apply_orientation(img: DynamicImage, orientation: u8) -> DynamicImage {
    use image::imageops;
    match orientation {
        2 => DynamicImage::from(imageops::flip_horizontal(&img)),
        3 => DynamicImage::from(imageops::rotate180(&img)),
        4 => DynamicImage::from(imageops::flip_vertical(&img)),
        5 => DynamicImage::from(imageops::flip_horizontal(&imageops::rotate90(&img))),
        6 => DynamicImage::from(imageops::rotate90(&img)),
        7 => DynamicImage::from(imageops::flip_horizontal(&imageops::rotate270(&img))),
        8 => DynamicImage::from(imageops::rotate270(&img)),
        _ => img, // 1 or unknown — already upright
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn orientation_1_is_identity() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 1);
        assert_eq!((out.width(), out.height()), (4, 2));
    }

    #[test]
    fn orientation_6_swaps_dimensions() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 6);
        assert_eq!((out.width(), out.height()), (2, 4));
    }

    #[test]
    fn orientation_8_swaps_dimensions() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 8);
        assert_eq!((out.width(), out.height()), (2, 4));
    }

    #[test]
    fn orientation_3_preserves_dimensions() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 3);
        assert_eq!((out.width(), out.height()), (4, 2));
    }

    #[test]
    fn orientation_unknown_is_identity() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(10, 5));
        let out = apply_orientation(img, 99);
        assert_eq!((out.width(), out.height()), (10, 5));
    }
}
