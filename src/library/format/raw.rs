use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::library::error::LibraryError;
use crate::library::format::registry::FormatHandler;

/// Decodes RAW camera files via the `rawler` crate.
///
/// Attempts to extract an embedded JPEG preview in this order:
///   1. `thumbnail_image` — smallest embedded preview
///   2. `preview_image`   — larger embedded preview
///
/// If neither is available the file is skipped (returns an error), as
/// full demosaicing is too slow and memory-intensive for thumbnailing.
pub struct RawHandler;

impl FormatHandler for RawHandler {
    fn extensions(&self) -> &[&str] {
        // rawler::decoders::supported_extensions() returns uppercase; the registry
        // requires lowercase, so we maintain the lowercased list here.
        &[
            "ari", "arw", "cr2", "cr3", "crm", "crw", "dcr", "dcs", "dng", "erf", "iiq", "kdc",
            "mef", "mos", "mrw", "nef", "nrw", "orf", "ori", "pef", "raf", "raw", "rw2", "rwl",
            "srw", "3fr", "fff", "x3f", "qtk",
        ]
    }

    fn decode(&self, path: &Path) -> Result<image::DynamicImage, LibraryError> {
        let source = RawSource::new(path)
            .map_err(|e| LibraryError::Thumbnail(format!("failed to open RAW file: {e}")))?;

        let decoder = rawler::get_decoder(&source)
            .map_err(|e| LibraryError::Thumbnail(format!("no RAW decoder for file: {e}")))?;

        let params = RawDecodeParams::default();

        // Prefer the smallest embedded preview; fall back through larger previews
        // and finally full demosaicing if no embedded JPEG is present.
        if let Some(img) = decoder
            .thumbnail_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW thumbnail extraction failed: {e}")))?
        {
            return Ok(img);
        }

        if let Some(img) = decoder
            .preview_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW preview extraction failed: {e}")))?
        {
            return Ok(img);
        }

        // Last resort: full demosaicing. Slower and memory-intensive, but ensures
        // every supported RAW file gets a thumbnail rather than silently failing.
        if let Some(img) = decoder
            .full_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW full decode failed: {e}")))?
        {
            return Ok(img);
        }

        Err(LibraryError::Thumbnail(
            "RAW decoder returned no image".to_string(),
        ))
    }
}

impl RawHandler {
    /// Decode at the highest available resolution for full-res viewing.
    ///
    /// Tries full demosaicing first (actual sensor data), falls back to the
    /// largest embedded preview, then the smallest thumbnail as a last resort.
    /// This is the reverse order of [`FormatHandler::decode`] which optimises
    /// for speed (thumbnailing).
    pub fn decode_full_res(&self, path: &Path) -> Result<image::DynamicImage, LibraryError> {
        let source = RawSource::new(path)
            .map_err(|e| LibraryError::Thumbnail(format!("failed to open RAW file: {e}")))?;

        let decoder = rawler::get_decoder(&source)
            .map_err(|e| LibraryError::Thumbnail(format!("no RAW decoder for file: {e}")))?;

        let params = RawDecodeParams::default();

        // Full demosaicing — highest quality, slowest.
        if let Some(img) = decoder
            .full_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW full decode failed: {e}")))?
        {
            return Ok(img);
        }

        // Embedded full-size JPEG preview — fast and usually camera-quality.
        if let Some(img) = decoder
            .preview_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW preview extraction failed: {e}")))?
        {
            return Ok(img);
        }

        // Last resort: smallest embedded thumbnail.
        if let Some(img) = decoder
            .thumbnail_image(&source, &params)
            .map_err(|e| LibraryError::Thumbnail(format!("RAW thumbnail extraction failed: {e}")))?
        {
            return Ok(img);
        }

        Err(LibraryError::Thumbnail(
            "RAW decoder returned no image".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercase() {
        for ext in RawHandler.extensions() {
            assert_eq!(*ext, ext.to_lowercase(), "extension {ext:?} is not lowercase");
        }
    }

    #[test]
    fn extensions_include_common_raw_formats() {
        let exts = RawHandler.extensions();
        for expected in &["cr2", "nef", "arw", "dng", "raf", "rw2", "orf"] {
            assert!(exts.contains(expected), "missing extension: {expected}");
        }
    }
}
