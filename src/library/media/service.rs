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
use crate::library::resolver::OriginalResolver;

/// Media asset service.
///
/// Owns all media-table operations and the filesystem knowledge needed
/// to resolve original-file paths and clean up files on deletion.
#[derive(Clone)]
pub struct MediaService {
    repo: MediaRepository,
    originals_dir: PathBuf,
    mode: LocalStorageMode,
    recorder: Arc<dyn MutationRecorder>,
    resolver: Arc<dyn OriginalResolver>,
}

impl MediaService {
    pub fn new(
        db: Database,
        originals_dir: PathBuf,
        mode: LocalStorageMode,
        recorder: Arc<dyn MutationRecorder>,
        resolver: Arc<dyn OriginalResolver>,
    ) -> Self {
        Self {
            repo: MediaRepository::new(db),
            originals_dir,
            mode,
            recorder,
            resolver,
        }
    }

    // ── Path resolution ─────────────────────────────────────────────

    /// Resolve the original file path for `id`.
    ///
    /// Delegates to the injected [`OriginalResolver`] — local backends
    /// return a filesystem path directly; remote backends may fetch first.
    pub async fn original_path(&self, id: &MediaId) -> Result<Option<PathBuf>, LibraryError> {
        let info = self.repo.resolve_info(id).await?;
        match info {
            Some((rel, filename, external_id)) => {
                self.resolver
                    .resolve(id, &rel, Some(&filename), external_id.as_deref())
                    .await
            }
            None => Ok(None),
        }
    }

    /// Collect original file paths for a batch of IDs (managed mode only).
    ///
    /// Must be called **before** the DB delete — after deletion the
    /// `relative_path` lookup would return `None`.
    pub async fn collect_original_paths(&self, ids: &[MediaId]) -> Vec<(MediaId, PathBuf)> {
        if !matches!(self.mode, LocalStorageMode::Managed) {
            return Vec::new();
        }
        let mut paths = Vec::new();
        for id in ids {
            if let Ok(Some(rel)) = self.repo.relative_path(id).await {
                paths.push((id.clone(), self.originals_dir.join(&rel)));
            }
        }
        paths
    }

    // ── Sync upsert (pull from server, no outbox recording) ────────

    /// Insert or replace a media record from the sync stream.
    pub async fn upsert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.repo.upsert(record).await
    }

    // ── Delegating methods ──────────────────────────────────────────

    pub async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.exists(id).await
    }

    /// Check if an asset with this content hash already exists (dedup).
    pub async fn exists_by_content_hash(&self, hash: &str) -> Result<bool, LibraryError> {
        self.repo.exists_by_content_hash(hash).await
    }

    pub async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.repo.get(id).await
    }

    pub async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.repo.insert(record).await?;
        let file_path = match self.mode {
            LocalStorageMode::Managed => self.originals_dir.join(&record.relative_path),
            LocalStorageMode::Referenced => PathBuf::from(&record.relative_path),
        };
        if let Err(e) = self
            .recorder
            .record(&Mutation::AssetImported {
                id: record.id.clone(),
                file_path,
            })
            .await
        {
            warn!(id = %record.id, error = %e, "failed to record AssetImported mutation");
        }
        Ok(())
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
        if let Err(e) = self
            .recorder
            .record(&Mutation::AssetFavorited {
                ids: ids.to_vec(),
                favorite,
            })
            .await
        {
            warn!(error = %e, "failed to record AssetFavorited mutation");
        }
        Ok(())
    }

    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.trash(ids).await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::AssetTrashed { ids: ids.to_vec() })
            .await
        {
            warn!(error = %e, "failed to record AssetTrashed mutation");
        }
        Ok(())
    }

    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.restore(ids).await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::AssetRestored { ids: ids.to_vec() })
            .await
        {
            warn!(error = %e, "failed to record AssetRestored mutation");
        }
        Ok(())
    }

    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // Capture external_ids before the DB delete removes the rows.
        let ext_map = self.repo.external_ids(ids).await.unwrap_or_default();
        self.repo.delete_permanently(ids).await?;
        let items: Vec<(MediaId, Option<String>)> = ids
            .iter()
            .map(|id| {
                let ext = ext_map
                    .iter()
                    .find(|(lid, _)| lid == id.as_str())
                    .map(|(_, eid)| eid.clone());
                (id.clone(), ext)
            })
            .collect();
        if let Err(e) = self
            .recorder
            .record(&Mutation::AssetDeleted { items })
            .await
        {
            warn!(error = %e, "failed to record AssetDeleted mutation");
        }
        Ok(())
    }

    /// Permanently delete without outbox recording (used by pull sync).
    pub async fn delete_permanently_no_record(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.repo.delete_permanently(ids).await
    }

    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.expired_trash(max_age_secs).await
    }

    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        self.repo.library_stats().await
    }
}
