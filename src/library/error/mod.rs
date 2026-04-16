use gettextrs::gettext;

use crate::UserFacingError;

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

    #[error("render error: {0}")]
    Render(#[from] crate::renderer::error::RenderError),

    #[error("immich error: {0}")]
    Immich(String),

    #[error("server unreachable: {0}")]
    Connectivity(String),
}

impl UserFacingError for LibraryError {
    fn to_user_facing(&self) -> String {
        match self {
            Self::Db(_) => gettext("A database error occurred"),
            Self::Io(_) => gettext("Could not read or write file"),
            Self::Runtime(_) => gettext("An unexpected error occurred"),
            // Remaining variants — fall back to Display for now.
            _ => self.to_string(),
        }
    }
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

    #[test]
    fn user_facing_db_is_non_technical() {
        let sql_err = sqlx::Error::RowNotFound;
        let err = LibraryError::from(sql_err);
        let msg = err.to_user_facing();
        assert!(!msg.contains("RowNotFound"));
        assert!(!msg.is_empty());
    }

    #[test]
    fn user_facing_runtime_is_non_technical() {
        let err = LibraryError::Runtime("task panicked".to_string());
        let msg = err.to_user_facing();
        assert!(!msg.contains("panicked"));
        assert!(!msg.is_empty());
    }

    #[test]
    fn user_facing_fallback_uses_display() {
        let err = LibraryError::Bundle("corrupt header".to_string());
        let msg = err.to_user_facing();
        // Fallback variants use Display until they get their own message.
        assert_eq!(msg, err.to_string());
    }
}
