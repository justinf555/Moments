// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::event::MediaEvent;
use super::model::{LibraryStats, MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord};
use super::repository::MediaRepository;
use crate::library::config::LocalStorageMode;
use crate::library::db::{Database, Tx};
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
    db: Database,
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
            repo: MediaRepository::new(db.clone()),
            db,
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

    /// Broadcast the events an `_in_tx` method returned, once the
    /// caller's transaction has committed.
    ///
    /// `_in_tx` methods never emit themselves: an event for a write that
    /// is later rolled back would desync the client models (#655).
    pub(crate) fn emit_committed(&self, events: Vec<MediaEvent>) {
        for event in events {
            self.emit(event);
        }
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
        self.insert_media_with_taken_at_offset(record, None).await
    }

    /// [`Self::insert_media`], also recording the capture-time UTC offset
    /// (minutes east of UTC) from the file's EXIF `OffsetTime*` tag.
    /// The local importer is the only caller that passes an offset; every
    /// other path uses [`Self::insert_media`]. See issue #677.
    pub async fn insert_media_with_taken_at_offset(
        &self,
        record: &MediaRecord,
        taken_at_offset_minutes: Option<i64>,
    ) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        let events = self
            .insert_media_in_tx(&mut tx, record, taken_at_offset_minutes)
            .await?;
        tx.commit().await?;
        self.emit_committed(events);
        Ok(())
    }

    /// Insert a media row and record `AssetImported` on the caller's
    /// transaction. Returns the events to emit once it commits (see
    /// [`Self::emit_committed`]).
    pub(crate) async fn insert_media_in_tx(
        &self,
        tx: &mut Tx,
        record: &MediaRecord,
        taken_at_offset_minutes: Option<i64>,
    ) -> Result<Vec<MediaEvent>, LibraryError> {
        self.repo
            .insert_in_tx(tx, record, taken_at_offset_minutes)
            .await?;
        let file_path = match self.mode {
            LocalStorageMode::Managed => self.originals_dir.join(&record.relative_path),
            LocalStorageMode::Referenced => PathBuf::from(&record.relative_path),
        };
        self.recorder
            .record(
                tx,
                &Mutation::AssetImported {
                    id: record.id.clone(),
                    file_path,
                },
            )
            .await?;
        Ok(vec![MediaEvent::Added(vec![record.id.clone()])])
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
        let mut tx = self.db.begin().await?;
        self.repo.set_favorite_in_tx(&mut tx, ids, favorite).await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AssetFavorited {
                    ids: ids.to_vec(),
                    favorite,
                },
            )
            .await?;
        tx.commit().await?;
        self.emit(MediaEvent::Updated(ids.to_vec()));
        Ok(())
    }

    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        self.repo.trash_in_tx(&mut tx, ids).await?;
        self.recorder
            .record(&mut tx, &Mutation::AssetTrashed { ids: ids.to_vec() })
            .await?;
        tx.commit().await?;
        self.emit(MediaEvent::Updated(ids.to_vec()));
        Ok(())
    }

    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        self.repo.restore_in_tx(&mut tx, ids).await?;
        self.recorder
            .record(&mut tx, &Mutation::AssetRestored { ids: ids.to_vec() })
            .await?;
        tx.commit().await?;
        self.emit(MediaEvent::Updated(ids.to_vec()));
        Ok(())
    }

    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        let events = self.delete_permanently_in_tx(&mut tx, ids).await?;
        tx.commit().await?;
        self.emit_committed(events);
        Ok(())
    }

    /// Delete media rows and record `AssetDeleted` on the caller's
    /// transaction. Returns the events to emit once it commits (see
    /// [`Self::emit_committed`]). Files are not touched — see
    /// [`crate::library::Library::delete_permanently`].
    pub(crate) async fn delete_permanently_in_tx(
        &self,
        tx: &mut Tx,
        ids: &[MediaId],
    ) -> Result<Vec<MediaEvent>, LibraryError> {
        // Capture external_ids before the delete removes the rows; the
        // push handler needs them to delete the asset on the server.
        let ext_map = self.repo.external_ids_in_tx(tx, ids).await?;
        // Issue #224: capture siblings whose stack_id will be SET NULL
        // by the FK cascade so we can emit `MediaEvent::Updated` for
        // them after the delete — otherwise the live grid stays
        // out of sync until restart.
        let freed_siblings = self.repo.siblings_freed_by_deletion_in_tx(tx, ids).await?;
        self.repo.delete_permanently_in_tx(tx, ids).await?;
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
        self.recorder
            .record(tx, &Mutation::AssetDeleted { items })
            .await?;
        let mut events = vec![MediaEvent::Removed(ids.to_vec())];
        if !freed_siblings.is_empty() {
            events.push(MediaEvent::Updated(freed_siblings));
        }
        Ok(events)
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
    use crate::library::media::model::{MediaVisibility, Stack};
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
        svc.upsert_media(&server_row, MediaVisibility::Timeline)
            .await
            .unwrap();

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

    // ── Transactional outbox (#655) ─────────────────────────────────

    use crate::library::recorder::tests::{outbox_count, FailingRecorder};

    /// A service whose recorder writes its outbox rows and then fails,
    /// and a pre-inserted row to mutate.
    async fn failing_service() -> (tempfile::TempDir, MediaService, Database, MediaId) {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("victim".to_string());
        MediaRepository::new(db.clone())
            .insert(&record_with_taken_at(id.clone(), "a/victim.jpg", Some(1)))
            .await
            .unwrap();
        let svc = MediaService::new(
            db.clone(),
            originals.clone(),
            LocalStorageMode::Managed,
            Arc::new(FailingRecorder::always(db.clone())),
            Arc::new(LocalResolver::new(originals, LocalStorageMode::Managed)),
        );
        (dir, svc, db, id)
    }

    #[tokio::test]
    async fn recorder_failure_rolls_back_trash_and_favorite() {
        let (_dir, svc, db, id) = failing_service().await;
        let mut rx = svc.subscribe();
        let ids = std::slice::from_ref(&id);

        assert!(svc.trash(ids).await.is_err());
        assert!(svc.set_favorite(ids, true).await.is_err());

        let item = svc.get_media_item(&id).await.unwrap().unwrap();
        assert!(!item.is_trashed, "trash must roll back");
        assert!(!item.is_favorite, "favourite must roll back");
        assert_eq!(outbox_count(&db).await, 0);
        assert!(drain(&mut rx).await.is_empty(), "no event for a rollback");
    }

    #[tokio::test]
    async fn recorder_failure_rolls_back_restore() {
        let (_dir, svc, db, id) = failing_service().await;
        MediaRepository::new(db.clone())
            .trash(std::slice::from_ref(&id))
            .await
            .unwrap();
        let mut rx = svc.subscribe();

        assert!(svc.restore(std::slice::from_ref(&id)).await.is_err());

        let item = svc.get_media_item(&id).await.unwrap().unwrap();
        assert!(item.is_trashed, "restore must roll back");
        assert_eq!(outbox_count(&db).await, 0);
        assert!(drain(&mut rx).await.is_empty());
    }

    #[tokio::test]
    async fn recorder_failure_rolls_back_insert_and_delete() {
        let (_dir, svc, db, id) = failing_service().await;
        let mut rx = svc.subscribe();

        let new_id = MediaId::new("fresh".to_string());
        assert!(svc
            .insert_media(&record_with_taken_at(
                new_id.clone(),
                "a/fresh.jpg",
                Some(2)
            ))
            .await
            .is_err());
        assert!(svc.get_media_item(&new_id).await.unwrap().is_none());

        assert!(svc
            .delete_permanently(std::slice::from_ref(&id))
            .await
            .is_err());
        assert!(svc.get_media_item(&id).await.unwrap().is_some());

        assert_eq!(outbox_count(&db).await, 0);
        assert!(drain(&mut rx).await.is_empty());
    }

    /// With a working outbox the write, its row and the event all land.
    #[tokio::test]
    async fn trash_commits_write_and_outbox_row_together() {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("ok".to_string());
        MediaRepository::new(db.clone())
            .insert(&record_with_taken_at(id.clone(), "a/ok.jpg", Some(1)))
            .await
            .unwrap();
        let svc = MediaService::new(
            db.clone(),
            originals.clone(),
            LocalStorageMode::Managed,
            Arc::new(crate::sync::outbox::QueueWriterOutbox::new(db.clone())),
            Arc::new(LocalResolver::new(originals, LocalStorageMode::Managed)),
        );
        let mut rx = svc.subscribe();

        svc.trash(std::slice::from_ref(&id)).await.unwrap();

        assert!(svc.get_media_item(&id).await.unwrap().unwrap().is_trashed);
        assert_eq!(outbox_count(&db).await, 1);
        let events = drain(&mut rx).await;
        assert!(matches!(events.as_slice(), [MediaEvent::Updated(ids)] if ids == &[id]));
    }
}
