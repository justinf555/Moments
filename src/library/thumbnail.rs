use std::path::PathBuf;

use async_trait::async_trait;

use super::error::LibraryError;
use super::media::MediaId;

/// Processing state of a single thumbnail, mirroring the `thumbnails.status` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ThumbnailStatus {
    Pending = 0,
    Ready   = 1,
    Failed  = 2,
}

impl ThumbnailStatus {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// Feature trait for thumbnail path resolution and persistence.
///
/// Implemented by every backend that manages thumbnails. The GTK layer
/// calls `thumbnail_path` to obtain the filesystem path for an asset's
/// thumbnail image without touching the database.
///
/// `Database` implements the three async persistence methods.
/// `LocalLibrary` provides `thumbnail_path` (pure path construction using
/// its `thumbnails_dir`) and delegates the async methods to its `Database`.
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
    async fn thumbnail_status(&self, id: &MediaId) -> Result<Option<ThumbnailStatus>, LibraryError>;
}

/// Compute the two-level sharded thumbnail path.
///
/// Extracted as a free function so both `LocalLibrary` and `ThumbnailJob`
/// can use the same logic without coupling to a specific type.
pub fn sharded_thumbnail_path(thumbnails_dir: &std::path::Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    thumbnails_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.webp"))
}
