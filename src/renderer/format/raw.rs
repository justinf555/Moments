// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::renderer::error::RenderError;
use crate::renderer::format::registry::{DecodeHint, FormatHandler};

/// Decodes RAW camera files via the `rawler` crate.
///
/// The decode chain order depends on [`DecodeHint`]:
///
/// * [`DecodeHint::Full`] — `full_image` (full demosaic) → `preview_image`
///   → `thumbnail_image`. Used by the viewer's full-resolution path so
///   editing operations work on real sensor data when available.
/// * [`DecodeHint::Thumbnail`] — `thumbnail_image` → `preview_image` →
///   `full_image`. Used by the import thumbnail pipeline. The embedded
///   JPEGs are typically 160-2048 px and decode in milliseconds with a
///   ~5 MB transient allocation, vs ~150 MB+ for a 50 MP demosaic. See
///   issue #617.
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

    fn decode(&self, path: &Path, hint: DecodeHint) -> Result<image::DynamicImage, RenderError> {
        let source = RawSource::new(path)
            .map_err(|e| RenderError::DecodeFailed(format!("failed to open RAW file: {e}")))?;

        let decoder = rawler::get_decoder(&source)
            .map_err(|e| RenderError::DecodeFailed(format!("no RAW decoder for file: {e}")))?;

        let params = RawDecodeParams::default();

        let thumb = || {
            decoder.thumbnail_image(&source, &params).map_err(|e| {
                RenderError::DecodeFailed(format!("RAW thumbnail extraction failed: {e}"))
            })
        };
        let preview = || {
            decoder.preview_image(&source, &params).map_err(|e| {
                RenderError::DecodeFailed(format!("RAW preview extraction failed: {e}"))
            })
        };
        let full = || {
            decoder
                .full_image(&source, &params)
                .map_err(|e| RenderError::DecodeFailed(format!("RAW full decode failed: {e}")))
        };

        // Fall through past `Err(...)` from earlier options too — rawler
        // can fail on a particular embedded JPEG or demosaic variant while
        // a different path on the same file still works. Only the last
        // option in each chain propagates its error, so a useful message
        // surfaces when nothing succeeded.
        match hint {
            DecodeHint::Thumbnail => {
                if let Ok(Some(img)) = thumb() {
                    return Ok(img);
                }
                if let Ok(Some(img)) = preview() {
                    return Ok(img);
                }
                full()?
            }
            DecodeHint::Full => {
                if let Ok(Some(img)) = full() {
                    return Ok(img);
                }
                if let Ok(Some(img)) = preview() {
                    return Ok(img);
                }
                thumb()?
            }
        }
        .ok_or_else(|| RenderError::DecodeFailed("RAW decoder returned no image".to_string()))
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
