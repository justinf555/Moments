// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Queries used only by Immich sync: pull upserts, external-id lookups
//! and the `last_seen_at` heartbeat.

use super::AlbumRepository;
use crate::library::album::model::Album;
use crate::library::album::model::AlbumId;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

impl AlbumRepository {
    /// Upsert an album with a known ID (used by sync and Immich write-through).
    ///
    /// Inserts a new row or updates the existing one in place. Unlike
    /// `create()`, this does not generate a new UUID — the caller provides
    /// the ID. Uses `ON CONFLICT(id) DO UPDATE` rather than
    /// `INSERT OR REPLACE` so that columns not listed here (notably
    /// `is_pinned`, which is locally-owned and never sent over sync) are
    /// preserved across each pull cycle (#585 review).
    pub async fn upsert(
        &self,
        id: &str,
        name: &str,
        created_at: i64,
        updated_at: i64,
        external_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO albums (id, name, created_at, updated_at, external_id)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at,
                 external_id = excluded.external_id",
        )
        .bind(id)
        .bind(name)
        .bind(created_at)
        .bind(updated_at)
        .bind(external_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Fetch a single album by raw ID string, including media count and cover.
    pub async fn get_by_raw_id(&self, id: &str) -> Result<Option<Album>, LibraryError> {
        self.get(&AlbumId::from_raw(id.to_string())).await
    }

    /// Look up the local [`AlbumId`] for a given `external_id`.
    ///
    /// Used by sync handlers to translate Immich-side album UUIDs into the
    /// stable, locally-owned id under which we actually store the row —
    /// preventing duplicate album rows on the post-push pull round-trip
    /// (#585). Returns `None` if no row carries that `external_id`.
    pub async fn id_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<AlbumId>, LibraryError> {
        let row: Option<String> = sqlx::query_scalar("SELECT id FROM albums WHERE external_id = ?")
            .bind(external_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.map(AlbumId::from_raw))
    }

    /// Sync-only: insert one membership row from the Immich pull stream.
    ///
    /// Differs from [`add_media`]: takes the server-provided `added_at`
    /// and does not bump `albums.updated_at` — sync replicates the
    /// server's timestamp via the `AlbumV1` entity instead.
    pub async fn upsert_membership(
        &self,
        album_id: &AlbumId,
        media_id: &MediaId,
        added_at: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT OR IGNORE INTO album_media (album_id, media_id, added_at) VALUES (?, ?, ?)",
        )
        .bind(album_id.as_str())
        .bind(media_id.as_str())
        .bind(added_at)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Sync-only: delete one membership row from the Immich pull stream.
    ///
    /// Counterpart to [`upsert_membership`] — does not bump
    /// `albums.updated_at`.
    pub async fn delete_membership(
        &self,
        album_id: &AlbumId,
        media_id: &MediaId,
    ) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM album_media WHERE album_id = ? AND media_id = ?")
            .bind(album_id.as_str())
            .bind(media_id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete albums whose heartbeat lags the checkpoint and have a
    /// non-null `external_id`. Cascades to `album_media` membership rows
    /// in the same transaction.
    ///
    /// Issue #628: the reset-cycle orphan sweep on the `albums` table.
    /// `external_id IS NOT NULL` excludes locally-created albums the
    /// server has never seen. Returns the deleted album ids for
    /// logging.
    pub async fn delete_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<AlbumId>, LibraryError> {
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM albums
             WHERE last_seen_at < ? AND external_id IS NOT NULL",
        )
        .bind(checkpoint)
        .fetch_all(&mut *tx)
        .await
        .map_err(LibraryError::Db)?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<AlbumId> = rows
            .into_iter()
            .map(|(id,)| AlbumId::from_raw(id))
            .collect();

        // Cascade-delete membership rows first, then the album rows
        // themselves. Mirrors the manual cascade in `delete()` —
        // `album_media.album_id` references `albums(id)` without
        // ON DELETE CASCADE, so SQLite would reject the album DELETE
        // otherwise.
        for id in &ids {
            sqlx::query("DELETE FROM album_media WHERE album_id = ?")
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(LibraryError::Db)?;
            sqlx::query("DELETE FROM albums WHERE id = ?")
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(LibraryError::Db)?;
        }

        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(ids)
    }

    /// Update `last_seen_at` to the given unix timestamp for one album row.
    ///
    /// Issue #628: heartbeat for the reset-cycle orphan sweep on
    /// `albums`. Bumped from `AlbumHandler` (pull) and any
    /// server-confirmed album mutation (push).
    pub async fn bump_last_seen_at(&self, id: &AlbumId, now: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE albums SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_repo;
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

    #[tokio::test]
    async fn upsert_preserves_is_pinned() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        let id = repo.create("Pinned").await.unwrap();
        repo.set_pinned(&id, true).await.unwrap();
        assert!(repo.get(&id).await.unwrap().unwrap().is_pinned);

        // Sync pull arrives — must not clobber the locally-owned pin.
        repo.upsert(id.as_str(), "Pinned", 0, 0, Some("server-uuid"))
            .await
            .unwrap();
        assert!(repo.get(&id).await.unwrap().unwrap().is_pinned);
    }

    #[tokio::test]
    async fn id_by_external_id_finds_row_or_returns_none() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        let local_id = repo.create("Round-trip").await.unwrap();
        let server_id = "server-album-uuid";
        repo.upsert(local_id.as_str(), "Round-trip", 0, 0, Some(server_id))
            .await
            .unwrap();

        let found = repo.id_by_external_id(server_id).await.unwrap();
        assert_eq!(found, Some(local_id));

        let missing = repo.id_by_external_id("nope").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn upsert_membership_idempotent_then_delete() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        let album_id = repo.create("Sync Album").await.unwrap();
        let media_id = MediaId::new("med-1".to_string());
        media.insert(&test_record(media_id.clone())).await.unwrap();

        let now = chrono::Utc::now().timestamp();
        repo.upsert_membership(&album_id, &media_id, now)
            .await
            .unwrap();
        repo.upsert_membership(&album_id, &media_id, now)
            .await
            .unwrap();

        let album = repo.get(&album_id).await.unwrap().unwrap();
        assert_eq!(album.media_count, 1);

        repo.delete_membership(&album_id, &media_id).await.unwrap();
        let album = repo.get(&album_id).await.unwrap().unwrap();
        assert_eq!(album.media_count, 0);
    }

