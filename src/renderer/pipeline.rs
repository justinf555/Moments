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

    /// Full render pipeline: decode → orient → resize → edit → DynamicImage.
    ///
    /// Blocking — call from `spawn_blocking` or a blocking thread.
    /// The caller converts the result using [`super::output`] helpers.
    #[instrument(skip(self, options))]
    pub fn render(
        &self,
        path: &Path,
        options: &RenderOptions<'_>,
    ) -> Result<DynamicImage, LibraryError> {
        // Step 1: Decode — detect format from magic bytes, dispatch to handler.
        let img = super::decode::decode(path, &self.formats)?;

        // Step 2: Orient — apply EXIF rotation/flip (skips video and HEIF).
        let img = self.apply_exif_orientation(path, img);

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
    use crate::library::format::standard::StandardHandler;
    use image::{GenericImageView, ImageFormat, RgbaImage};
    use std::io::Cursor;

    fn test_formats() -> Arc<FormatRegistry> {
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

    fn test_pipeline(dir: &Path) -> RenderPipeline {
        RenderPipeline::new(
            test_formats(),
            dir.to_path_buf(),
            dir.to_path_buf(),
        )
    }

    #[test]
    fn render_fullres_returns_correct_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline(dir.path());

        let img = pipeline
            .render(&path, &RenderOptions { size: RenderSize::FullRes, edits: None })
            .unwrap();
        assert_eq!(img.dimensions(), (100, 50));
    }

    #[test]
    fn render_thumbnail_resizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline(dir.path());

        let img = pipeline
            .render(&path, &RenderOptions { size: RenderSize::Thumbnail(20), edits: None })
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

        let pipeline = test_pipeline(dir.path());
        let mut edits = EditState::default();
        edits.exposure.brightness = 0.5;

        let img = pipeline
            .render(&path, &RenderOptions { size: RenderSize::FullRes, edits: Some(&edits) })
            .unwrap();

        let px = img.as_rgba8().unwrap().get_pixel(0, 0);
        assert!(px[0] > 100);
    }

    #[test]
    fn render_extensionless_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "no_extension");
        let pipeline = test_pipeline(dir.path());

        let img = pipeline
            .render(&path, &RenderOptions { size: RenderSize::FullRes, edits: None })
            .unwrap();
        assert_eq!(img.dimensions(), (100, 50));
    }

    #[test]
    fn output_helpers_work_with_pipeline_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_jpeg(dir.path(), "photo");
        let pipeline = test_pipeline(dir.path());

        let img = pipeline
            .render(&path, &RenderOptions { size: RenderSize::FullRes, edits: None })
            .unwrap();

        let (bytes, w, h) = super::super::output::to_rgba(&img);
        assert_eq!(w, 100);
        assert_eq!(h, 50);
        assert_eq!(bytes.len(), (100 * 50 * 4) as usize);

        let webp = super::super::output::to_webp(&img).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
    }
}
