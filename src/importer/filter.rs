use std::path::Path;

use crate::library::format::FormatRegistry;
use crate::library::media::MediaType;

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
/// Uses the format registry for extension matching with magic-byte sniffing
/// fallback. Returns the detected [`MediaType`] and normalised extension,
/// or [`FilterResult::Unsupported`] if the file is not recognised.
pub fn filter(source: &Path, formats: &FormatRegistry) -> FilterResult {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match formats.media_type_with_sniff(source, &ext) {
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
    use crate::library::format::StandardHandler;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn test_registry() -> FormatRegistry {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(StandardHandler));
        reg
    }

    #[test]
    fn jpeg_is_accepted() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"fake jpeg").unwrap();

        let reg = test_registry();
        match filter(&file, &reg) {
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

        let reg = test_registry();
        assert!(matches!(filter(&file, &reg), FilterResult::Unsupported));
    }

    #[test]
    fn no_extension_is_unsupported() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("noext");
        std::fs::write(&file, b"mystery").unwrap();

        let reg = test_registry();
        assert!(matches!(filter(&file, &reg), FilterResult::Unsupported));
    }
}