    /// Sync membership ops must NOT bump `albums.updated_at` — the
    /// `AlbumV1` handler already replicates the server-side timestamp.
    #[tokio::test]
    async fn upsert_membership_does_not_bump_updated_at() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        let album_id = repo.create("Stable Updated At").await.unwrap();
        let baseline = repo.get(&album_id).await.unwrap().unwrap().updated_at;

        let media_id = MediaId::new("med-1".to_string());
        media.insert(&test_record(media_id.clone())).await.unwrap();

        // Use a clearly different timestamp so any bump would be visible.
        repo.upsert_membership(&album_id, &media_id, baseline + 10_000)
            .await
            .unwrap();

        let after = repo.get(&album_id).await.unwrap().unwrap().updated_at;
        assert_eq!(after, baseline);
    }

    #[tokio::test]
    async fn bump_last_seen_at_writes_value() {
        let dir = tempdir().unwrap();
        let (repo, _media, db) = test_repo(dir.path()).await;
        let id = repo.create("Heartbeat").await.unwrap();

        repo.bump_last_seen_at(&id, 12345).await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT last_seen_at FROM albums WHERE id = ?")
            .bind(id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 12345);
    }

    #[tokio::test]
    async fn bump_last_seen_at_missing_id_is_noop() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id = AlbumId::from_raw("nonexistent".to_string());
        repo.bump_last_seen_at(&id, 12345).await.unwrap();
    }

    #[tokio::test]
    async fn delete_with_stale_heartbeat_removes_only_eligible_rows() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;

        // Insert one media row so we can attach an album_media membership
        // and verify the cascade delete.
        let media_id = MediaId::new("a".repeat(64));
        media
            .insert(&record_with_taken_at(media_id.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();

        // Server-sourced album, stale heartbeat → orphan.
        let stale_synced = repo.create("Stale Synced").await.unwrap();
        sqlx::query("UPDATE albums SET external_id = 'immich-1', last_seen_at = 100 WHERE id = ?")
            .bind(stale_synced.as_str())
            .execute(db.pool())
            .await
            .unwrap();
        repo.add_media(&stale_synced, std::slice::from_ref(&media_id))
            .await
            .unwrap();

        // Server-sourced album, fresh heartbeat → safe.
        let fresh_synced = repo.create("Fresh Synced").await.unwrap();
        sqlx::query("UPDATE albums SET external_id = 'immich-2', last_seen_at = 300 WHERE id = ?")
            .bind(fresh_synced.as_str())
            .execute(db.pool())
            .await
            .unwrap();

        // Locally-created album (no external_id) → immune.
        let local_only = repo.create("Local Only").await.unwrap();

        let deleted = repo.delete_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].as_str(), stale_synced.as_str());

        // Stale synced album is gone, fresh + local survive.
        assert!(repo.get(&stale_synced).await.unwrap().is_none());
        assert!(repo.get(&fresh_synced).await.unwrap().is_some());
        assert!(repo.get(&local_only).await.unwrap().is_some());

        // Membership row for the deleted album is gone too.
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM album_media WHERE album_id = ?")
            .bind(stale_synced.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0, "membership rows must cascade with the album");
    }

    #[tokio::test]
    async fn delete_with_stale_heartbeat_empty_returns_empty() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let deleted = repo.delete_with_stale_heartbeat(100).await.unwrap();
        assert!(deleted.is_empty());
    }
}
