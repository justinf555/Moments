//! Unified render pipeline.
//!
//! Central, stateless orchestrator that composes the individual step
//! modules into a single render call.
//!
//! ```text
//! Path → decode → orient (EXIF) → resize? → apply edits? → DynamicImage
//! ```
//!
//! The caller converts the output using [`super::output`] helpers.

use std::path::Path;
use std::sync::Arc;

use image::DynamicImage;
use tracing::instrument;

use crate::library::editing::EditState;
use crate::library::media::MediaType;
use crate::renderer::error::RenderError;
use crate::renderer::format::FormatRegistry;

/// What size to render.
#[derive(Debug, Clone)]
pub enum RenderSize {
    /// Full resolution — no resize.
    FullRes,
    /// Resize longest edge to this many pixels.
    Thumbnail(u32),
}

/// Options controlling what the pipeline produces.
#[derive(Debug, Clone)]
pub struct RenderOptions<'a> {
    pub size: RenderSize,
    pub edits: Option<&'a EditState>,
}

/// Central stateless render pipeline.
///
/// Shared via `Arc<RenderPipeline>` — all methods take `&self`.
/// One instance per application, created at startup.
pub struct RenderPipeline {
    formats: Arc<FormatRegistry>,
}

impl Default for RenderPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderPipeline {
    /// Create a pipeline with all supported format handlers registered.
    pub fn new() -> Self {
        use super::format::{raw::RawHandler, standard::StandardHandler, video::VideoHandler};
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(StandardHandler));
        registry.register(Arc::new(RawHandler));
        registry.register(Arc::new(VideoHandler));
        Self {
            formats: Arc::new(registry),
        }
    }

    /// Detect the media type of a file using magic bytes + extension fallback.
    ///
    /// Used by the import filter to decide whether to accept a file.
    pub fn media_type(&self, path: &Path, ext: &str) -> Option<MediaType> {
        self.formats.media_type_with_sniff(path, ext)
    }

    /// Full render pipeline: decode → orient → resize → edit → DynamicImage.
    ///
    /// Blocking — call from `spawn_blocking` or a blocking thread.
    /// The caller converts the result using [`super::output`] helpers.
    #[instrument(skip(self, options))]
    pub fn render(
        &self,
        path: &Path,
        options: &RenderOptions<'_>,
    ) -> Result<DynamicImage, RenderError> {
        // Step 1: Decode — detect format from magic bytes, dispatch to handler.
        let img = super::decode::decode(path, &self.formats)?;

        // Step 2: Orient — apply EXIF rotation/flip (skips video and HEIF).
        let img = super::orientation::orient(path, img, &self.formats);

        // Step 3: Resize — scale to thumbnail size if requested.
        let img = match options.size {
            RenderSize::FullRes => img,
            RenderSize::Thumbnail(max_edge) => super::resize::resize(img, max_edge),
        };

        // Step 4: Edit — apply non-destructive edits if provided.
        let img = match options.edits {
            Some(edits) => super::edits::apply_edits(&img, edits),
            None => img,
        };

        // Caller converts to output format via output::to_rgba() or output::to_webp().
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, ImageFormat, RgbaImage};
    use std::io::Cursor;
    use std::path::PathBuf;

    fn write_test_jpeg(dir: &Path, name: &str) -> PathBuf {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 50));
        let path = dir.join(name);
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, &buf).unwrap();
        path
    }

    fn test_pipeline() -> RenderPipeline {
        RenderPipeline::new()
    }

    #[test]
    fn render_fullres_returns_correct_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline();

        let img = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    edits: None,
                },
            )
            .unwrap();
        assert_eq!(img.dimensions(), (100, 50));
    }

    #[test]
    fn render_thumbnail_resizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline();

        let img = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::Thumbnail(20),
                    edits: None,
                },
            )
            .unwrap();
        assert!(img.width() <= 20);
        assert!(img.height() <= 20);
    }

    #[test]
    fn render_with_edits_applies_brightness() {
        let dir = tempfile::tempdir().unwrap();

        let mut src = RgbaImage::new(4, 4);
        for px in src.pixels_mut() {
            *px = image::Rgba([100, 100, 100, 255]);
        }
        let path = dir.path().join("test");
        let mut buf = Vec::new();
        DynamicImage::ImageRgba8(src)
            .write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, &buf).unwrap();

        let pipeline = test_pipeline();
        let mut edits = EditState::default();
        edits.exposure.brightness = 0.5;

        let img = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    edits: Some(&edits),
                },
            )
            .unwrap();

        let px = img.as_rgba8().unwrap().get_pixel(0, 0);
        assert!(px[0] > 100);
    }

    #[test]
    fn render_extensionless_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "no_extension");
        let pipeline = test_pipeline();

        let img = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    edits: None,
                },
            )
            .unwrap();
        assert_eq!(img.dimensions(), (100, 50));
    }

    #[test]
    fn output_helpers_work_with_pipeline_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline();

        let img = pipeline
            .render(
                &path,
                &RenderOptions {
                    size: RenderSize::FullRes,
                    edits: None,
                },
            )
            .unwrap();

        let (bytes, w, h) = super::super::output::to_rgba(&img);
        assert_eq!(w, 100);
        assert_eq!(h, 50);
        assert_eq!(bytes.len(), (100 * 50 * 4) as usize);

        let webp = super::super::output::to_webp(&img).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
    }
}
