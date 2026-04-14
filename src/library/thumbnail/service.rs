use std::path::PathBuf;

use async_trait::async_trait;

use super::model::ThumbnailStatus;
use super::repository::ThumbnailRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Feature trait for thumbnail path resolution and persistence.
///
/// Implemented by every backend that manages thumbnails. The GTK layer
/// calls `thumbnail_path` to obtain the filesystem path for an asset's
/// thumbnail image without touching the database.
#[async_trait]
pub trait LibraryThumbnail: Send + Sync {
    /// Compute the path for an asset's thumbnail without hitting the DB.
    ///
    /// Layout: `<thumbnails_dir>/{shard1}/{shard2}/{media_id}.webp`
    /// where `shard1 = id[..2]` and `shard2 = id[2..4]`.
    fn thumbnail_path(&self, id: &MediaId) -> PathBuf;

    /// Insert a `Pending` row for `id`. No-op if a row already exists.
    async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Mark a thumbnail `Ready` and record its `file_path` relative to the
    /// bundle's `thumbnails/` directory.
    async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError>;

    /// Mark a thumbnail `Failed`.
    async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Return the stored [`ThumbnailStatus`] for `id`, or `None` if no row exists.
    async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError>;
}

/// Compute the two-level sharded thumbnail path.
///
/// Extracted as a free function so both the service and internal
/// components (thumbnailer, sync downloader) can use the same logic.
pub fn sharded_thumbnail_path(thumbnails_dir: &std::path::Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    if hex.len() < 4 {
        return thumbnails_dir.join(format!("{hex}.webp"));
    }
    thumbnails_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.webp"))
}

/// Local-first thumbnail service.
///
/// Implements [`LibraryThumbnail`] by delegating DB ops to
/// [`ThumbnailRepository`] and path resolution to [`sharded_thumbnail_path`].
#[derive(Clone)]
pub struct ThumbnailService {
    pub(crate) repo: ThumbnailRepository,
    thumbnails_dir: PathBuf,
}

impl ThumbnailService {
    pub fn new(db: Database, thumbnails_dir: PathBuf) -> Self {
        Self {
            repo: ThumbnailRepository::new(db),
            thumbnails_dir,
        }
    }
}

#[async_trait]
impl LibraryThumbnail for ThumbnailService {
    fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        sharded_thumbnail_path(&self.thumbnails_dir, id)
    }

    async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.insert_pending(id).await
    }

    async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        self.repo.set_ready(id, file_path, generated_at).await
    }

    async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.set_failed(id).await
    }

    async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        self.repo.status(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sharded_path_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcdef1234567890.webp"));
    }

    #[test]
    fn sharded_path_short_id_no_panic() {
        let id = MediaId::new("ab".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab.webp"));
    }

    #[test]
    fn sharded_path_empty_id_no_panic() {
        let id = MediaId::new(String::new());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/.webp"));
    }

    #[test]
    fn sharded_path_exactly_four_chars() {
        let id = MediaId::new("abcd".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcd.webp"));
    }
}
