//! Thin forwarding layer for metadata operations on `Database`.
//!
//! All SQL lives in `MetadataRepository` (`library/metadata/repository.rs`).
//! This module exists so that code holding a `Database` can still call
//! metadata methods directly. It will be removed when all features are
//! converted to repositories.

use crate::library::error::LibraryError;
use crate::library::metadata::repository::MetadataRepository;
use crate::library::metadata::MediaMetadataRecord;

use super::Database;

impl Database {
    /// Forwarding shim — delegates to `MetadataRepository`.
    pub async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        MetadataRepository::new(self.clone()).insert(record).await
    }
}
