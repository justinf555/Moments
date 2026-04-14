use async_trait::async_trait;

use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord};
use super::repository::MediaRepository;
use crate::library::db::{Database, LibraryStats};
use crate::library::error::LibraryError;

/// Feature trait for media asset persistence.
///
/// Implemented by every backend that stores media records.
///
/// The GTK layer calls these methods through the `Library` supertrait —
/// it never touches `Database` or `MediaRepository` directly.
#[async_trait]
pub trait LibraryMedia: Send + Sync {
    /// Return `true` if an asset with this [`MediaId`] is already stored.
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError>;

    /// Fetch a single media item by ID.
    ///
    /// Used for incremental grid updates without full reload.
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError>;

    /// Persist a newly imported media asset record.
    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError>;

    /// Return a page of [`MediaItem`]s in reverse chronological order.
    ///
    /// Pass `cursor: None` for the first page. Pass the cursor from a previous
    /// result to fetch the next page. Returns an empty `Vec` when exhausted.
    ///
    /// Items without a `taken_at` date sort to the end (treated as timestamp 0).
    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError>;

    /// Set or clear the favourite flag on one or more assets.
    async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError>;

    /// Move assets to the trash (soft delete).
    ///
    /// Sets `is_trashed = 1` and records the current timestamp in `trashed_at`.
    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Restore trashed assets back to the library.
    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Permanently delete assets: removes the DB row, original file, and thumbnail.
    ///
    /// This is irreversible. Callers should confirm with the user before calling.
    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Return IDs of items trashed longer than `max_age_secs` ago.
    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError>;

    /// Return aggregate library statistics for the preferences overview.
    async fn library_stats(&self) -> Result<LibraryStats, LibraryError>;
}

/// Local-first media service.
///
/// Implements [`LibraryMedia`] by delegating to [`MediaRepository`].
/// This is the canonical implementation — both the unified Library and
/// individual providers delegate to it.
#[derive(Clone)]
pub struct MediaService {
    pub(crate) repo: MediaRepository,
}

impl MediaService {
    pub fn new(db: Database) -> Self {
        Self {
            repo: MediaRepository::new(db),
        }
    }
}

#[async_trait]
impl LibraryMedia for MediaService {
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.exists(id).await
    }

    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.repo.get(id).await
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.repo.insert(record).await
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.repo.list(filter, cursor, limit).await
    }

    async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        self.repo.set_favorite(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.delete_permanently(ids).await
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.expired_trash(max_age_secs).await
    }

    async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        self.repo.library_stats().await
    }
}
