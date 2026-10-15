// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::event::MediaEvent;
use super::model::MediaCursor;
use super::model::MediaFilter;
use super::model::MediaId;
use super::model::MediaItem;
use super::model::MediaRecord;
use super::repository::MediaRepository;
use crate::library::config::LocalStorageMode;
use crate::library::db::{Database, LibraryStats};
use crate::library::error::LibraryError;
use crate::library::event_emitter::EventEmitter;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;
use crate::library::resolver::OriginalResolver;

mod sync;

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

    /// Filesystem directory holding originals for the local library
    /// (under `Managed` mode). Phase C uses this to write newly-rendered
    /// edits at their sharded UUID path before inserting the media row.
    pub fn originals_dir(&self) -> &PathBuf {
        &self.originals_dir
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
            match self.repo.relative_path(id).await {
                Ok(Some(rel)) => paths.push((id.clone(), self.originals_dir.join(&rel))),
                Ok(None) => {}
                // The file is left behind as an orphan; deletion itself goes ahead.
                Err(e) => warn!(%id, "could not look up original path for deletion: {e}"),
            }
        }
        paths
    }

    // ── Delegating methods ──────────────────────────────────────────

    pub async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.exists(id).await
    }

    /// Check if an asset with this content hash already exists (dedup).
    pub async fn exists_by_content_hash(&self, hash: &str) -> Result<bool, LibraryError> {
        self.repo.exists_by_content_hash(hash).await
    }

    /// Fetch the full DB row (including `content_hash` and
    /// `external_id`) for an asset. Phase C uses this in
    /// [`crate::library::Library::save_pixel_edit`] to populate the
    /// embedded XMP block; the column subset returned by
    /// [`Self::get_media_item`] omits those fields.
    pub async fn get_media_record(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaRecord>, LibraryError> {
        self.repo.get_record(id).await
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
        // Issue #224: capture siblings whose stack_id will be SET NULL
        // by the FK cascade so we can emit `MediaEvent::Updated` for
        // them after the delete — otherwise the live grid stays
        // out of sync until restart.
        let freed_siblings = self
            .repo
            .siblings_freed_by_deletion(ids)
            .await
            .unwrap_or_default();
        self.repo.delete_permanently(ids).await?;
        self.emit(MediaEvent::Removed(ids.to_vec()));
        if !freed_siblings.is_empty() {
            self.emit(MediaEvent::Updated(freed_siblings));
        }
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
    use crate::library::media::model::Stack;
    use crate::library::media::MediaId;
    use crate::library::resolver::LocalResolver;
    use crate::sync::outbox::NoOpRecorder;

    pub(super) async fn make_service() -> (tempfile::TempDir, MediaService) {
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
    pub(super) async fn drain(rx: &mut mpsc::UnboundedReceiver<MediaEvent>) -> Vec<MediaEvent> {
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

    /// Sync-then-import dedup: when Immich pulls down an asset with a
    /// `checksum` (SHA-1 base64), it lands in `content_hash`. A local
    /// import of the same bytes hashes to the same value, so the
    /// importer's `exists_by_content_hash` check rejects it as a
    /// duplicate — no second row, no parallel upload, no UNIQUE-index
    /// collision when push later tries to stamp `external_id`.
    #[tokio::test]
    async fn exists_by_content_hash_matches_immich_pulled_row() {
        let (_dir, svc) = make_service().await;

        let server_id = MediaId::new("server-uuid-aaaaaaaaaaaaaaaaaaaaaa".to_string());
        let mut server_row =
            record_with_taken_at(server_id.clone(), "server/photo.jpg", Some(1_000));
        server_row.external_id = Some("immich-uuid".to_string());
        // SHA-1 base64 of "abc" — same value the importer would compute
        // for an identical file.
        server_row.content_hash = Some("qZk+NkcGgWq6PiVxeFDCbJzQ2J0=".to_string());
        svc.upsert_media(&server_row).await.unwrap();

        let dup = svc
            .exists_by_content_hash("qZk+NkcGgWq6PiVxeFDCbJzQ2J0=")
            .await
            .unwrap();
        assert!(dup, "importer must see the pulled row as a duplicate");

        let miss = svc
            .exists_by_content_hash("aGVsbG8gd29ybGQ=")
            .await
            .unwrap();
        assert!(!miss, "an unrelated hash must not match");
    }

    /// Issue #224: deleting a stack primary must emit `Updated` for
    /// the surviving siblings whose `stack_id` was cleared by the FK
    /// cascade — otherwise the live grid stays out of sync.
    #[tokio::test]
    async fn delete_permanently_emits_updated_for_freed_siblings() {
        let (_dir, svc) = make_service().await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        for (id, name) in [(&primary, "p.jpg"), (&sibling, "s.jpg")] {
            svc.insert_media(&record_with_taken_at(id.clone(), name, None))
                .await
                .unwrap();
        }
        svc.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        svc.set_media_stack_id(&primary, "stk").await.unwrap();
        svc.set_media_stack_id(&sibling, "stk").await.unwrap();

        let mut rx = svc.subscribe();
        svc.delete_permanently_no_record(std::slice::from_ref(&primary))
            .await
            .unwrap();

        let events = drain(&mut rx).await;
        let mut saw_removed = false;
        let mut saw_updated_sibling = false;
        for e in &events {
            match e {
                MediaEvent::Removed(ids) if ids.as_slice() == std::slice::from_ref(&primary) => {
                    saw_removed = true
                }
                MediaEvent::Updated(ids) if ids.contains(&sibling) => saw_updated_sibling = true,
                _ => {}
            }
        }
        assert!(saw_removed, "expected Removed for primary; got {events:?}");
        assert!(
            saw_updated_sibling,
            "expected Updated for sibling whose stack_id was cleared by cascade; got {events:?}"
        );
    }
}
