//! Original file resolution trait.
//!
//! Abstracts how the original asset file is located or fetched.
//! Local backends resolve a filesystem path directly; Immich backends
//! check a local cache first and fetch from the server on miss.

use std::path::PathBuf;

use async_trait::async_trait;

use super::error::LibraryError;
use super::media::MediaId;

/// Resolves the original file for a media asset.
///
/// Injected into [`super::media::MediaService`] at construction time.
#[async_trait]
pub trait OriginalResolver: Send + Sync {
    /// Return the filesystem path to the original file.
    ///
    /// For local backends this is immediate. For remote backends this
    /// may involve downloading the file to a local cache first.
    async fn resolve(
        &self,
        id: &MediaId,
        relative_path: &str,
        original_filename: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError>;
}

/// Local filesystem resolver — returns the path directly, no network.
pub struct LocalResolver {
    originals_dir: PathBuf,
    mode: super::config::LocalStorageMode,
}

impl LocalResolver {
    pub fn new(originals_dir: PathBuf, mode: super::config::LocalStorageMode) -> Self {
        Self {
            originals_dir,
            mode,
        }
    }
}

#[async_trait]
impl OriginalResolver for LocalResolver {
    async fn resolve(
        &self,
        _id: &MediaId,
        relative_path: &str,
        _original_filename: Option<&str>,
        _external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let path = match self.mode {
            super::config::LocalStorageMode::Referenced => PathBuf::from(relative_path),
            super::config::LocalStorageMode::Managed => self.originals_dir.join(relative_path),
        };
        if path.exists() {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn managed_mode_joins_originals_dir() {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();

        // Create a file at originals/2025/01/photo.jpg
        let photo_dir = originals.join("2025").join("01");
        std::fs::create_dir_all(&photo_dir).unwrap();
        let photo_path = photo_dir.join("photo.jpg");
        std::fs::write(&photo_path, b"jpeg data").unwrap();

        let resolver = LocalResolver::new(
            originals.clone(),
            super::super::config::LocalStorageMode::Managed,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "2025/01/photo.jpg", None, None)
            .await
            .unwrap();
        assert_eq!(result, Some(photo_path));
    }

    #[tokio::test]
    async fn managed_mode_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = LocalResolver::new(
            dir.path().to_path_buf(),
            super::super::config::LocalStorageMode::Managed,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "nonexistent/photo.jpg", None, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn referenced_mode_uses_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let photo_path = dir.path().join("my_photo.jpg");
        std::fs::write(&photo_path, b"jpeg data").unwrap();

        let resolver = LocalResolver::new(
            dir.path().to_path_buf(),
            super::super::config::LocalStorageMode::Referenced,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        // In referenced mode, relative_path is actually the absolute path.
        let result = resolver
            .resolve(&id, photo_path.to_str().unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(result, Some(photo_path));
    }

    #[tokio::test]
    async fn referenced_mode_returns_none_for_missing() {
        let resolver = LocalResolver::new(
            std::path::PathBuf::from("/unused"),
            super::super::config::LocalStorageMode::Referenced,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "/absolutely/nonexistent/photo.jpg", None, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn managed_mode_ignores_media_id() {
        // The LocalResolver does not use the MediaId for path resolution.
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.jpg");
        std::fs::write(&photo, b"data").unwrap();

        let resolver = LocalResolver::new(
            dir.path().to_path_buf(),
            super::super::config::LocalStorageMode::Managed,
        );

        let id1 = super::super::media::MediaId::new("id-a".to_string());
        let id2 = super::super::media::MediaId::new("id-b".to_string());

        let r1 = resolver
            .resolve(&id1, "photo.jpg", None, None)
            .await
            .unwrap();
        let r2 = resolver
            .resolve(&id2, "photo.jpg", None, None)
            .await
            .unwrap();
        assert_eq!(r1, r2);
    }
}
