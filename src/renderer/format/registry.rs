// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tracing::debug;

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

/// Hint to decoders about the size the caller actually needs.
///
/// Some formats (notably RAW) have multiple decode paths with very different
/// cost: a full sensor demosaic produces a sensor-resolution image and a
/// large transient allocation, while an embedded JPEG preview is small and
/// almost free. When the caller only wants a thumbnail, decoders should
/// prefer the cheap path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeHint {
    /// Best quality available (e.g. full demosaic for RAW). Used by the
    /// viewer's full-resolution path.
    Full,
    /// Smallest acceptable result. Used by the thumbnail pipeline.
    Thumbnail,
}

/// Decodes image files of a specific set of formats into a [`image::DynamicImage`].
///
/// Implement this trait for each format family (standard, RAW, etc.) and
/// register an instance with [`FormatRegistry`]. Adding a new format is then
/// one `registry.register()` call — no other pipeline changes required.
pub trait FormatHandler: Send + Sync {
    /// Lowercase file extensions this handler claims (no leading dot).
    fn extensions(&self) -> &[&str];

    /// Decode the file at `path` to a [`image::DynamicImage`].
    ///
    /// `hint` lets the handler pick a cheaper decode path when only a
    /// thumbnail is needed; handlers may ignore it if they have only one
    /// decode path.
    fn decode(&self, path: &Path, hint: DecodeHint) -> Result<image::DynamicImage, RenderError>;

    /// Whether this handler is safe to invoke *speculatively* — i.e.
    /// when the registry has fallen back to "try every handler" because
    /// the magic-byte handler errored out (e.g. a TIFF-headered CR2
    /// file).
    ///
    /// Defaults to `true`. Handlers that wrap external pipelines whose
    /// failure modes can hang the calling thread (rather than returning
    /// a quick `Err`) must override this to `false`. Such handlers are
    /// still reachable through extension and magic-byte matches; they
    /// just won't be tried blindly on unidentified data.
    ///
    /// See `VideoHandler` — GStreamer's `uridecodebin`/`typefind`
    /// probe can sit forever waiting for buffers when handed input it
    /// doesn't recognise.
    fn fallback_safe(&self) -> bool {
        true
    }
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

    /// Decode the file at `path`.
    ///
    /// Dispatch order:
    /// 1. Magic-byte detection (works for extensionless library originals).
    ///    If the detected handler fails, every other fallback-safe handler
    ///    is tried (e.g. a TIFF-headered CR2). If none succeeds, the
    ///    *detected* handler's error is returned as
    ///    [`RenderError::DecodeFailed`], prefixed with the detected format,
    ///    so the real cause (missing codec, truncated file, ...) is kept.
    /// 2. Lookup by file extension, when detection found nothing. The
    ///    handler's own error is returned if it fails.
    ///
    /// Returns [`RenderError::FormatNotRecognised`] only when detection
    /// found nothing and no handler is registered for the extension.
    pub fn decode(
        &self,
        path: &Path,
        hint: DecodeHint,
    ) -> Result<image::DynamicImage, RenderError> {
        // Try magic-byte detection first (works for extensionless files).
        if let Some((label, handler)) = self.handler_by_magic(path) {
            let detected_err = match handler.decode(path, hint) {
                Ok(img) => return Ok(img),
                Err(e) => e,
            };
            debug!(format = label, error = %detected_err, "detected handler failed, trying fallbacks");
            // Magic-byte match failed (e.g. TIFF header but the file is
            // actually a CR2 — Canon's TIFF-derived RAW format). Try every
            // other *fallback-safe* handler until one succeeds. We
            // deliberately exclude handlers whose decode path can hang on
            // unidentified input — see `fallback_safe` and `VideoHandler`.
            for other in self.handlers.values() {
                if !other.fallback_safe() {
                    continue;
                }
                match other.decode(path, hint) {
                    Ok(img) => return Ok(img),
                    Err(e) => {
                        debug!(
                            handler = ?other.extensions(),
                            error = %e,
                            "fallback handler failed"
                        );
                    }
                }
            }
            let msg = match detected_err {
                RenderError::DecodeFailed(msg) => msg,
                other => other.to_string(),
            };
            return Err(RenderError::DecodeFailed(format!("{label}: {msg}")));
        }

        // Detection found nothing: fall back to extension-based lookup.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let handler = self
            .handlers
            .get(&ext)
            .ok_or_else(|| RenderError::FormatNotRecognised(path.to_path_buf()))?;

        handler.decode(path, hint)
    }

