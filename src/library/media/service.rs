use std::path::PathBuf;
use std::sync::Arc;

use tracing::warn;

use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord};
use super::repository::MediaRepository;
use crate::library::config::LocalStorageMode;
use crate::library::db::{Database, LibraryStats};
use crate::library::error::LibraryError;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Media asset service.
///
/// Owns all media-table operations and the filesystem knowledge needed
/// to resolve original-file paths and clean up files on deletion.
#[derive(Clone)]
pub struct MediaService {
    pub(crate) repo: MediaRepository,
    originals_dir: PathBuf,
    mode: LocalStorageMode,
    recorder: Arc<dyn MutationRecorder>,
}

impl MediaService {
    pub fn new(
        db: Database,
        originals_dir: PathBuf,
        mode: LocalStorageMode,
        recorder: Arc<dyn MutationRecorder>,
    ) -> Self {
        Self {
            repo: MediaRepository::new(db),
            originals_dir,
            mode,
            recorder,
        }
    }

    // ── Path resolution ─────────────────────────────────────────────

    /// Absolute filesystem path to the original file for `id`.
    ///
    /// In **Referenced** mode the DB stores the absolute (portal) path.
    /// In **Managed** mode the DB stores a path relative to `originals/`.
    pub async fn original_path(&self, id: &MediaId) -> Result<Option<PathBuf>, LibraryError> {
        let stored = self.repo.relative_path(id).await?;
        Ok(stored.map(|p| match self.mode {
            LocalStorageMode::Referenced => PathBuf::from(p),
            LocalStorageMode::Managed => self.originals_dir.join(p),
        }))
    }

    /// Remove the original file from disk (managed mode only).
    ///
    /// In referenced mode the file belongs to the user — never deleted.
    pub async fn remove_original(&self, id: &MediaId) {
        if let LocalStorageMode::Managed = self.mode {
            if let Ok(Some(rel)) = self.repo.relative_path(id).await {
                let full = self.originals_dir.join(&rel);
                if let Err(e) = tokio::fs::remove_file(&full).await {
                    warn!(id = %id, path = %full.display(), "failed to remove original: {e}");
                }
            }
        }
    }

    // ── Delegating methods ──────────────────────────────────────────

    pub async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.exists(id).await
    }

    pub async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.repo.get(id).await
    }

    pub async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.repo.insert(record).await
    }

    pub async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.repo.list(filter, cursor, limit).await
    }

    pub async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        self.repo.set_favorite(ids, favorite).await?;
        self.recorder
            .record(&Mutation::AssetFavorited {
                ids: ids.to_vec(),
                favorite,
            })
            .await
    }

    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.trash(ids).await?;
        self.recorder
            .record(&Mutation::AssetTrashed {
                ids: ids.to_vec(),
            })
            .await
    }

    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.restore(ids).await?;
        self.recorder
            .record(&Mutation::AssetRestored {
                ids: ids.to_vec(),
            })
            .await
    }

    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.delete_permanently(ids).await?;
        self.recorder
            .record(&Mutation::AssetDeleted {
                ids: ids.to_vec(),
            })
            .await
    }

    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.expired_trash(max_age_secs).await
    }

    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        self.repo.library_stats().await
    }
}
