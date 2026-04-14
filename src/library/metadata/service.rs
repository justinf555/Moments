use async_trait::async_trait;

use super::model::MediaMetadataRecord;
use super::repository::MetadataRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Feature trait for media metadata (EXIF detail) persistence.
///
/// The public contract consumed by the UI, sync extensions, and the
/// future Media Portal. Implementations must be `Send + Sync` so they
/// can be shared across the GTK and Tokio executors.
#[async_trait]
pub trait LibraryMetadata: Send + Sync {
    /// Persist the full EXIF detail row. No-op if `record.has_data()` is false.
    async fn insert_media_metadata(&self, record: &MediaMetadataRecord)
        -> Result<(), LibraryError>;

    /// Fetch the full EXIF metadata record for `id`.
    ///
    /// Returns `None` if no metadata row was stored (e.g. the asset has no EXIF
    /// data, or metadata extraction failed silently at import time).
    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError>;
}

/// Local-first metadata service.
///
/// Implements [`LibraryMetadata`] by delegating to [`MetadataRepository`].
/// This is the canonical implementation — both the unified Library and
/// individual providers delegate to it.
#[derive(Clone)]
pub struct MetadataService {
    pub(crate) repo: MetadataRepository,
}

impl MetadataService {
    pub fn new(db: Database) -> Self {
        Self {
            repo: MetadataRepository::new(db),
        }
    }
}

#[async_trait]
impl LibraryMetadata for MetadataService {
    async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.repo.insert(record).await
    }

    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        self.repo.get(id).await
    }
}
