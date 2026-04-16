//! Thin forwarding layer for thumbnail operations on `Database`.
//!
//! All SQL lives in `ThumbnailRepository` (`library/thumbnail/repository.rs`).
//! This module exists so that code holding a `Database` (thumbnailer,
//! sync downloader) can still call thumbnail methods directly.
//! It will be removed when all features are converted to repositories.

use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::thumbnail::repository::ThumbnailRepository;
use crate::library::thumbnail::ThumbnailStatus;

use super::Database;

impl Database {
    pub async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        ThumbnailRepository::new(self.clone())
            .insert_pending(id)
            .await
    }

    pub async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        ThumbnailRepository::new(self.clone())
            .set_ready(id, file_path, generated_at)
            .await
    }

    pub async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        ThumbnailRepository::new(self.clone()).set_failed(id).await
    }

    pub async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        ThumbnailRepository::new(self.clone()).status(id).await
    }
}
