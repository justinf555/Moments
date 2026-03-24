use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::library::error::LibraryError;
use crate::library::media::MediaType;

/// Video file extensions accepted during import.
///
/// Videos are recognised by extension but not decoded by `FormatHandler`
/// (that trait returns `DynamicImage`). Thumbnail generation for videos
/// requires a separate pipeline (Phase 2 — GStreamer).
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "3gp",
];

/// Decodes image files of a specific set of formats into a [`image::DynamicImage`].
///
/// Implement this trait for each format family (standard, RAW, etc.) and
/// register an instance with [`FormatRegistry`]. Adding a new format is then
/// one `registry.register()` call — no other pipeline changes required.
pub trait FormatHandler: Send + Sync {
    /// Lowercase file extensions this handler claims (no leading dot).
    fn extensions(&self) -> &[&str];

    /// Decode the file at `path` to a [`image::DynamicImage`].
    fn decode(&self, path: &Path) -> Result<image::DynamicImage, LibraryError>;
}

/// Single source of truth for all supported image formats.
///
/// Owns a map from extension → handler. Used by:
/// - The import scanner, via [`FormatRegistry::is_supported`], to decide
///   which files to accept.
/// - The thumbnail pipeline, via [`FormatRegistry::decode`], to dispatch
///   to the correct decoder.
pub struct FormatRegistry {
    handlers: HashMap<String, Arc<dyn FormatHandler>>,
}

impl FormatRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for all extensions it declares.
    ///
    /// If two handlers claim the same extension the last one wins.
    pub fn register(&mut self, handler: Arc<dyn FormatHandler>) {
        for ext in handler.extensions() {
            self.handlers.insert(ext.to_string(), Arc::clone(&handler));
        }
    }

    /// Returns `true` if any registered handler claims `ext` (case-insensitive).
    pub fn is_supported(&self, ext: &str) -> bool {
        self.handlers.contains_key(ext)
    }

    /// Decode the file at `path` using the handler registered for its extension.
    ///
    /// Returns [`LibraryError::Thumbnail`] if no handler is registered or
    /// decoding fails.
    pub fn decode(&self, path: &Path) -> Result<image::DynamicImage, LibraryError> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let handler = self.handlers.get(&ext).ok_or_else(|| {
            LibraryError::Thumbnail(format!("no handler registered for .{ext}"))
        })?;

        handler.decode(path)
    }

    /// Returns `true` if `ext` is a recognised video format (case-insensitive).
    pub fn is_video(&self, ext: &str) -> bool {
        VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    }

    /// Determine the [`MediaType`] for a file extension.
    ///
    /// Returns `None` if the extension is neither a registered image format
    /// nor a recognised video format.
    pub fn media_type(&self, ext: &str) -> Option<MediaType> {
        let lower = ext.to_ascii_lowercase();
        // Check video first — VideoHandler registers in the handlers map
        // for thumbnail decode, but the media type is still Video.
        if VIDEO_EXTENSIONS.contains(&lower.as_str()) {
            Some(MediaType::Video)
        } else if self.handlers.contains_key(&lower) {
            Some(MediaType::Image)
        } else {
            None
        }
    }

    /// All extensions supported across all registered handlers.
    #[allow(dead_code)]
    pub fn supported_extensions(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeHandler;
    impl FormatHandler for FakeHandler {
        fn extensions(&self) -> &[&str] {
            &["fake"]
        }
        fn decode(&self, _path: &Path) -> Result<image::DynamicImage, LibraryError> {
            Err(LibraryError::Thumbnail("fake handler".into()))
        }
    }

    #[test]
    fn is_supported_returns_true_for_registered_extension() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));
        assert!(reg.is_supported("fake"));
        assert!(!reg.is_supported("jpg"));
    }

    #[test]
    fn supported_extensions_includes_all_registered() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));
        let exts: Vec<_> = reg.supported_extensions().collect();
        assert!(exts.contains(&"fake"));
    }

    #[test]
    fn decode_returns_error_for_unknown_extension() {
        let reg = FormatRegistry::new();
        let err = reg.decode(&PathBuf::from("photo.jpg")).unwrap_err();
        assert!(matches!(err, LibraryError::Thumbnail(_)));
    }

    #[test]
    fn is_video_recognises_common_formats() {
        let reg = FormatRegistry::new();
        assert!(reg.is_video("mp4"));
        assert!(reg.is_video("MOV"));
        assert!(reg.is_video("mkv"));
        assert!(!reg.is_video("jpg"));
        assert!(!reg.is_video("unknown"));
    }

    #[test]
    fn media_type_returns_correct_type() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));
        assert_eq!(reg.media_type("fake"), Some(MediaType::Image));
        assert_eq!(reg.media_type("mp4"), Some(MediaType::Video));
        assert_eq!(reg.media_type("unknown"), None);
    }
}
