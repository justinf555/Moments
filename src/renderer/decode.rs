//! Image decode step.
//!
//! Decodes an image file using magic-byte format detection. This is the
//! single decode entry point — no other code should call `image::open`,
//! `image::ImageReader`, or `FormatRegistry::decode` directly.

use std::path::Path;
use std::sync::Arc;

use image::DynamicImage;
use tracing::instrument;

use crate::library::error::LibraryError;
use crate::renderer::format::FormatRegistry;

/// Decode an image from a file path using magic-byte detection.
#[instrument(skip(formats))]
pub fn decode(path: &Path, formats: &Arc<FormatRegistry>) -> Result<DynamicImage, LibraryError> {
    formats.decode(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::format::standard::StandardHandler;
    use image::{ImageFormat, RgbaImage};
    use std::io::Cursor;

    fn test_formats() -> Arc<FormatRegistry> {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(StandardHandler));
        Arc::new(reg)
    }

    fn write_test_jpeg(dir: &Path, name: &str) -> std::path::PathBuf {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 50));
        let path = dir.join(name);
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, &buf).unwrap();
        path
    }

    #[test]
    fn decode_jpeg_by_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "no_extension");
        let formats = test_formats();

        let img = decode(&path, &formats).unwrap();
        assert_eq!((img.width(), img.height()), (100, 50));
    }

    #[test]
    fn decode_missing_file_returns_error() {
        let formats = test_formats();
        let result = decode(Path::new("/nonexistent/photo"), &formats);
        assert!(result.is_err());
    }
}
