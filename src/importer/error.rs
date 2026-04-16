use crate::library::error::LibraryError;
use crate::renderer::error::RenderError;

/// Errors that can arise during the import pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// File I/O failure (copy, read metadata, open for hashing).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Tokio runtime or spawn_blocking join failure.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// Source file has no filename or invalid path.
    #[error("invalid source: {0}")]
    InvalidSource(String),

    /// Thumbnail decode or encode failure.
    #[error("thumbnail error: {0}")]
    Thumbnail(String),

    /// Builder validation — a required field was not set.
    #[error("builder error: {0}")]
    Builder(String),

    /// A render pipeline step failed (decode, orient, resize).
    #[error("render error: {0}")]
    Render(#[from] RenderError),

    /// A library service call failed (DB insert, duplicate check, etc.).
    #[error("library error: {0}")]
    Library(#[from] LibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = ImportError::from(io_err);
        assert!(matches!(err, ImportError::Io(_)));
    }

    #[test]
    fn library_error_conversion() {
        let lib_err = LibraryError::Runtime("db failed".to_string());
        let err = ImportError::from(lib_err);
        assert!(matches!(err, ImportError::Library(_)));
    }

    #[test]
    fn display_includes_message() {
        let err = ImportError::Builder("missing field".to_string());
        assert!(err.to_string().contains("missing field"));
    }
}
