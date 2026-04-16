use std::path::Path;

use tracing::instrument;

use super::error::ImportError;
use crate::library::media::MediaType;
use crate::library::metadata::exif::{extract_exif, ExifInfo};
use crate::renderer::format::VIDEO_EXTENSIONS;

/// Extracted metadata for a single file.
pub struct ExtractedMetadata {
    /// EXIF data (all-`None` for videos or files without EXIF).
    pub exif: ExifInfo,
    /// Video duration in milliseconds (`None` for images).
    pub duration_ms: Option<u64>,
}

/// Extract metadata from a media file on a blocking thread.
///
/// For images: extracts EXIF data (capture timestamp, camera, GPS, etc.).
/// For videos: extracts duration via GStreamer.
/// Returns an all-`None` result on any parse failure — never fails the pipeline.
#[instrument(skip_all, fields(path = %source.display(), media_type = ?media_type))]
pub async fn extract_metadata(
    source: &Path,
    media_type: MediaType,
    extension: &str,
) -> Result<ExtractedMetadata, ImportError> {
    let source = source.to_path_buf();
    let ext = extension.to_owned();

    tokio::task::spawn_blocking(move || {
        let is_video = VIDEO_EXTENSIONS.contains(&ext.as_str());
        let exif = if is_video {
            ExifInfo::default()
        } else {
            extract_exif(&source)
        };
        let duration_ms = if is_video {
            crate::library::metadata::video_meta::extract_video_metadata(&source).duration_ms
        } else {
            None
        };
        Ok(ExtractedMetadata { exif, duration_ms })
    })
    .await
    .map_err(|e| ImportError::Runtime(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn image_extraction_returns_exif_info() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"fake jpeg").unwrap();

        let result = extract_metadata(&file, MediaType::Image, "jpg")
            .await
            .unwrap();
        // Fake JPEG has no real EXIF, but extraction should succeed gracefully.
        assert!(result.duration_ms.is_none());
    }

    #[tokio::test]
    async fn video_extraction_skips_exif() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        std::fs::write(&file, b"fake video").unwrap();

        let result = extract_metadata(&file, MediaType::Video, "mp4")
            .await
            .unwrap();
        // EXIF should be default (all None) for videos.
        assert!(result.exif.captured_at.is_none());
        assert!(result.exif.camera_make.is_none());
    }
}
