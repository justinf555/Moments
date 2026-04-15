//! Resize step.
//!
//! Resizes an image to fit within a maximum edge length, preserving
//! aspect ratio. Used for thumbnail generation. Full-res rendering
//! skips this step.

use image::DynamicImage;

/// Resize `img` so its longest edge is at most `max_edge` pixels.
///
/// Preserves aspect ratio. Returns the image unchanged if both
/// dimensions are already within the limit.
pub fn resize(img: DynamicImage, max_edge: u32) -> DynamicImage {
    if img.width() <= max_edge && img.height() <= max_edge {
        return img;
    }
    img.thumbnail(max_edge, max_edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn resize_landscape() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(200, 100));
        let out = resize(img, 50);
        assert_eq!(out.width(), 50);
        assert_eq!(out.height(), 25);
    }

    #[test]
    fn resize_portrait() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 200));
        let out = resize(img, 50);
        assert_eq!(out.width(), 25);
        assert_eq!(out.height(), 50);
    }

    #[test]
    fn resize_square() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 100));
        let out = resize(img, 50);
        assert_eq!(out.width(), 50);
        assert_eq!(out.height(), 50);
    }

    #[test]
    fn resize_already_within_limit() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(30, 20));
        let out = resize(img, 50);
        assert_eq!(out.width(), 30);
        assert_eq!(out.height(), 20);
    }

    #[test]
    fn resize_exact_match_no_change() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(50, 50));
        let out = resize(img, 50);
        assert_eq!(out.width(), 50);
        assert_eq!(out.height(), 50);
    }
}
