// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Operations used only by Immich sync: pull upserts (no outbox
//! recording), external-id lookups and the `last_seen_at` heartbeat.

use super::AlbumService;
use crate::library::album::event::AlbumEvent;
use crate::library::album::model::AlbumId;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

impl AlbumService {
    /// Insert or replace an album from the sync stream.
    pub async fn upsert_album(
        &self,
        id: &str,
        name: &str,
        created_at: i64,
        updated_at: i64,
        external_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        let existed = self.repo.get_by_raw_id(id).await?.is_some();
        self.repo
            .upsert(id, name, created_at, updated_at, external_id)
            .await?;
        let album_id = AlbumId::new(id.to_string());
        if existed {
            self.emit(AlbumEvent::AlbumUpdated(album_id));
        } else {
            self.emit(AlbumEvent::AlbumAdded(album_id));
        }
        Ok(())
    }

    /// Sync-only: delete an album because the server deleted it, and emit
    /// [`AlbumEvent::AlbumRemoved`]. Unlike [`AlbumService::delete_album`]
    /// this does **not** record an `AlbumDeleted` mutation, so the delete
    /// is not pushed back to the server (#700).
    pub async fn delete_album_from_sync(&self, id: &AlbumId) -> Result<(), LibraryError> {
        self.repo.delete(id).await?;
        self.emit(AlbumEvent::AlbumRemoved(id.clone()));
        Ok(())
    }

    /// Sync-only: insert one membership row from the Immich pull stream
    /// and emit [`AlbumEvent::AlbumMediaChanged`].
    ///
    /// Mirrors how the user-driven [`add_to_album`] / [`remove_from_album`]
    /// methods own their event emission — every public mutator on this
    /// service that changes album-membership state emits, so callers
    /// can't forget.
    pub async fn upsert_album_membership(
        &self,
        album_id: &AlbumId,
        media_id: &MediaId,
        added_at: i64,
    ) -> Result<(), LibraryError> {
        self.repo
            .upsert_membership(album_id, media_id, added_at)
            .await?;
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        Ok(())
    }

    /// Sync-only: delete one membership row from the Immich pull stream
    /// and emit [`AlbumEvent::AlbumMediaChanged`]. Counterpart to
    /// [`upsert_album_membership`].
    pub async fn delete_album_membership(
        &self,
        album_id: &AlbumId,
        media_id: &MediaId,
    ) -> Result<(), LibraryError> {
        self.repo.delete_membership(album_id, media_id).await?;
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        Ok(())
    }

    /// Translate an Immich-side album UUID to the local [`AlbumId`] under
    /// which the row is stored. Returns `None` if no local row carries
    /// that `external_id`. Sync handlers use this to land pulled albums on
    /// the existing local row instead of inserting a duplicate (#585).
    pub async fn id_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<AlbumId>, LibraryError> {
        self.repo.id_by_external_id(external_id).await
    }

    /// Sync-only: bump `last_seen_at` for one album row. See issue
    /// #628 — the heartbeat that the reset-cycle orphan sweep
    /// compares against.
    pub async fn bump_last_seen_at(&self, id: &AlbumId, now: i64) -> Result<(), LibraryError> {
        self.repo.bump_last_seen_at(id, now).await
    }

