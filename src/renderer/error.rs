//! Render pipeline errors.

use std::path::PathBuf;

use gettextrs::gettext;

use crate::UserFacingError;

/// Errors that can occur during the render pipeline.
#[derive(Debug)]
pub enum RenderError {
    /// No handler recognised the file format (magic bytes + extension both failed).
    FormatNotRecognised(PathBuf),
    /// A handler was found but decoding failed (corrupt file, unsupported codec).
    DecodeFailed(String),
    /// Output encoding failed (WebP write).
    EncodeFailed(String),
    /// File I/O error (read, write, create directory).
    Io(std::io::Error),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FormatNotRecognised(path) => {
                write!(f, "format not recognised: {}", path.display())
            }
            Self::DecodeFailed(msg) => write!(f, "decode failed: {msg}"),
            Self::EncodeFailed(msg) => write!(f, "encode failed: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl UserFacingError for RenderError {
    fn to_user_facing(&self) -> String {
        match self {
            Self::FormatNotRecognised(_) => gettext("Unsupported image format"),
            Self::DecodeFailed(_) => gettext("Could not decode image"),
            Self::EncodeFailed(_) => gettext("Could not save image"),
            Self::Io(_) => gettext("Could not read file"),
        }
    }
}

impl From<std::io::Error> for RenderError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format_not_recognised() {
        let err = RenderError::FormatNotRecognised(PathBuf::from("/photos/unknown"));
        assert!(err.to_string().contains("format not recognised"));
        assert!(err.to_string().contains("/photos/unknown"));
    }

    #[test]
    fn display_decode_failed() {
        let err = RenderError::DecodeFailed("corrupt JPEG".into());
        assert!(err.to_string().contains("corrupt JPEG"));
    }

    #[test]
    fn user_facing_is_non_technical() {
        let err = RenderError::DecodeFailed("InvalidImageData(42)".into());
        let msg = err.to_user_facing();
        // User message should NOT contain the technical detail.
        assert!(!msg.contains("InvalidImageData"));
        assert!(!msg.is_empty());
    }

    #[test]
    fn io_error_converts() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err = RenderError::from(io_err);
        assert!(matches!(err, RenderError::Io(_)));
    }
}
