use std::path::Path;

use crate::renderer::error::RenderError;
use crate::renderer::format::registry::{DecodeHint, FormatHandler};

/// Decodes all formats supported by the [`image`] crate via `image::open`.
///
/// After [`libheif_rs::integration::register_all_decoding_hooks`] is called
/// at startup, `image::open` transparently handles HEIC and HEIF files too —
/// so those extensions are claimed here rather than in a separate handler.
pub struct StandardHandler;

impl FormatHandler for StandardHandler {
    fn extensions(&self) -> &[&str] {
        &["jpg", "jpeg", "png", "webp", "tiff", "tif", "heic", "heif"]
    }

    fn decode(&self, path: &Path, _hint: DecodeHint) -> Result<image::DynamicImage, RenderError> {
        // The hint is ignored for now. JPEG DCT-scale decoding (1/2, 1/4, 1/8)
        // would let us honour DecodeHint::Thumbnail at near-zero cost — see
        // issue #617 follow-up.
        //
        // Use Reader with format guessing instead of image::open() so that
        // extensionless files (UUID-sharded originals) are decoded via magic
        // bytes rather than relying on the file extension.
        image::ImageReader::open(path)
            .map_err(|e| RenderError::DecodeFailed(e.to_string()))?
            .with_guessed_format()
            .map_err(|e| RenderError::DecodeFailed(e.to_string()))?
            .decode()
            .map_err(|e| RenderError::DecodeFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercase() {
        for ext in StandardHandler.extensions() {
            assert_eq!(*ext, ext.to_lowercase());
        }
    }

    #[test]
    fn extensions_include_heif_formats() {
        let exts = StandardHandler.extensions();
        assert!(exts.contains(&"heic"));
        assert!(exts.contains(&"heif"));
    }
}
