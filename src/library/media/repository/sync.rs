// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Queries used only by Immich sync: pull upserts, external-id lookups
//! and the `last_seen_at` heartbeat.

use super::MediaRepository;
use crate::library::db::id_placeholders;
use crate::library::error::LibraryError;
use crate::library::media::model::MediaId;
use crate::library::media::model::MediaRecord;
use crate::library::media::model::Stack;

impl MediaRepository {
    /// Return the local `MediaId`s of every member of a stack
    /// (regardless of whether each member is the primary). Used by
    /// the service layer to fan out `MediaEvent::Updated` to all
    /// affected rows when a stack is mutated server-side.
    pub async fn list_stack_members(&self, stack_id: &str) -> Result<Vec<MediaId>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM media WHERE stack_id = ?")
            .bind(stack_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Upsert a media record (used by sync).
    ///
    /// Uses `INSERT OR REPLACE` so existing records are fully overwritten.
    /// Before inserting, removes any existing row whose `external_id`
    /// matches the incoming `id` — this handles the case where a locally
    /// imported asset (local UUID) was uploaded to Immich and the server
    /// now streams it back with its own UUID as the `id`.
    ///
    /// Returns the id of the replaced local row if one was deleted by the
    /// external_id match, so the caller can emit a `Removed` event for it.
    /// Without this, the UI would still hold a model item keyed on the
    /// local id even though the row has been replaced server-side
    /// (issue #610).
    ///
    /// `imported_at` is treated as a **local-only** field: when an existing
    /// row is being replaced (matched by id, or by external_id during the
    /// local→server UUID swap), its `imported_at` is preserved instead of
    /// being overwritten by the incoming value. This stops sync from
    /// retroactively rewriting "when this asset entered my library" to
    /// the server's `file_created_at` (the photo's capture time), which
    /// would silently move assets out of the Recent Imports view (issue #614).
    pub async fn upsert(&self, record: &MediaRecord) -> Result<Option<MediaId>, LibraryError> {
        // Capture the existing row's locally-only fields so we can
        // preserve them across the INSERT OR REPLACE:
        //
        // * `imported_at` — see #614. The server's `file_created_at`
        //   is the photo's capture time, not when it entered this
        //   library; overwriting moves assets out of Recent Imports.
        // * `is_moments_render` — Phase C (#224). The Phase C save
        //   sets this to 1 on the rendered child it produces; sync's
        //   `AssetHandler` always passes `false` (pull-side tag-based
        //   derivation is Phase D scope). Without preserving the
        //   column, the §8.2 grid filter would flip the wrong
        //   sibling visible after the next pull.
        //
        // Post-#626 the upserting handler always passes the locally-owned
        // `MediaId` (resolved via `id_by_external_id` /
        // `id_by_content_hash_pending_push`), so a plain `id = ?` lookup
        // is sufficient; the older `OR external_id = ?` arm was for the
        // local→server UUID swap that no longer happens.
        let existing: Option<(i64, i64)> =
            sqlx::query_as("SELECT imported_at, is_moments_render FROM media WHERE id = ?")
                .bind(record.id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        let imported_at = existing.map(|(t, _)| t).unwrap_or(record.imported_at);
        let is_moments_render = existing
            .map(|(_, r)| r != 0)
            .unwrap_or(record.is_moments_render);

        // Atomic delete-and-return so the replaced-id observation can
        // never disagree with the row that was actually removed. SQLite
        // 3.35+ supports RETURNING; sqlx surfaces it via fetch_optional.
        let replaced: Option<String> =
            sqlx::query_scalar("DELETE FROM media WHERE external_id = ? AND id != ? RETURNING id")
                .bind(record.id.as_str())
                .bind(record.id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        sqlx::query(
            "INSERT OR REPLACE INTO media (id, content_hash, external_id, relative_path,
                                           original_filename, file_size, imported_at,
                                           media_type, taken_at, width, height,
                                           orientation, duration_ms, is_favorite,
                                           is_trashed, trashed_at, is_moments_render)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.content_hash)
        .bind(&record.external_id)
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .bind(is_moments_render as i64)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(replaced.map(MediaId::new))
    }

    /// Look up a locally-imported row by `content_hash` that has not yet
    /// been pushed to a server (i.e. `external_id IS NULL`).
    ///
    /// Used by the sync handler to *adopt* a local row when the same
    /// asset arrives over the pull stream before push has finished
    /// stamping the server id. Without this step the AssetHandler would
    /// generate a fresh `MediaId` and `INSERT` a parallel stub row,
    /// then push's eventual `UPDATE … SET external_id = ?` would hit
    /// the unique partial index on `external_id` and fail.
    ///
    /// Restricting the match to `external_id IS NULL` is deliberate:
    /// rows already mapped to a *different* Immich asset must never be
    /// silently re-pointed by a hash collision.
    pub async fn id_by_content_hash_pending_push(
        &self,
        content_hash: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT id FROM media WHERE content_hash = ? AND external_id IS NULL",
        )
        .bind(content_hash)
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.map(MediaId::new))
    }

