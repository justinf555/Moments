//! Unified render pipeline.
//!
//! Central, stateless pipeline for all image decode/render/output operations.
//! Replaces scattered inline decode logic across the viewer, thumbnail
//! generator, and edit panel.
//!
//! ```text
//! Path → decode (magic bytes) → orient (EXIF) → resize? → apply edits? → output
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::DynamicImage;
use tracing::instrument;

use crate::library::editing::EditState;
use crate::library::error::LibraryError;
use crate::library::format::FormatRegistry;
use crate::library::media::MediaId;
use crate::library::thumbnail::{sharded_original_path, sharded_thumbnail_path};

/// What size to render.
#[derive(Debug, Clone)]
pub enum RenderSize {
    /// Full resolution — no resize.
    FullRes,
    /// Resize longest edge to this many pixels.
    Thumbnail(u32),
}

/// Output format for the rendered image.
#[derive(Debug, Clone)]
pub enum RenderOutput {
    /// Raw RGBA pixel bytes + dimensions. Ready for `GdkMemoryTexture`.
    Rgba,
    /// Encoded WebP bytes. Ready for disk cache.
    WebP,
}

/// Options controlling what the pipeline produces.
#[derive(Debug, Clone)]
pub struct RenderOptions<'a> {
    pub size: RenderSize,
    pub output: RenderOutput,
    pub edits: Option<&'a EditState>,
}

/// Result of a render operation.
pub enum RenderResult {
    /// RGBA pixel bytes with dimensions.
    Rgba {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Encoded WebP bytes.
    WebP(Vec<u8>),
}

/// Central stateless render pipeline.
///
/// Shared via `Arc<RenderPipeline>` — all methods take `&self`.
/// One instance per application, created at startup.
pub struct RenderPipeline {
    formats: Arc<FormatRegistry>,
    originals_dir: PathBuf,
    thumbnails_dir: PathBuf,
}

impl RenderPipeline {
    pub fn new(
        formats: Arc<FormatRegistry>,
        originals_dir: PathBuf,
        thumbnails_dir: PathBuf,
    ) -> Self {
        Self {
            formats,
            originals_dir,
            thumbnails_dir,
        }
    }

    /// Resolve the original file path for a media ID.
    pub fn original_path(&self, id: &MediaId) -> PathBuf {
        sharded_original_path(&self.originals_dir, id)
    }

    /// Resolve the thumbnail file path for a media ID.
    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        sharded_thumbnail_path(&self.thumbnails_dir, id)
    }

    /// Decode an image from a file path using magic-byte detection.
    ///
    /// This is the single decode entry point — no other code should call
    /// `image::open`, `image::ImageReader`, or `FormatRegistry::decode`
    /// directly.
    #[instrument(skip(self))]
    pub fn decode(&self, path: &Path) -> Result<DynamicImage, LibraryError> {
        self.formats.decode(path)
    }

