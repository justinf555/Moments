use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{instrument, warn};

use super::event::MediaEvent;
use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord, Stack};
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

    /// Translate an `external_id` (e.g. an Immich asset UUID) to the local
    /// [`MediaId`] under which the row is stored, if any.
    #[instrument(skip(self))]
    pub async fn id_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        self.repo.id_by_external_id(external_id).await
    }

    /// Find a locally-imported row by `content_hash` that has not yet
    /// been pushed (i.e. has no `external_id`). Used by the sync handler
    /// to adopt a local row when push hasn't finished stamping the
    /// server id by the time the same asset arrives over the pull
    /// stream — see [`MediaRepository::id_by_content_hash_pending_push`].
    #[instrument(skip(self))]
    pub async fn id_by_content_hash_pending_push(
        &self,
        content_hash: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        self.repo
            .id_by_content_hash_pending_push(content_hash)
            .await
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

    /// Permanently delete without outbox recording (used by pull sync).
    pub async fn delete_permanently_no_record(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
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
        Ok(())
    }

    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.expired_trash(max_age_secs).await
    }

    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        self.repo.library_stats().await
    }

    /// Sync-only: bump `last_seen_at` for one media row. See issue
    /// #628 — the heartbeat that the reset-cycle orphan sweep
    /// compares against.
    pub async fn bump_last_seen_at(&self, id: &MediaId, now: i64) -> Result<(), LibraryError> {
        self.repo.bump_last_seen_at(id, now).await
    }

    /// Sync-only: return media ids whose heartbeat lags `checkpoint`
    /// and which have a non-null `external_id`. See issue #628.
    pub async fn ids_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.ids_with_stale_heartbeat(checkpoint).await
    }

    // ── Stacks (issue #224) ─────────────────────────────────────────

    /// Sync-only: upsert a stack row from `StackV1`. Emits
    /// `MediaEvent::Updated` for every current member of the stack
    /// so any primary swap reflects in tracked grid models without
    /// a restart.
    pub async fn upsert_stack(&self, stack: &Stack) -> Result<(), LibraryError> {
        self.repo.upsert_stack(stack).await?;
        let members = self.repo.list_stack_members(&stack.id).await?;
        if !members.is_empty() {
            self.emit(MediaEvent::Updated(members));
        }
        Ok(())
    }

    /// Sync-only: ensure a stub `stacks` row exists for the given id,
    /// pointing at the supplied media row as a placeholder primary.
    /// Used by `AssetHandler` to satisfy the FK before binding when
    /// `StackV1` hasn't streamed yet. No event is emitted — the
    /// matching `set_media_stack_id` call that follows fires the
    /// `Updated` event for the asset that just bound.
    ///
    /// `now` seeds the stub's heartbeat so the same-cycle reset
    /// sweep doesn't delete it before `StackV1` arrives.
    pub async fn ensure_stack_stub(
        &self,
        stack_id: &str,
        primary_fallback: &MediaId,
        now: i64,
    ) -> Result<(), LibraryError> {
        self.repo
            .ensure_stack_stub(stack_id, primary_fallback, now)
            .await
    }

    /// Sync-only: bind a media row to a stack. Emits
    /// `MediaEvent::Updated` only when the row actually changed —
    /// re-syncing an unchanged membership skips the event. The grid
    /// reconciles via `get_many`'s primary-only clause.
    pub async fn set_media_stack_id(
        &self,
        media_id: &MediaId,
        stack_id: &str,
    ) -> Result<(), LibraryError> {
        if self.repo.set_media_stack_id(media_id, stack_id).await? {
            self.emit(MediaEvent::Updated(vec![media_id.clone()]));
        }
        Ok(())
    }

    /// Sync-only: clear a media row's stack pointer (the asset was
    /// un-stacked server-side). Emits `MediaEvent::Updated` only
    /// when the row actually changed — re-syncing an
    /// already-un-stacked asset (the common case for most `AssetV1`
    /// payloads) skips the event.
    pub async fn clear_media_stack_id(&self, media_id: &MediaId) -> Result<(), LibraryError> {
        if self.repo.clear_media_stack_id(media_id).await? {
            self.emit(MediaEvent::Updated(vec![media_id.clone()]));
        }
        Ok(())
    }

    /// Sync-only: bump a stack's heartbeat. See issue #628.
    pub async fn bump_stack_last_seen_at(
        &self,
        stack_id: &str,
        now: i64,
    ) -> Result<(), LibraryError> {
        self.repo.bump_stack_last_seen_at(stack_id, now).await
    }

    /// Sync-only: delete a single stack by id (matches the
    /// `SyncStackDeleteV1` ingress path). Emits `MediaEvent::Updated`
    /// for every member that was bound at delete time so they
    /// reappear in the un-stacked grid.
    pub async fn delete_stack(&self, stack_id: &str) -> Result<(), LibraryError> {
        let members = self.repo.list_stack_members(stack_id).await?;
        self.repo.delete_stack(stack_id).await?;
        if !members.is_empty() {
            self.emit(MediaEvent::Updated(members));
        }
        Ok(())
    }

    /// Sync-only: stack ids whose heartbeat lags the checkpoint.
    pub async fn ids_with_stale_stack_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        self.repo.ids_with_stale_stack_heartbeat(checkpoint).await
    }

    /// Sync-only: delete stale stacks. Returns the removed stack ids
    /// for logging. Members are rejoined to the un-stacked timeline
    /// in the same transaction as the row delete. Emits
    /// `MediaEvent::Updated` for every member that was bound at
    /// sweep time so they reappear in the un-stacked grid.
    pub async fn delete_stacks_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let stale = self.repo.ids_with_stale_stack_heartbeat(checkpoint).await?;
        let mut affected_members: Vec<MediaId> = Vec::new();
        for stack_id in &stale {
            affected_members.extend(self.repo.list_stack_members(stack_id).await?);
        }
        let removed = self
            .repo
            .delete_stacks_with_stale_heartbeat(checkpoint)
            .await?;
        if !affected_members.is_empty() {
            self.emit(MediaEvent::Updated(affected_members));
        }
        Ok(removed)
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

    /// Issue #626: with the stable-MediaId sync handler, an upload→pull
    /// round-trip no longer swaps the row's primary key. The handler
    /// looks up by `external_id`, finds the existing local row, and
    /// upserts using its locally-owned id. The service must emit a
    /// single `Updated` — no `Replaced`, no `Removed`+`Added` churn.
    #[tokio::test]
    async fn upsert_media_emits_updated_when_round_tripped_via_external_id() {
        let (_dir, svc) = make_service().await;
        let mut rx = svc.subscribe();

        let local_id = MediaId::new("local-uuid-aaaaaaaaaaaaaaaaaaaaaaaa".to_string());
        let server_id = "server-uuid-bbbbbbbbbbbbbbbbbbbbb".to_string();

        // Local import; push has stamped the server UUID as external_id.
        let mut local = record_with_taken_at(local_id.clone(), "local/photo.jpg", Some(1_000));
        local.external_id = Some(server_id.clone());
        svc.insert_media(&local).await.unwrap();
        let _ = drain(&mut rx).await; // drop the Added event from import

        // Pull-sync now arrives. The new handler resolves external_id →
        // local_id and reuses it as the record's primary key.
        let resolved = svc.id_by_external_id(&server_id).await.unwrap();
        assert_eq!(resolved.as_ref(), Some(&local_id));

        let mut from_server =
            record_with_taken_at(local_id.clone(), "local/photo.jpg", Some(1_000));
        from_server.external_id = Some(server_id.clone());
        svc.upsert_media(&from_server).await.unwrap();

        let events = drain(&mut rx).await;
        assert_eq!(events.len(), 1, "expected single Updated; got {events:?}");
        match &events[0] {
            MediaEvent::Updated(ids) => assert_eq!(ids, &[local_id]),
            other => panic!("expected Updated; got {other:?}"),
        }
    }

    /// Issue #224: `upsert_stack` must emit `MediaEvent::Updated` for
    /// every current member of the stack so a primary swap reflects
    /// in tracked grid models without a restart.
    #[tokio::test]
    async fn upsert_stack_emits_updated_for_all_members() {
        let (_dir, svc) = make_service().await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        for (id, name) in [(&primary, "p.jpg"), (&sibling, "s.jpg")] {
            svc.insert_media(&record_with_taken_at(id.clone(), name, None))
                .await
                .unwrap();
        }
        // Set up the stacks row first so the FK is satisfied when we
        // bind members.
        svc.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        svc.set_media_stack_id(&primary, "stk").await.unwrap();
        svc.set_media_stack_id(&sibling, "stk").await.unwrap();

        // Subscribe AFTER seeding so the Added/Updated noise from
        // setup doesn't leak into the assertion.
        let mut rx = svc.subscribe();
        // Re-upsert with a new primary — the case we actually care
        // about (primary swap should reflect in grid models).
        svc.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: sibling.clone(),
        })
        .await
        .unwrap();

        let events = drain(&mut rx).await;
        let updated_ids: Vec<MediaId> = events
            .iter()
            .flat_map(|e| match e {
                MediaEvent::Updated(ids) => ids.clone(),
                _ => Vec::new(),
            })
            .collect();
        assert!(
            updated_ids.contains(&primary) && updated_ids.contains(&sibling),
            "upsert_stack must emit Updated for both members; got {updated_ids:?}"
        );
    }

    /// Issue #224: `delete_stack` must emit `MediaEvent::Updated` for
    /// every member so they reappear in the un-stacked grid.
    #[tokio::test]
    async fn delete_stack_emits_updated_for_freed_members() {
        let (_dir, svc) = make_service().await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        for (id, name) in [(&primary, "p.jpg"), (&sibling, "s.jpg")] {
            svc.insert_media(&record_with_taken_at(id.clone(), name, None))
                .await
                .unwrap();
        }
        svc.upsert_stack(&Stack {
            id: "doomed".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        svc.set_media_stack_id(&primary, "doomed").await.unwrap();
        svc.set_media_stack_id(&sibling, "doomed").await.unwrap();

        let mut rx = svc.subscribe();
        svc.delete_stack("doomed").await.unwrap();

        let events = drain(&mut rx).await;
        let updated_ids: Vec<MediaId> = events
            .iter()
            .flat_map(|e| match e {
                MediaEvent::Updated(ids) => ids.clone(),
                _ => Vec::new(),
            })
            .collect();
        assert!(
            updated_ids.contains(&primary) && updated_ids.contains(&sibling),
            "delete_stack must emit Updated for both members; got {updated_ids:?}"
        );
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
