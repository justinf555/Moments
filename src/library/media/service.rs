use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::event::MediaEvent;
use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord};
use super::repository::MediaRepository;
use crate::event_emitter::EventEmitter;
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
///
/// Holds an [`EventEmitter<MediaEvent>`] to notify clients of state
/// changes. Each call to [`subscribe`] returns a fresh receiver; every
/// emitted event is delivered to every live subscriber.
///
/// [`subscribe`]: MediaService::subscribe
#[derive(Clone)]
pub struct MediaService {
    repo: MediaRepository,
    originals_dir: PathBuf,
    mode: LocalStorageMode,
    recorder: Arc<dyn MutationRecorder>,
    resolver: Arc<dyn OriginalResolver>,
    events: EventEmitter<MediaEvent>,
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
            events: EventEmitter::new(),
        }
    }

    /// Register a new subscriber. Every emitted event is delivered to every
    /// live subscriber.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<MediaEvent> {
        self.events.subscribe()
    }

    /// Broadcast an event to every live subscriber.
    fn emit(&self, event: MediaEvent) {
        self.events.emit(event);
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
    ///
    /// Pre-queries existence so the emitted event distinguishes a new row
    /// (`Added`) from a refreshed row (`Updated`).
    ///
    /// Special case: when a locally-imported asset is uploaded to Immich
    /// and streamed back with the server's UUID as the `id`, the
    /// repository deletes the old local-keyed row before inserting the
    /// new server-keyed one. We emit a single [`MediaEvent::Replaced`]
    /// so UI models can swap entries in place without triggering the
    /// `Removed`-path side effects (sidebar trash badge, selection
    /// exit) — see #610.
    pub async fn upsert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        let existed = self.repo.exists(&record.id).await?;
        let replaced = self.repo.upsert(record).await?;
        if let Some(old) = replaced {
            self.emit(MediaEvent::Replaced {
                old,
                new: record.id.clone(),
            });
        } else if existed {
            self.emit(MediaEvent::Updated(vec![record.id.clone()]));
        } else {
            self.emit(MediaEvent::Added(vec![record.id.clone()]));
        }
        Ok(())
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

    /// Fetch media items for a batch of IDs in one query.
    ///
    /// Used by `MediaClientV2`'s event listener to reconcile tracked models
    /// after a batched `MediaEvent::Added` or `MediaEvent::Updated` — one
    /// DB roundtrip regardless of batch size. IDs that have been deleted
    /// since the event fired are absent from the result.
    pub async fn get_media_items(&self, ids: &[MediaId]) -> Result<Vec<MediaItem>, LibraryError> {
        self.repo.get_many(ids).await
    }

    pub async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.repo.insert(record).await?;
        self.emit(MediaEvent::Added(vec![record.id.clone()]));
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
        self.emit(MediaEvent::Updated(ids.to_vec()));
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
        self.emit(MediaEvent::Updated(ids.to_vec()));
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
        self.emit(MediaEvent::Updated(ids.to_vec()));
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
        self.emit(MediaEvent::Removed(ids.to_vec()));
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
        self.repo.delete_permanently(ids).await?;
        self.emit(MediaEvent::Removed(ids.to_vec()));
        Ok(())
    }

    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.expired_trash(max_age_secs).await
    }

    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        self.repo.library_stats().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, record_with_taken_at};
    use crate::library::media::MediaId;
    use crate::library::resolver::LocalResolver;
    use crate::sync::outbox::NoOpRecorder;

    async fn make_service() -> (tempfile::TempDir, MediaService) {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();
        let db = open_test_db(dir.path()).await;
        let svc = MediaService::new(
            db,
            originals.clone(),
            LocalStorageMode::Managed,
            Arc::new(NoOpRecorder),
            Arc::new(LocalResolver::new(originals, LocalStorageMode::Managed)),
        );
        (dir, svc)
    }

    /// Drain a receiver into a Vec, but stop after a short timeout so the
    /// test doesn't hang waiting for a hypothetical extra event.
    async fn drain(rx: &mut mpsc::UnboundedReceiver<MediaEvent>) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Some(e)) => events.push(e),
                Ok(None) => break, // channel closed
                Err(_) => break,   // timeout — no more events
            }
        }
        events
    }

    /// Issue #610: when an upload-then-sync round-trip causes the
    /// repository to replace a local-keyed row with a server-keyed one,
    /// the service emits a single `Replaced { old, new }` event so UI
    /// models can swap entries in place — without triggering the
    /// `Removed`-path side effects (sidebar trash badge, selection exit).
    #[tokio::test]
    async fn upsert_media_emits_replaced_when_local_row_swapped_for_server() {
        let (_dir, svc) = make_service().await;
        let mut rx = svc.subscribe();

        let local_id = MediaId::new("local-uuid-aaaaaaaaaaaaaaaaaaaaaaaa".to_string());
        let server_id = MediaId::new("server-uuid-bbbbbbbbbbbbbbbbbbbbbbb".to_string());

        // 1) Local import where push has already assigned the server's
        //    UUID as external_id (i.e. the row state immediately before
        //    the sync stream replays the asset).
        let mut local_record =
            record_with_taken_at(local_id.clone(), "local/photo.jpg", Some(1_000));
        local_record.external_id = Some(server_id.as_str().to_string());
        svc.insert_media(&local_record).await.unwrap();

        // 2) Drop the Added event from the import so we only see what
        //    upsert emits.
        let _ = drain(&mut rx).await;

        // 3) Pull-sync now upserts the asset keyed on the server UUID.
        let mut from_server =
            record_with_taken_at(server_id.clone(), "server/photo.jpg", Some(1_000));
        from_server.external_id = Some(server_id.as_str().to_string());
        svc.upsert_media(&from_server).await.unwrap();

        let events = drain(&mut rx).await;
        assert_eq!(events.len(), 1, "expected single Replaced; got {events:?}");
        match &events[0] {
            MediaEvent::Replaced { old, new } => {
                assert_eq!(old, &local_id);
                assert_eq!(new, &server_id);
            }
            other => panic!("expected Replaced; got {other:?}"),
        }
    }

    /// Plain sync upsert of a brand-new row emits a single `Added`.
    #[tokio::test]
    async fn upsert_media_emits_added_for_fresh_row() {
        let (_dir, svc) = make_service().await;
        let mut rx = svc.subscribe();

        let record = record_with_taken_at(
            MediaId::new("fresh-server-uuid-cccccccccccccccccc".to_string()),
            "fresh/photo.jpg",
            Some(2_000),
        );
        svc.upsert_media(&record).await.unwrap();

        let events = drain(&mut rx).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], MediaEvent::Added(ids) if ids.len() == 1));
    }

    /// Re-syncing an asset that already exists by id emits a single
    /// `Updated` (no replacement happened).
    #[tokio::test]
    async fn upsert_media_emits_updated_for_known_id() {
        let (_dir, svc) = make_service().await;

        let id = MediaId::new("known-id-dddddddddddddddddddddddddd".to_string());
        let record = record_with_taken_at(id.clone(), "known/photo.jpg", Some(3_000));
        svc.upsert_media(&record).await.unwrap();

        let mut rx = svc.subscribe();
        // Re-upsert the same id.
        svc.upsert_media(&record).await.unwrap();

        let events = drain(&mut rx).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], MediaEvent::Updated(ids) if ids == &[id]));
    }
}
