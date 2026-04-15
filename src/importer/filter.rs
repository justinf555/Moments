use std::path::Path;

use crate::library::media::MediaType;
use crate::renderer::pipeline::RenderPipeline;

/// Result of the filter step: either a recognised media type or a skip.
pub enum FilterResult {
    /// File recognised — proceed with import.
    Accepted {
        media_type: MediaType,
        extension: String,
    },
    /// File extension not recognised — skip.
    Unsupported,
}

/// Check whether `source` is a supported media file.
///
/// Uses the render pipeline's format detection (magic-byte sniffing
/// with extension fallback). Returns the detected [`MediaType`] and
/// normalised extension, or [`FilterResult::Unsupported`] if the file
/// is not recognised.
pub fn filter(source: &Path, pipeline: &RenderPipeline) -> FilterResult {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match pipeline.media_type(source, &ext) {
        Some(media_type) => FilterResult::Accepted {
            media_type,
            extension: ext,
        },
        None => FilterResult::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn test_pipeline() -> Arc<RenderPipeline> {
        Arc::new(RenderPipeline::new())
    }

    #[test]
    fn jpeg_is_accepted() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"fake jpeg").unwrap();

        let pipeline = test_pipeline();
        match filter(&file, &pipeline) {
            FilterResult::Accepted { media_type, .. } => {
                assert_eq!(media_type, MediaType::Image);
            }
            FilterResult::Unsupported => panic!("expected accepted"),
        }
    }

    #[test]
    fn pdf_is_unsupported() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("doc.pdf");
        std::fs::write(&file, b"not a photo").unwrap();

        let pipeline = test_pipeline();
        assert!(matches!(filter(&file, &pipeline), FilterResult::Unsupported));
    }

    #[test]
    fn no_extension_is_unsupported() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("noext");
        std::fs::write(&file, b"mystery").unwrap();

        let pipeline = test_pipeline();
        assert!(matches!(filter(&file, &pipeline), FilterResult::Unsupported));
    }
}
