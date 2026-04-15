use super::model::MediaMetadataRecord;
use super::repository::MetadataRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Media metadata (EXIF detail) service.
#[derive(Clone)]
pub struct MetadataService {
    repo: MetadataRepository,
}

impl MetadataService {
    pub fn new(db: Database) -> Self {
        Self {
            repo: MetadataRepository::new(db),
        }
    }

    pub async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.repo.insert(record).await
    }

    /// Insert or replace metadata from the sync stream.
    pub async fn upsert_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.repo.upsert(record).await
    }

    pub async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        self.repo.get(id).await
    }
}
