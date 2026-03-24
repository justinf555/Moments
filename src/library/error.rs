/// Errors that can arise from any library operation.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Bundle error: {0}")]
    Bundle(String),

    #[error("Backend not supported")]
    BackendNotSupported,

    #[error("unknown backend type '{0}' in library.toml")]
    InvalidBackend(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("thumbnail error: {0}")]
    Thumbnail(String),

    #[error("immich error: {0}")]
    Immich(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = LibraryError::from(io_err);
        assert!(matches!(err, LibraryError::Io(_)));
        assert!(err.to_string().contains("I/O error"));
    }

    #[test]
    fn bundle_error_message() {
        let err = LibraryError::Bundle("corrupt header".to_string());
        assert!(err.to_string().contains("Bundle error"));
        assert!(err.to_string().contains("corrupt header"));
    }

    #[test]
    fn backend_not_supported_message() {
        let err = LibraryError::BackendNotSupported;
        assert!(err.to_string().contains("Backend not supported"));
    }

    #[test]
    fn invalid_backend_includes_name() {
        let err = LibraryError::InvalidBackend("s3".to_string());
        assert!(err.to_string().contains("s3"));
    }
}
