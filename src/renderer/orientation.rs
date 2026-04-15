//! EXIF orientation correction.
//!
//! Applies the rotation/flip indicated by EXIF orientation tag (1–8).
//! Used as a pipeline step after decode, before resize and edits.

use image::DynamicImage;

/// Rotate/flip `img` to match the EXIF orientation tag value (1–8).
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
