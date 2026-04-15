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
