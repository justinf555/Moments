use std::path::Path;

use crate::library::error::LibraryError;
use crate::library::format::registry::FormatHandler;

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

    fn decode(&self, path: &Path) -> Result<image::DynamicImage, LibraryError> {
        // Use Reader with format guessing instead of image::open() so that
        // extensionless files (UUID-sharded originals) are decoded via magic
        // bytes rather than relying on the file extension.
        image::io::Reader::open(path)
            .map_err(|e| LibraryError::Thumbnail(e.to_string()))?
            .with_guessed_format()
            .map_err(|e| LibraryError::Thumbnail(e.to_string()))?
            .decode()
            .map_err(|e| LibraryError::Thumbnail(e.to_string()))
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
