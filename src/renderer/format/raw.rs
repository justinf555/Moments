use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::renderer::error::RenderError;
use crate::renderer::format::registry::FormatHandler;

/// Decodes RAW camera files via the `rawler` crate.
///
/// Attempts to extract an embedded JPEG preview in this order:
///   1. `thumbnail_image` — smallest embedded preview
///   2. `preview_image`   — larger embedded preview
///
/// Thumbnails are cached on disk after import, so the decode cost is
/// paid only once per asset.
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

    fn decode(&self, path: &Path) -> Result<image::DynamicImage, RenderError> {
        let source = RawSource::new(path)
            .map_err(|e| RenderError::DecodeFailed(format!("failed to open RAW file: {e}")))?;

        let decoder = rawler::get_decoder(&source)
            .map_err(|e| RenderError::DecodeFailed(format!("no RAW decoder for file: {e}")))?;

        let params = RawDecodeParams::default();

        // Full demosaicing — highest quality. Thumbnails are cached on disk
        // so this cost is paid only once per import.
        if let Some(img) = decoder
            .full_image(&source, &params)
            .map_err(|e| RenderError::DecodeFailed(format!("RAW full decode failed: {e}")))?
        {
            return Ok(img);
        }

        // Embedded full-size preview — fast fallback if demosaic unavailable.
        if let Some(img) = decoder
            .preview_image(&source, &params)
            .map_err(|e| RenderError::DecodeFailed(format!("RAW preview extraction failed: {e}")))?
        {
            return Ok(img);
        }

        // Last resort: smallest embedded thumbnail.
        if let Some(img) = decoder.thumbnail_image(&source, &params).map_err(|e| {
            RenderError::DecodeFailed(format!("RAW thumbnail extraction failed: {e}"))
        })? {
            return Ok(img);
        }

        Err(RenderError::DecodeFailed(
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
            assert_eq!(
                *ext,
                ext.to_lowercase(),
                "extension {ext:?} is not lowercase"
            );
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