    /// Sync-only: delete albums whose heartbeat lags `checkpoint`
    /// and which have a non-null `external_id`. Returns the deleted
    /// album ids. Emits [`AlbumEvent::AlbumRemoved`] for each so
    /// client `ListStore`s drop the rows without waiting for a UI
    /// refresh. See issue #628.
    pub async fn delete_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<AlbumId>, LibraryError> {
        let removed_ids = self.repo.delete_with_stale_heartbeat(checkpoint).await?;
        for id in &removed_ids {
            self.emit(AlbumEvent::AlbumRemoved(id.clone()));
        }
        Ok(removed_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::make_service;
    use super::*;

    /// Issue #585: a locally-created album that has been pushed (so it
    /// carries an `external_id` for the server UUID) and then pulled
    /// back via the sync stream must NOT produce a duplicate row. The
    /// handler resolves `external_id` → local id and upserts in place.
    /// The service must emit `AlbumUpdated`, not `AlbumAdded`.
    #[tokio::test]
    async fn upsert_album_emits_updated_when_round_tripped_via_external_id() {
        let (_dir, svc) = make_service().await;
        let mut rx = svc.subscribe();

        // Local create. `create_album` writes the row but does not emit
        // an event. Then push stamps the server UUID as external_id —
        // simulated here by an `upsert_album` that lands on the existing
        // row, which emits `AlbumUpdated`. Drain it so we can isolate
        // the second upsert below.
        let local_id = svc.create_album("Vacation").await.unwrap();
        let server_id = "server-album-uuid";
        svc.upsert_album(local_id.as_str(), "Vacation", 0, 0, Some(server_id))
            .await
            .unwrap();
        while rx.try_recv().is_ok() {}

        // Pull-sync arrives. The new handler resolves external_id →
        // local_id and upserts under it.
        let resolved = svc.id_by_external_id(server_id).await.unwrap();
        assert_eq!(resolved.as_ref(), Some(&local_id));

        svc.upsert_album(
            resolved.unwrap().as_str(),
            "Vacation",
            0,
            0,
            Some(server_id),
        )
        .await
        .unwrap();

        let albums = svc.list_albums().await.unwrap();
        assert_eq!(albums.len(), 1, "expected single album row; got {albums:?}");
        assert_eq!(albums[0].id, local_id);

        let event = rx.try_recv().expect("expected one event");
        match event {
            AlbumEvent::AlbumUpdated(id) => assert_eq!(id, local_id),
            other => panic!("expected AlbumUpdated; got {other:?}"),
        }
    }

    /// Issue #700: a server-driven album delete must not record an
    /// `AlbumDeleted` mutation (it would be pushed back), but must still
    /// emit `AlbumRemoved` for the client's model patch (#589 / #693).
    #[tokio::test]
    async fn delete_album_from_sync_emits_removed_without_recording() {
        use crate::library::recorder::tests::CapturingRecorder;
        let dir = tempfile::tempdir().unwrap();
        let db = crate::library::db::test_helpers::open_test_db(dir.path()).await;
        let recorder = std::sync::Arc::new(CapturingRecorder::default());
        let svc = AlbumService::new(db, recorder.clone());

        let id = svc.create_album("Doomed").await.unwrap();
        svc.upsert_album(id.as_str(), "Doomed", 0, 0, Some("immich-x"))
            .await
            .unwrap();
        recorder.clear();
        let mut rx = svc.subscribe();

        svc.delete_album_from_sync(&id).await.unwrap();

        assert!(
            recorder.snapshot().is_empty(),
            "sync delete must not record"
        );
        assert!(svc.list_albums().await.unwrap().is_empty());
        match rx.try_recv().expect("expected AlbumRemoved") {
            AlbumEvent::AlbumRemoved(r) => assert_eq!(r, id),
            other => panic!("expected AlbumRemoved; got {other:?}"),
        }

        // Contrast: the user path does record.
        let id2 = svc.create_album("User").await.unwrap();
        recorder.clear();
        svc.delete_album(&id2).await.unwrap();
        assert_eq!(recorder.snapshot().len(), 1);
    }

    /// Issue #628: the orphan sweep at end-of-reset must emit
    /// `AlbumRemoved` for each deleted album so client `ListStore`s
    /// patch in real time instead of waiting for a UI reload.
    #[tokio::test]
    async fn delete_with_stale_heartbeat_emits_album_removed_per_id() {
        let (_dir, svc) = make_service().await;

        let stale = svc.create_album("Stale Synced").await.unwrap();
        let fresh = svc.create_album("Fresh Synced").await.unwrap();
        // Promote both to server-sourced and pin heartbeats.
        svc.upsert_album(stale.as_str(), "Stale Synced", 0, 0, Some("immich-1"))
            .await
            .unwrap();
        svc.upsert_album(fresh.as_str(), "Fresh Synced", 0, 0, Some("immich-2"))
            .await
            .unwrap();
        svc.bump_last_seen_at(&stale, 100).await.unwrap();
        svc.bump_last_seen_at(&fresh, 300).await.unwrap();

        // Subscribe AFTER setup so we don't see upsert events.
        let mut rx = svc.subscribe();

        let removed = svc.delete_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(removed, vec![stale.clone()]);

        let event = rx.try_recv().expect("expected one event");
        match event {
            AlbumEvent::AlbumRemoved(id) => assert_eq!(id, stale),
            other => panic!("expected AlbumRemoved; got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "fresh album wasn't swept; only one event expected"
        );
    }
}
