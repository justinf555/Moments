use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::super::error::RenderError;
use crate::library::media::MediaType;

/// Video file extensions accepted during import.
///
/// Videos are recognised by extension but not decoded by `FormatHandler`
/// (that trait returns `DynamicImage`). Thumbnail generation for videos
/// requires a separate pipeline (Phase 2 — GStreamer).
pub const VIDEO_EXTENSIONS: &[&str] = &[
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
    fn decode(&self, path: &Path) -> Result<image::DynamicImage, RenderError>;
}

/// Single source of truth for all supported image formats.
///
/// Owns a map from extension → handler. Used by:
/// - The import scanner, via [`FormatRegistry::media_type`], to decide
///   which files to accept.
/// - The thumbnail pipeline, via [`FormatRegistry::decode`], to dispatch
///   to the correct decoder.
pub struct FormatRegistry {
    handlers: HashMap<String, Arc<dyn FormatHandler>>,
}

impl Default for FormatRegistry {
    fn default() -> Self {
        Self::new()
    }
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

    /// Decode the file at `path` using the handler registered for its extension.
    ///
    /// Returns [`RenderError::DecodeFailed`] if no handler is registered or
    /// decoding fails.
    pub fn decode(&self, path: &Path) -> Result<image::DynamicImage, RenderError> {
        // Try magic-byte detection first (works for extensionless files).
        if let Some(handler) = self.handler_by_magic(path) {
            match handler.decode(path) {
                Ok(img) => return Ok(img),
                Err(_) => {
                    // Magic-byte match failed (e.g. TIFF header but actually
                    // a RAW format). Try all other handlers.
                    for other in self.handlers.values() {
                        if let Ok(img) = other.decode(path) {
                            return Ok(img);
                        }
                    }
                }
            }
        }

        // Fall back to extension-based lookup.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let handler = self
            .handlers
            .get(&ext)
            .ok_or_else(|| RenderError::FormatNotRecognised(path.to_path_buf()))?;

        handler.decode(path)
    }

    /// Try to find a handler by reading magic bytes from the file.
    fn handler_by_magic(&self, path: &Path) -> Option<Arc<dyn FormatHandler>> {
        use super::detect::{detect_format, DetectedFormat, ImageFormat, VideoFormat};

        let detected = detect_format(path).ok()?;
        let ext = match detected {
            DetectedFormat::Image(ImageFormat::Jpeg) => "jpg",
            DetectedFormat::Image(ImageFormat::Png) => "png",
            DetectedFormat::Image(ImageFormat::WebP) => "webp",
            DetectedFormat::Image(ImageFormat::Gif) => "gif",
            DetectedFormat::Image(ImageFormat::Tiff) => "tiff",
            DetectedFormat::Image(ImageFormat::Heif) => "heic",
            DetectedFormat::Video(VideoFormat::Mp4) => "mp4",
            DetectedFormat::Video(VideoFormat::Mov) => "mov",
            DetectedFormat::Video(VideoFormat::Mkv) => "mkv",
            DetectedFormat::Video(VideoFormat::Avi) => "avi",
            DetectedFormat::Unknown => return None,
        };
        self.handlers.get(ext).cloned()
    }

    /// Returns `true` if `ext` is a recognised video format (case-insensitive).
    pub fn is_video(&self, ext: &str) -> bool {
        VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    }

    /// Returns `true` if the file at `path` is a video (by magic bytes).
    pub fn is_video_by_magic(&self, path: &Path) -> bool {
        use super::detect::{detect_format, DetectedFormat};
        matches!(detect_format(path), Ok(DetectedFormat::Video(_)))
    }

    /// Returns `true` if the file at `path` is HEIC/HEIF (by magic bytes).
    pub fn is_heif_by_magic(&self, path: &Path) -> bool {
        use super::detect::{detect_format, DetectedFormat, ImageFormat};
        matches!(detect_format(path), Ok(DetectedFormat::Image(ImageFormat::Heif)))
    }

    /// Determine the [`MediaType`] using content sniffing with extension fallback.
    ///
    /// Reads the first 12 bytes of the file to identify the format by magic
    /// bytes. Falls back to extension-based detection for unknown signatures
    /// (e.g. RAW camera formats that share TIFF headers).
    pub fn media_type_with_sniff(&self, path: &Path, ext: &str) -> Option<MediaType> {
        use super::detect::{detect_format, DetectedFormat};

        if let Ok(detected) = detect_format(path) {
            match detected {
                DetectedFormat::Image(_) => return Some(MediaType::Image),
                DetectedFormat::Video(_) => return Some(MediaType::Video),
                DetectedFormat::Unknown => {} // fall through to extension
            }
        }

        self.media_type(ext)
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
        fn decode(&self, _path: &Path) -> Result<image::DynamicImage, RenderError> {
            Err(RenderError::DecodeFailed("fake handler".into()))
        }
    }

    #[test]
    fn media_type_returns_some_for_registered_extension() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));
        assert!(reg.media_type("fake").is_some());
        assert!(reg.media_type("jpg").is_none());
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
        assert!(matches!(err, RenderError::FormatNotRecognised(_)));
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

    #[test]
    fn media_type_with_sniff_overrides_extension() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));

        // Write JPEG magic bytes to a file with .fake extension.
        let mut f = NamedTempFile::with_suffix(".fake").unwrap();
        f.write_all(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        f.flush().unwrap();

        // Content sniffing detects Image (JPEG) — matches extension fallback too.
        assert_eq!(
            reg.media_type_with_sniff(f.path(), "fake"),
            Some(MediaType::Image),
        );

        // Write MP4 magic bytes to a file with .fake extension.
        let mut f = NamedTempFile::with_suffix(".fake").unwrap();
        f.write_all(b"\x00\x00\x00\x18ftypisom").unwrap();
        f.flush().unwrap();

        // Content sniffing detects Video, overriding extension-based Image.
        assert_eq!(
            reg.media_type_with_sniff(f.path(), "fake"),
            Some(MediaType::Video),
        );
    }

    #[test]
    fn media_type_with_sniff_falls_back_for_unknown_content() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FakeHandler));

        // Write unknown bytes — sniff returns Unknown, falls back to extension.
        let mut f = tempfile::NamedTempFile::with_suffix(".fake").unwrap();
        std::io::Write::write_all(&mut f, &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05]).unwrap();
        std::io::Write::flush(&mut f).unwrap();

        assert_eq!(
            reg.media_type_with_sniff(f.path(), "fake"),
            Some(MediaType::Image), // extension fallback
        );
    }
}