    /// Look up the local [`MediaId`] for a given `external_id`.
    ///
    /// Used by sync handlers to translate Immich-side asset UUIDs into the
    /// stable, locally-owned id under which we actually store the row.
    /// Returns `None` if no row carries that `external_id`.
    pub async fn id_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        let row: Option<String> = sqlx::query_scalar("SELECT id FROM media WHERE external_id = ?")
            .bind(external_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.map(MediaId::new))
    }

    /// Return media ids whose heartbeat lags the given checkpoint and
    /// have a non-null `external_id`.
    ///
    /// Issue #628: callers (the reset-cycle orphan sweep) pass the
    /// returned ids to `delete_permanently_from_sync` so the removal
    /// goes through the recorder + on-disk file cleanup.
    /// `external_id IS NOT NULL` excludes locally-imported rows the
    /// server never knew about.
    pub async fn ids_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM media
             WHERE last_seen_at < ? AND external_id IS NOT NULL",
        )
        .bind(checkpoint)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Update `last_seen_at` to the given unix timestamp for one media row.
    ///
    /// Issue #628: this is the heartbeat that the reset-cycle orphan
    /// sweep compares against. Sync paths call it whenever the server
    /// confirms an asset is still alive — pull `AssetV1` after the
    /// upsert, push completion after stamping `external_id`, and any
    /// server-confirmed write-through (favorite, restore). Rows whose
    /// heartbeat lags the cycle's checkpoint and have a non-null
    /// `external_id` are treated as deleted server-side.
    ///
    /// Missing row is a no-op — it was deleted under us, which is fine.
    pub async fn bump_last_seen_at(&self, id: &MediaId, now: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE media SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Stacks (issue #224) ─────────────────────────────────────────

    /// Upsert a stack row. Idempotent — reuses the existing id and
    /// overwrites `primary_asset_id` if it changed (e.g. the user
    /// re-stacked on the server). Does not bump `last_seen_at`; the
    /// caller does that separately so the heartbeat write stays
    /// uniform across all sync paths.
    pub async fn upsert_stack(&self, stack: &Stack) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO stacks (id, primary_asset_id, last_seen_at)
             VALUES (?, ?, COALESCE((SELECT last_seen_at FROM stacks WHERE id = ?), 0))
             ON CONFLICT(id) DO UPDATE SET primary_asset_id = excluded.primary_asset_id",
        )
        .bind(&stack.id)
        .bind(stack.primary_asset_id.as_str())
        .bind(&stack.id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Ensure a `stacks` row exists for `stack_id`, creating a stub
    /// pointing at `primary_fallback` if it doesn't.
    ///
    /// Used by `AssetHandler` when it sees an `AssetV1.stackId` for a
    /// stack we haven't received the matching `StackV1` for yet — the
    /// FK on `media.stack_id REFERENCES stacks(id)` is enforced
    /// (sqlx 0.8 enables `PRAGMA foreign_keys` by default), so we
    /// can't bind the asset to a non-existent stack. The stub uses
    /// the current asset as a placeholder primary; `StackV1` later
    /// overwrites it via `upsert_stack`'s ON CONFLICT clause.
    /// Idempotent — `ON CONFLICT DO NOTHING` preserves any stub or
    /// authoritative row already present.
    ///
    /// `now` seeds the stub's `last_seen_at`. Issue #628 reset-cycle
    /// reconciliation: if we left the stub at `0`, the same cycle's
    /// sweep at `finish_sync` would delete it before the matching
    /// `StackV1` got a chance to bump it (or before it has a chance
    /// to arrive at all). Seeding with the cycle's wall time keeps
    /// the stub alive for at least one cycle; if `StackV1` never
    /// arrives, the next reset-cycle's checkpoint will be `> now` and
    /// the orphan sweep will clean it up.
    pub async fn ensure_stack_stub(
        &self,
        stack_id: &str,
        primary_fallback: &MediaId,
        now: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO stacks (id, primary_asset_id, last_seen_at)
             VALUES (?, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(stack_id)
        .bind(primary_fallback.as_str())
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Point a media row at a stack. Used when the server announces
    /// stack membership through `AssetV1.stackId`.
    ///
    /// Returns `true` if the row's `stack_id` actually changed. The
    /// SQL guards on `(stack_id IS NULL OR stack_id != ?)` so the
    /// UPDATE is a no-op when the asset is already bound to the
    /// requested stack — saves a redundant `MediaEvent::Updated` on
    /// the steady-state re-sync of an unchanged stack membership.
    pub async fn set_media_stack_id(
        &self,
        media_id: &MediaId,
        stack_id: &str,
    ) -> Result<bool, LibraryError> {
        let result = sqlx::query(
            "UPDATE media SET stack_id = ?
             WHERE id = ? AND (stack_id IS NULL OR stack_id != ?)",
        )
        .bind(stack_id)
        .bind(media_id.as_str())
        .bind(stack_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.rows_affected() > 0)
    }

    /// Clear the stack pointer on a media row. Used when an `AssetV1`
    /// arrives with `stackId: null` (the asset was un-stacked
    /// server-side).
    ///
    /// Returns `true` if the row's `stack_id` actually changed. The
    /// SQL guards on `stack_id IS NOT NULL` so re-sync of an
    /// already-un-stacked asset (the common case for the vast
    /// majority of `AssetV1` payloads) doesn't emit a spurious event.
    pub async fn clear_media_stack_id(&self, media_id: &MediaId) -> Result<bool, LibraryError> {
        let result = sqlx::query(
            "UPDATE media SET stack_id = NULL
             WHERE id = ? AND stack_id IS NOT NULL",
        )
        .bind(media_id.as_str())
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.rows_affected() > 0)
    }

    /// Update a stack's heartbeat. Issue #628 reset-cycle reconciliation
    /// — stacks join the four pre-existing tables (media, albums,
    /// people, asset_faces) in the orphan sweep.
    pub async fn bump_stack_last_seen_at(
        &self,
        stack_id: &str,
        now: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE stacks SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(stack_id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return stack ids whose heartbeat lags the given checkpoint.
    /// Unlike media/albums, stacks have no local-only counterpart —
    /// they only exist as a server projection — so no `external_id`
    /// gate is needed.
    pub async fn ids_with_stale_stack_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM stacks WHERE last_seen_at < ?")
            .bind(checkpoint)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Delete one stack by id, rejoining its members to the un-stacked
    /// timeline. The matching path for `SyncStackDeleteV1` events.
    /// Idempotent — a missing stack id is a no-op.
    ///
    /// `media.stack_id REFERENCES stacks(id) ON DELETE SET NULL`
    /// (migration 023) cascades automatically with `PRAGMA
    /// foreign_keys` enabled (the sqlx 0.8 default). The explicit
    /// `UPDATE … SET stack_id = NULL` here is defensive — if FK
    /// enforcement were ever flipped off, this still does the right
    /// thing — but on a current build it's a redundant no-op.
    pub async fn delete_stack(&self, stack_id: &str) -> Result<(), LibraryError> {
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        sqlx::query("UPDATE media SET stack_id = NULL WHERE stack_id = ?")
            .bind(stack_id)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        sqlx::query("DELETE FROM stacks WHERE id = ?")
            .bind(stack_id)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete stacks whose heartbeat lags the checkpoint, rejoining
    /// each member to the un-stacked timeline. Returns the removed
    /// stack ids for logging.
    ///
    /// As with `delete_stack`, the explicit `UPDATE` is defensive
    /// against future FK pragma changes; under `PRAGMA foreign_keys = ON`
    /// (sqlx 0.8 default) the `media.stack_id` `ON DELETE SET NULL`
    /// cascade does the same work automatically.
    pub async fn delete_stacks_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let ids = self.ids_with_stale_stack_heartbeat(checkpoint).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;

        let unbind_sql =
            format!("UPDATE media SET stack_id = NULL WHERE stack_id IN ({placeholders})");
        let mut unbind = sqlx::query(&unbind_sql);
        for id in &ids {
            unbind = unbind.bind(id);
        }
        unbind.execute(&mut *tx).await.map_err(LibraryError::Db)?;

        let delete_sql = format!("DELETE FROM stacks WHERE id IN ({placeholders})");
        let mut delete = sqlx::query(&delete_sql);
        for id in &ids {
            delete = delete.bind(id);
        }
        delete.execute(&mut *tx).await.map_err(LibraryError::Db)?;

        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_repo;
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::model::MediaFilter;
    use tempfile::tempdir;

    /// Phase C blocker #2: `upsert` is used by the pull-side
    /// `AssetHandler`, which constructs the record with
    /// `is_moments_render: false` (Phase D will derive it from tags).
    /// Without preservation, every pull would clobber the Phase-C-set
    /// `true` and break §8.2.
    #[tokio::test]
    async fn upsert_preserves_existing_is_moments_render() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("c".repeat(64));

        // Insert with the Phase C save flag set.
        let mut rec = test_record(id.clone());
        rec.is_moments_render = true;
        repo.insert(&rec).await.unwrap();

        // Upsert with the column false (simulating pull from Immich).
        let mut sync_rec = test_record(id.clone());
        sync_rec.is_moments_render = false;
        repo.upsert(&sync_rec).await.unwrap();

        // The locally-set flag survives the pull.
        let item = repo.get(&id).await.unwrap().unwrap();
        assert!(
            item.is_moments_render,
            "upsert must preserve the local is_moments_render flag"
        );
    }

    #[tokio::test]
    async fn upsert_inserts_and_replaces() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("g".repeat(64));
        let mut record = test_record(id.clone());
        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none(), "fresh insert returns None");
        assert!(repo.exists(&id).await.unwrap());

        record.original_filename = "updated.jpg".to_string();
        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none(), "same-id upsert returns None");
        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.original_filename, "updated.jpg");
    }

    /// Round-trip a locally-imported asset that gets uploaded to Immich
    /// and streamed back with the server's UUID — `upsert` must report
    /// the local id it just replaced so the service can emit a
    /// `Removed` event for it (issue #610).
    #[tokio::test]
    async fn upsert_returns_replaced_local_id_when_external_id_matches() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        // Local row keyed on a local UUID, with the server UUID stored
        // as external_id (this is the state after a successful push).
        let local_id = MediaId::new("local-uuid-aaaaaaaaaaaaaaaaaaaa".to_string());
        let server_id = MediaId::new("server-uuid-bbbbbbbbbbbbbbbbbbbb".to_string());
        let mut local = test_record(local_id.clone());
        local.external_id = Some(server_id.as_str().to_string());
        repo.insert(&local).await.unwrap();

        // Pull-sync now upserts the asset keyed on the server UUID.
        let mut from_server = test_record(server_id.clone());
        from_server.external_id = Some(server_id.as_str().to_string());
        // Different relative_path is fine — the upsert REPLACEs.
        from_server.relative_path = "from-server.jpg".to_string();

        let replaced = repo.upsert(&from_server).await.unwrap();

        assert_eq!(
            replaced.as_ref().map(|i| i.as_str()),
            Some(local_id.as_str())
        );
        assert!(
            !repo.exists(&local_id).await.unwrap(),
            "old local row deleted"
        );
        assert!(
            repo.exists(&server_id).await.unwrap(),
            "new server row inserted"
        );
    }

    /// Adopt-by-content_hash: pull's `AssetHandler` looks up a local
    /// row that has the same hash but no `external_id`, so the same
    /// asset arriving from sync before push has stamped the server id
    /// is merged in place. A row that *already* has an `external_id`
    /// — i.e. is mapped to a different (or even the same) Immich
    /// asset — must never be returned, since silently re-pointing it
    /// would corrupt the mapping.
    #[tokio::test]
    async fn id_by_content_hash_pending_push_only_matches_unstamped_rows() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let hash = "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=".to_string();

        // Locally-imported, push hasn't run yet → eligible for adoption.
        let pending = MediaId::new("local-pending-aaaaaaaaaaaaaaaaaaa".to_string());
        let mut pending_rec = test_record(pending.clone());
        pending_rec.content_hash = Some(hash.clone());
        pending_rec.external_id = None;
        repo.insert(&pending_rec).await.unwrap();

        let found = repo.id_by_content_hash_pending_push(&hash).await.unwrap();
        assert_eq!(found.as_ref().map(|m| m.as_str()), Some(pending.as_str()));

        // Now stamp external_id — the row must no longer be eligible.
        sqlx::query("UPDATE media SET external_id = ? WHERE id = ?")
            .bind("immich-uuid")
            .bind(pending.as_str())
            .execute(db.pool())
            .await
            .unwrap();

        let after_stamp = repo.id_by_content_hash_pending_push(&hash).await.unwrap();
        assert!(
            after_stamp.is_none(),
            "stamped rows must never be adopted by content_hash"
        );

        // A different hash never matches.
        let miss = repo.id_by_content_hash_pending_push("other").await.unwrap();
        assert!(miss.is_none());
    }

    /// Issue #626: sync handlers translate Immich UUIDs to local
    /// `MediaId`s via `id_by_external_id`. The lookup must return the
    /// row's primary key when `external_id` matches, and `None`
    /// otherwise.
    #[tokio::test]
    async fn id_by_external_id_finds_row_or_returns_none() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let local_id = MediaId::new("local-uuid-eeeeeeeeeeeeeeeeeeeeeee".to_string());
        let server_id = "server-uuid-ffffffffffffffffffffff".to_string();
        let mut rec = test_record(local_id.clone());
        rec.external_id = Some(server_id.clone());
        repo.insert(&rec).await.unwrap();

        let found = repo.id_by_external_id(&server_id).await.unwrap();
        assert_eq!(found.as_ref().map(|m| m.as_str()), Some(local_id.as_str()));

        let missing = repo.id_by_external_id("nope").await.unwrap();
        assert!(missing.is_none());
    }

    /// Upsert with an external_id that doesn't match any existing row
    /// must not report a phantom replacement.
    #[tokio::test]
    async fn upsert_returns_none_when_no_external_id_match() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let id = MediaId::new("just-a-fresh-row-cccccccccccccccccc".to_string());
        let mut record = test_record(id.clone());
        record.external_id = Some("server-uuid-not-otherwise-known".to_string());

        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none());
    }

    /// Re-syncing an existing asset must not overwrite its `imported_at` —
    /// that field belongs to the local library's view of "when did this
    /// arrive", not the server's view of "when was it captured". See #614.
    #[tokio::test]
    async fn upsert_preserves_imported_at_for_existing_id() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("e".repeat(64));

        let mut record = test_record(id.clone());
        record.imported_at = 1_700_000_000; // original local import
        repo.insert(&record).await.unwrap();

        // Pull-sync upserts with a much older capture-time-derived value
        // (what the asset handler used to do).
        record.imported_at = 1_400_000_000;
        repo.upsert(&record).await.unwrap();

        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.imported_at, 1_700_000_000);
    }

    #[tokio::test]
    async fn bump_last_seen_at_writes_value() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;
        let id = MediaId::new("a".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();

        repo.bump_last_seen_at(&id, 12345).await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT last_seen_at FROM media WHERE id = ?")
            .bind(id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 12345);
    }

    #[tokio::test]
    async fn bump_last_seen_at_missing_id_is_noop() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("z".repeat(64));
        // No row exists; the UPDATE matches nothing. Must not error —
        // sync handlers may bump after a row was deleted under them.
        repo.bump_last_seen_at(&id, 12345).await.unwrap();
    }

    #[tokio::test]
    async fn ids_with_stale_heartbeat_finds_only_eligible_rows() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        // Server-sourced row, heartbeat older than checkpoint → orphan.
        let stale_synced = MediaId::new("a".repeat(64));
        let mut rec = record_with_taken_at(stale_synced.clone(), "stale.jpg", Some(1));
        rec.external_id = Some("immich-uuid-a".to_string());
        repo.insert(&rec).await.unwrap();
        repo.bump_last_seen_at(&stale_synced, 100).await.unwrap();

        // Server-sourced row, heartbeat newer than checkpoint → safe.
        let fresh_synced = MediaId::new("b".repeat(64));
        let mut rec = record_with_taken_at(fresh_synced.clone(), "fresh.jpg", Some(2));
        rec.external_id = Some("immich-uuid-b".to_string());
        repo.insert(&rec).await.unwrap();
        repo.bump_last_seen_at(&fresh_synced, 300).await.unwrap();

        // Local-only row (no external_id), heartbeat doesn't matter →
        // immune via the external_id IS NOT NULL filter.
        let local_only = MediaId::new("c".repeat(64));
        let mut rec = record_with_taken_at(local_only.clone(), "local.jpg", Some(3));
        rec.external_id = None;
        repo.insert(&rec).await.unwrap();
        // Heartbeat at 0 (insert default) is < checkpoint, but filter saves it.

        let orphans = repo.ids_with_stale_heartbeat(200).await.unwrap();

        assert_eq!(orphans.len(), 1, "only the stale synced row is orphaned");
        assert_eq!(orphans[0].as_str(), stale_synced.as_str());
    }

    // ── Stacks (issue #224) ─────────────────────────────────────────

    /// `upsert_stack` is idempotent on the id and overwrites the
    /// `primary_asset_id` when re-stacking happens server-side.
    /// `last_seen_at` must be preserved across upserts so heartbeat
    /// state isn't reset on every pull.
    #[tokio::test]
    async fn upsert_stack_is_idempotent_and_preserves_heartbeat() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let primary_a = MediaId::new("a".repeat(64));
        let primary_b = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary_a.clone(), "a.jpg", None))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(primary_b.clone(), "b.jpg", None))
            .await
            .unwrap();

        repo.upsert_stack(&Stack {
            id: "stk1".to_string(),
            primary_asset_id: primary_a.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("stk1", 555).await.unwrap();

        // Upsert again — primary changed (server re-pinned), heartbeat
        // must NOT be reset to 0.
        repo.upsert_stack(&Stack {
            id: "stk1".to_string(),
            primary_asset_id: primary_b.clone(),
        })
        .await
        .unwrap();

        let row: (String, i64) =
            sqlx::query_as("SELECT primary_asset_id, last_seen_at FROM stacks WHERE id = ?")
                .bind("stk1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, primary_b.as_str(), "primary updated by upsert");
        assert_eq!(row.1, 555, "heartbeat preserved across upsert");
    }

    /// Stack ids whose heartbeat lags the checkpoint are returned.
    /// Stacks have no `external_id` gate (server-only construct), so
    /// every stale row is eligible.
    #[tokio::test]
    async fn ids_with_stale_stack_heartbeat_returns_only_stale() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        repo.insert(&test_record(primary.clone())).await.unwrap();

        repo.upsert_stack(&Stack {
            id: "stale".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("stale", 100).await.unwrap();

        repo.upsert_stack(&Stack {
            id: "fresh".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("fresh", 300).await.unwrap();

        let mut stale = repo.ids_with_stale_stack_heartbeat(200).await.unwrap();
        stale.sort();
        assert_eq!(stale, vec!["stale".to_string()]);
    }

    /// Deleting a stale stack must clear `media.stack_id` on its
    /// members via the migration's `ON DELETE SET NULL` cascade so
    /// surviving members rejoin the un-stacked timeline.
    #[tokio::test]
    async fn delete_stacks_with_stale_heartbeat_clears_member_stack_id() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let member = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary.clone(), "p.jpg", None))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(member.clone(), "m.jpg", None))
            .await
            .unwrap();

        repo.upsert_stack(&Stack {
            id: "doomed".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&primary, "doomed").await.unwrap();
        repo.set_media_stack_id(&member, "doomed").await.unwrap();
        repo.bump_stack_last_seen_at("doomed", 50).await.unwrap();

        let removed = repo.delete_stacks_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(removed, vec!["doomed".to_string()]);

        // FK cascade must have nulled stack_id on both members.
        let primary_stack: (Option<String>,) =
            sqlx::query_as("SELECT stack_id FROM media WHERE id = ?")
                .bind(primary.as_str())
                .fetch_one(db.pool())
                .await
                .unwrap();
        let member_stack: (Option<String>,) =
            sqlx::query_as("SELECT stack_id FROM media WHERE id = ?")
                .bind(member.as_str())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(primary_stack.0.is_none());
        assert!(member_stack.0.is_none());
    }

    /// Issue #224: `AssetV1` may arrive before its matching `StackV1`,
    /// and `media.stack_id` REFERENCES `stacks(id)` is enforced. The
    /// stub-then-bind path must succeed; the later `StackV1` upsert
    /// overwrites the placeholder primary.
    #[tokio::test]
    async fn ensure_stack_stub_lets_asset_handler_bind_before_stack_arrives() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let asset = MediaId::new("a".repeat(64));
        repo.insert(&record_with_taken_at(asset.clone(), "a.jpg", None))
            .await
            .unwrap();

        // Stub-then-bind, mirroring `AssetHandler::apply_stack_membership`.
        // Pass a non-zero `now` so the stub survives a same-cycle reset
        // sweep (issue #224 / #628).
        repo.ensure_stack_stub("late-stack", &asset, 555)
            .await
            .unwrap();
        repo.set_media_stack_id(&asset, "late-stack").await.unwrap();

        // Stub exists with the placeholder primary and a live heartbeat.
        let row: (String, i64) =
            sqlx::query_as("SELECT primary_asset_id, last_seen_at FROM stacks WHERE id = ?")
                .bind("late-stack")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, asset.as_str(), "stub primary is the binding asset");
        assert_eq!(
            row.1, 555,
            "stub heartbeat seeded from `now`, not 0 — survives same-cycle sweep"
        );

        // Real StackV1 arrives later: upsert overwrites the primary.
        let real_primary = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(real_primary.clone(), "b.jpg", None))
            .await
            .unwrap();
        repo.upsert_stack(&Stack {
            id: "late-stack".to_string(),
            primary_asset_id: real_primary.clone(),
        })
        .await
        .unwrap();

        let primary_after: String =
            sqlx::query_scalar("SELECT primary_asset_id FROM stacks WHERE id = ?")
                .bind("late-stack")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(primary_after, real_primary.as_str());
    }

    /// `clear_media_stack_id` is the path AssetHandler takes when
    /// `AssetV1.stack` arrives as `null` — the asset was un-stacked
    /// server-side.
    #[tokio::test]
    async fn clear_media_stack_id_unbinds() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let id = MediaId::new("a".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: id.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&id, "stk").await.unwrap();
        repo.clear_media_stack_id(&id).await.unwrap();

        let items = repo.list(MediaFilter::All, None, 10).await.unwrap();
        assert!(items[0].stack_id.is_none());
    }
}