    /// Try to find a handler by reading magic bytes from the file.
    ///
    /// Returns the detected format's display name alongside the handler.
    fn handler_by_magic(&self, path: &Path) -> Option<(&'static str, Arc<dyn FormatHandler>)> {
        use super::detect::{detect_format, DetectedFormat, ImageFormat, VideoFormat};

        let detected = detect_format(path).ok()?;
        let (label, ext) = match detected {
            DetectedFormat::Image(ImageFormat::Jpeg) => ("JPEG", "jpg"),
            DetectedFormat::Image(ImageFormat::Png) => ("PNG", "png"),
            DetectedFormat::Image(ImageFormat::WebP) => ("WebP", "webp"),
            DetectedFormat::Image(ImageFormat::Gif) => ("GIF", "gif"),
            DetectedFormat::Image(ImageFormat::Tiff) => ("TIFF", "tiff"),
            DetectedFormat::Image(ImageFormat::Heif) => ("HEIC", "heic"),
            DetectedFormat::Video(VideoFormat::Mp4) => ("MP4", "mp4"),
            DetectedFormat::Video(VideoFormat::Mov) => ("MOV", "mov"),
            DetectedFormat::Video(VideoFormat::Mkv) => ("MKV", "mkv"),
            DetectedFormat::Video(VideoFormat::Avi) => ("AVI", "avi"),
            DetectedFormat::Unknown => return None,
        };
        self.handlers.get(ext).cloned().map(|h| (label, h))
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
        matches!(
            detect_format(path),
            Ok(DetectedFormat::Image(ImageFormat::Heif))
        )
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
    use std::sync::Mutex;

    struct FakeHandler;
    impl FormatHandler for FakeHandler {
        fn extensions(&self) -> &[&str] {
            &["fake"]
        }
        fn decode(
            &self,
            _path: &Path,
            _hint: DecodeHint,
        ) -> Result<image::DynamicImage, RenderError> {
            Err(RenderError::DecodeFailed("fake handler".into()))
        }
    }

    /// Stand-in for a hang-prone handler (e.g. `VideoHandler` wrapping
    /// GStreamer). The test asserts that the registry's blind
    /// fallback iteration *never* invokes its `decode` — the symptom
    /// would be a hung importer thread parked inside `typefind:sink`.
    struct UnsafeFallbackHandler {
        invoked: Arc<Mutex<bool>>,
    }
    impl FormatHandler for UnsafeFallbackHandler {
        fn extensions(&self) -> &[&str] {
            &["unsafe"]
        }
        fn decode(
            &self,
            _path: &Path,
            _hint: DecodeHint,
        ) -> Result<image::DynamicImage, RenderError> {
            *self.invoked.lock().unwrap() = true;
            Err(RenderError::DecodeFailed("unsafe-fallback".into()))
        }
        fn fallback_safe(&self) -> bool {
            false
        }
    }

    /// Records the hint passed to `decode` so tests can assert it.
    struct RecordingHandler {
        last_hint: Arc<Mutex<Option<DecodeHint>>>,
    }
    impl FormatHandler for RecordingHandler {
        fn extensions(&self) -> &[&str] {
            &["rec"]
        }
        fn decode(
            &self,
            _path: &Path,
            hint: DecodeHint,
        ) -> Result<image::DynamicImage, RenderError> {
            *self.last_hint.lock().unwrap() = Some(hint);
            Err(RenderError::DecodeFailed("recording handler".into()))
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
        let err = reg
            .decode(&PathBuf::from("photo.jpg"), DecodeHint::Full)
            .unwrap_err();
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
    fn decode_forwards_hint_to_handler() {
        let last_hint = Arc::new(Mutex::new(None));
        let handler = Arc::new(RecordingHandler {
            last_hint: Arc::clone(&last_hint),
        });
        let mut reg = FormatRegistry::new();
        reg.register(handler);

        let _ = reg.decode(&PathBuf::from("photo.rec"), DecodeHint::Thumbnail);
        assert_eq!(*last_hint.lock().unwrap(), Some(DecodeHint::Thumbnail));

        let _ = reg.decode(&PathBuf::from("photo.rec"), DecodeHint::Full);
        assert_eq!(*last_hint.lock().unwrap(), Some(DecodeHint::Full));
    }

    /// Regression for the CR2 import hang: when the magic-byte handler
    /// errors out, the registry's blind retry loop must not invoke a
    /// handler that has opted out of speculative dispatch. Otherwise
    /// `VideoHandler` (wrapping GStreamer's `typefind`) would sit
    /// forever on a TIFF-headered CR2 file.
    /// Always-erroring stand-in for `StandardHandler` so we can drive
    /// the registry into its "magic match → decode failed → blind
    /// retry" path with a controlled handler set.
    struct ErroringJpegHandler;
    impl FormatHandler for ErroringJpegHandler {
        fn extensions(&self) -> &[&str] {
            &["jpg"]
        }
        fn decode(
            &self,
            _path: &Path,
            _hint: DecodeHint,
        ) -> Result<image::DynamicImage, RenderError> {
            Err(RenderError::DecodeFailed("forced fail".into()))
        }
    }

    #[test]
    fn fallback_iteration_skips_handlers_that_opt_out() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let invoked = Arc::new(Mutex::new(false));
        let mut reg = FormatRegistry::new();
        // ErroringJpegHandler claims "jpg" so JPEG magic-byte matching
        // picks it; its decode errors out, triggering the blind
        // fallback iteration.
        reg.register(Arc::new(ErroringJpegHandler));
        reg.register(Arc::new(UnsafeFallbackHandler {
            invoked: Arc::clone(&invoked),
        }));

        let mut f = NamedTempFile::with_suffix(".jpg").unwrap();
        f.write_all(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        f.flush().unwrap();

        // We don't care about the eventual error — only that
        // `UnsafeFallbackHandler::decode` is never reached.
        let _ = reg.decode(f.path(), DecodeHint::Thumbnail);

        assert!(
            !*invoked.lock().unwrap(),
            "fallback-unsafe handler must not be invoked during blind retry"
        );
    }

    /// Handler claiming `ext` that always fails with a fixed message.
    struct FailingHandler {
        ext: &'static str,
        msg: &'static str,
    }
    impl FormatHandler for FailingHandler {
        fn extensions(&self) -> &[&str] {
            std::slice::from_ref(&self.ext)
        }
        fn decode(
            &self,
            _path: &Path,
            _hint: DecodeHint,
        ) -> Result<image::DynamicImage, RenderError> {
            Err(RenderError::DecodeFailed(self.msg.into()))
        }
    }

    fn temp_file_with(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::with_suffix(suffix).unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn heic_decode_failure_is_reported_not_format_not_recognised() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FailingHandler {
            ext: "heic",
            msg: "no HEVC decoder",
        }));
        // Valid ftypheic header, no extension (like a library original).
        let f = temp_file_with(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00junk", "");

        let err = reg.decode(f.path(), DecodeHint::Thumbnail).unwrap_err();
        match &err {
            RenderError::DecodeFailed(msg) => {
                assert!(msg.contains("HEIC"), "{msg}");
                assert!(msg.contains("no HEVC decoder"), "{msg}");
            }
            other => panic!("expected DecodeFailed, got {other:?}"),
        }
        assert!(err.to_string().contains("HEIC"));
    }

    #[test]
    fn unrecognised_bytes_without_extension_is_format_not_recognised() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FailingHandler {
            ext: "jpg",
            msg: "bad jpeg",
        }));
        let f = temp_file_with(&[0x13, 0x37, 0xAB, 0xCD, 0x99, 0x01, 0x02, 0x03], "");

        let err = reg.decode(f.path(), DecodeHint::Full).unwrap_err();
        assert!(
            matches!(err, RenderError::FormatNotRecognised(_)),
            "{err:?}"
        );
    }

    #[test]
    fn unrecognised_bytes_with_known_extension_return_handler_error() {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(FailingHandler {
            ext: "jpg",
            msg: "bad jpeg",
        }));
        let f = temp_file_with(&[0x13, 0x37, 0xAB, 0xCD, 0x99, 0x01, 0x02, 0x03], ".jpg");

        let err = reg.decode(f.path(), DecodeHint::Full).unwrap_err();
        match err {
            RenderError::DecodeFailed(msg) => assert_eq!(msg, "bad jpeg"),
            other => panic!("expected DecodeFailed, got {other:?}"),
        }
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
