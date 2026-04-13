use std::path::PathBuf;

use async_trait::async_trait;

use super::error::LibraryError;
use super::media::MediaId;

/// Feature trait for detail-view data access.
///
/// Separated from [`super::media::LibraryMedia`] because `original_path`
/// requires knowledge of the bundle's filesystem layout, which the `Database`
/// layer does not have.
#[async_trait]
pub trait LibraryViewer: Send + Sync {
    /// Absolute filesystem path to the original file for `id`.
    ///
    /// Returns `None` if no media record exists for `id`. The path is
    /// guaranteed to be absolute and within the library bundle.
    async fn original_path(&self, id: &MediaId) -> Result<Option<PathBuf>, LibraryError>;
}