    /// Full render pipeline: decode → orient → resize → edit → output.
    ///
    /// Blocking — call from `spawn_blocking` or a blocking thread.
    #[instrument(skip(self, options))]
    pub fn render(
        &self,
        path: &Path,
        options: &RenderOptions<'_>,
    ) -> Result<RenderResult, LibraryError> {
        // 1. Decode
        let img = self.decode(path)?;

        // 2. Orient (EXIF)
        let img = self.apply_exif_orientation(path, img);

        // 3. Resize
        let img = match options.size {
            RenderSize::FullRes => img,
            RenderSize::Thumbnail(max_edge) => img.thumbnail(max_edge, max_edge),
        };

        // 4. Apply edits
        let img = match options.edits {
            Some(edits) => super::apply_edits(&img, edits),
            None => img,
        };

        // 5. Output
        match options.output {
            RenderOutput::Rgba => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Ok(RenderResult::Rgba {
                    bytes: rgba.into_raw(),
                    width: w,
                    height: h,
                })
            }
            RenderOutput::WebP => {
                let mut buf = std::io::Cursor::new(Vec::new());
                img.write_to(&mut buf, image::ImageFormat::WebP)
                    .map_err(|e| LibraryError::Thumbnail(e.to_string()))?;
                Ok(RenderResult::WebP(buf.into_inner()))
            }
        }
    }

    /// Apply EXIF orientation correction.
    ///
    /// Skips for video files and HEIC/HEIF (libheif applies orientation
    /// during decode — applying again would double-rotate).
    fn apply_exif_orientation(&self, path: &Path, img: DynamicImage) -> DynamicImage {
        if self.formats.is_video_by_magic(path) || self.formats.is_heif_by_magic(path) {
            return img;
        }

        let orientation = crate::library::metadata::exif::extract_exif(path)
            .orientation
            .unwrap_or(1);

        super::orientation::apply_orientation(img, orientation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, ImageFormat, RgbaImage};
    use std::io::Cursor;

    fn test_formats() -> Arc<FormatRegistry> {
        use crate::library::format::standard::StandardHandler;
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(StandardHandler));
        Arc::new(reg)
    }

    fn write_test_jpeg(dir: &Path, name: &str) -> PathBuf {
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
        // Write a JPEG but give it no extension.
        let path = write_test_jpeg(dir.path(), "no_extension");

        let pipeline = RenderPipeline::new(
            test_formats(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        let img = pipeline.decode(&path).unwrap();
        assert_eq!(img.dimensions(), (100, 50));
    }

    #[test]
    fn render_thumbnail_produces_webp() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");

        let pipeline = RenderPipeline::new(
            test_formats(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );

        let result = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::Thumbnail(50),
                    output: RenderOutput::WebP,
                    edits: None,
                },
            )
            .unwrap();

        match result {
            RenderResult::WebP(bytes) => {
                assert!(!bytes.is_empty());
                // WebP magic: RIFF....WEBP
                assert_eq!(&bytes[..4], b"RIFF");
                assert_eq!(&bytes[8..12], b"WEBP");
            }
            _ => panic!("expected WebP output"),
        }
    }

    #[test]
    fn render_fullres_produces_rgba() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");

        let pipeline = RenderPipeline::new(
            test_formats(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );

        let result = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    output: RenderOutput::Rgba,
                    edits: None,
                },
            )
            .unwrap();

        match result {
            RenderResult::Rgba {
                bytes,
                width,
                height,
            } => {
                assert_eq!(width, 100);
                assert_eq!(height, 50);
                assert_eq!(bytes.len(), (100 * 50 * 4) as usize);
            }
            _ => panic!("expected RGBA output"),
        }
    }

    #[test]
    fn render_with_edits_applies_brightness() {
        let dir = tempfile::tempdir().unwrap();

        // Write a JPEG with known pixel values.
        let mut img = RgbaImage::new(4, 4);
        for px in img.pixels_mut() {
            *px = image::Rgba([100, 100, 100, 255]);
        }
        let img = DynamicImage::ImageRgba8(img);
        let path = dir.path().join("test");
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, &buf).unwrap();

        let pipeline = RenderPipeline::new(
            test_formats(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );

        let mut edits = EditState::default();
        edits.exposure.brightness = 0.5;

        let result = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    output: RenderOutput::Rgba,
                    edits: Some(&edits),
                },
            )
            .unwrap();

        if let RenderResult::Rgba { bytes, .. } = result {
            // First pixel R channel should be brighter than 100.
            assert!(bytes[0] > 100);
        } else {
            panic!("expected RGBA output");
        }
    }

    #[test]
    fn thumbnail_resize_respects_max_edge() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "wide");

        let pipeline = RenderPipeline::new(
            test_formats(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );

        let result = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::Thumbnail(20),
                    output: RenderOutput::Rgba,
                    edits: None,
                },
            )
            .unwrap();

        if let RenderResult::Rgba { width, height, .. } = result {
            // Longest edge should be ≤ 20.
            assert!(width <= 20);
            assert!(height <= 20);
            // Aspect ratio preserved: 100x50 → 20x10.
            assert_eq!(width, 20);
            assert_eq!(height, 10);
        } else {
            panic!("expected RGBA output");
        }
    }
}
