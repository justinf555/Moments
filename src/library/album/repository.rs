use std::collections::HashMap;

use super::model::{Album, AlbumId};
use crate::library::db::{id_placeholders, Database};
use crate::library::error::LibraryError;
use crate::library::media::repository::MediaRow;
use crate::library::media::{MediaCursor, MediaId, MediaItem};

/// Internal row type for album queries.
#[derive(sqlx::FromRow)]
struct AlbumRow {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    media_count: i64,
    cover_media_id: Option<String>,
    is_pinned: bool,
}

/// Album persistence layer.
///
/// Encapsulates all album-related SQL queries. Used by the `Library`
/// struct (and eventually by sync extensions) — never accessed from
/// the UI layer directly.
#[derive(Clone)]
pub struct AlbumRepository {
    db: Database,
}

impl AlbumRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

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

    /// List all albums, ordered by most recently updated first.
    pub async fn list(&self) -> Result<Vec<Album>, LibraryError> {
        let rows: Vec<AlbumRow> = sqlx::query_as(
            "SELECT a.id, a.name, a.created_at, a.updated_at,
                    COUNT(am.media_id) as media_count,
                    (SELECT am2.media_id FROM album_media am2
                     JOIN media m ON m.id = am2.media_id AND m.is_trashed = 0
                     WHERE am2.album_id = a.id
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_media_id,
                    a.is_pinned
             FROM albums a
             LEFT JOIN album_media am ON a.id = am.album_id
                 LEFT JOIN media m2 ON am.media_id = m2.id AND m2.is_trashed = 0
             GROUP BY a.id
             ORDER BY a.updated_at DESC",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows.into_iter().map(album_from_row).collect())
    }

    /// Fetch a single album by raw ID string, including media count and cover.
    pub async fn get_by_raw_id(&self, id: &str) -> Result<Option<Album>, LibraryError> {
        self.get(&AlbumId::from_raw(id.to_string())).await
    }

    /// Fetch a single album by ID, including media count and cover.
    pub async fn get(&self, id: &AlbumId) -> Result<Option<Album>, LibraryError> {
        let row: Option<AlbumRow> = sqlx::query_as(
            "SELECT a.id, a.name, a.created_at, a.updated_at,
                    COUNT(am.media_id) as media_count,
                    (SELECT am2.media_id FROM album_media am2
                     JOIN media m ON m.id = am2.media_id AND m.is_trashed = 0
                     WHERE am2.album_id = a.id
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_media_id,
                    a.is_pinned
             FROM albums a
             LEFT JOIN album_media am ON a.id = am.album_id
                 LEFT JOIN media m2 ON am.media_id = m2.id AND m2.is_trashed = 0
             WHERE a.id = ?
             GROUP BY a.id",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(row.map(album_from_row))
    }

    /// Create a new album with the given name. Returns the new album's ID.
    pub async fn create(&self, name: &str) -> Result<AlbumId, LibraryError> {
        let id = AlbumId::new();
        let now = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO albums (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(id.as_str())
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(id)
    }

    /// Rename an existing album.
    pub async fn rename(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE albums SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(now)
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Set or clear the pinned state for an album.
    pub async fn set_pinned(&self, id: &AlbumId, pinned: bool) -> Result<(), LibraryError> {
        sqlx::query("UPDATE albums SET is_pinned = ? WHERE id = ?")
            .bind(pinned)
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Look up the external_id for an album.
    pub async fn external_id(&self, id: &AlbumId) -> Result<Option<String>, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT external_id FROM albums WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row.and_then(|(eid,)| eid))
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

    /// Delete an album and all its media associations.
    pub async fn delete(&self, id: &AlbumId) -> Result<(), LibraryError> {
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
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
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Add media items to an album. Duplicates are silently ignored.
    pub async fn add_media(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        if media_ids.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let row_placeholders: Vec<&str> = media_ids.iter().map(|_| "(?, ?, ?)").collect();
        let sql = format!(
            "INSERT OR IGNORE INTO album_media (album_id, media_id, added_at) VALUES {}",
            row_placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for media_id in media_ids {
            query = query
                .bind(album_id.as_str())
                .bind(media_id.as_str())
                .bind(now);
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;

        sqlx::query("UPDATE albums SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(album_id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
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

    /// Remove media items from an album.
    pub async fn remove_media(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        if media_ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(media_ids.len());
        let sql =
            format!("DELETE FROM album_media WHERE album_id = ? AND media_id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(album_id.as_str());
        for media_id in media_ids {
            query = query.bind(media_id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE albums SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(album_id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// List media in an album with keyset pagination.
    pub async fn list_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        let rows = match cursor {
            None => sqlx::query_as::<_, MediaRow>(
                "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                            m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                            m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id
                     FROM media m
                     JOIN album_media am ON m.id = am.media_id
                     LEFT JOIN stacks s ON m.stack_id = s.id
                     WHERE am.album_id = ? AND m.is_trashed = 0
                       AND (s.id IS NULL OR s.primary_asset_id = m.id)
                     ORDER BY COALESCE(m.taken_at, 0) DESC, m.id DESC
                     LIMIT ?",
            )
            .bind(album_id.as_str())
            .bind(limit as i64)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?,
            Some(cur) => sqlx::query_as::<_, MediaRow>(
                "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                            m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                            m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id
                     FROM media m
                     JOIN album_media am ON m.id = am.media_id
                     LEFT JOIN stacks s ON m.stack_id = s.id
                     WHERE am.album_id = ?
                       AND (COALESCE(m.taken_at, 0) < ?
                            OR (COALESCE(m.taken_at, 0) = ? AND m.id < ?))
                       AND m.is_trashed = 0
                       AND (s.id IS NULL OR s.primary_asset_id = m.id)
                     ORDER BY COALESCE(m.taken_at, 0) DESC, m.id DESC
                     LIMIT ?",
            )
            .bind(album_id.as_str())
            .bind(cur.sort_key)
            .bind(cur.sort_key)
            .bind(cur.id.as_str())
            .bind(limit as i64)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?,
        };

        Ok(rows.into_iter().map(MediaRow::into_item).collect())
    }

    /// For each album containing at least one of `media_ids`, return the
    /// count of how many are present.
    pub async fn containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<HashMap<AlbumId, usize>, LibraryError> {
        if media_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = id_placeholders(media_ids.len());
        let sql = format!(
            "SELECT am.album_id, COUNT(*) as cnt \
             FROM album_media am \
             JOIN media m ON am.media_id = m.id \
             WHERE am.media_id IN ({placeholders}) \
               AND m.is_trashed = 0 \
             GROUP BY am.album_id"
        );
        let mut query = sqlx::query_as::<_, (String, i64)>(&sql);
        for id in media_ids {
            query = query.bind(id.as_str());
        }
        let rows = query
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows
            .into_iter()
            .map(|(aid, cnt)| (AlbumId::from_raw(aid), cnt as usize))
            .collect())
    }

    /// Return up to `limit` most recent media IDs for an album's cover mosaic.
    pub async fn cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT am.media_id
             FROM album_media am
             JOIN media m ON am.media_id = m.id
             WHERE am.album_id = ? AND m.is_trashed = 0
             ORDER BY am.added_at DESC
             LIMIT ?",
        )
        .bind(album_id.as_str())
        .bind(limit as i64)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
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

/// Convert an `AlbumRow` into an `Album`.
fn album_from_row(r: AlbumRow) -> Album {
    Album {
        id: AlbumId::from_raw(r.id),
        name: r.name,
        created_at: r.created_at,
        updated_at: r.updated_at,
        media_count: r.media_count as u32,
        cover_media_id: r.cover_media_id.map(MediaId::new),
        is_pinned: r.is_pinned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::repository::MediaRepository;
    use crate::library::media::{MediaCursor, MediaId};
    use tempfile::tempdir;

    /// Create an `AlbumRepository` backed by a test database.
    async fn test_repo(dir: &std::path::Path) -> (AlbumRepository, MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = AlbumRepository::new(db.clone());
        let media = MediaRepository::new(db.clone());
        (repo, media, db)
    }

    #[tokio::test]
    async fn create_album_and_list() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id = repo.create("Vacation").await.unwrap();
        let albums = repo.list().await.unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].id, id);
        assert_eq!(albums[0].name, "Vacation");
        assert_eq!(albums[0].media_count, 0);
    }

    #[tokio::test]
    async fn get_album_returns_none_for_missing() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id = AlbumId::from_raw("nonexistent".to_string());
        assert!(repo.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_album_returns_album_with_counts() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Get Test").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&id_a))
            .await
            .unwrap();

        let album = repo.get(&album_id).await.unwrap().unwrap();
        assert_eq!(album.name, "Get Test");
        assert_eq!(album.media_count, 1);
        assert_eq!(album.cover_media_id, Some(id_a));
    }

    #[tokio::test]
    async fn create_album_generates_unique_ids() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id1 = repo.create("Album 1").await.unwrap();
        let id2 = repo.create("Album 2").await.unwrap();
        assert_ne!(id1, id2);
    }

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
    async fn rename_album_updates_name() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id = repo.create("Old Name").await.unwrap();
        repo.rename(&id, "New Name").await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].name, "New Name");
    }

    #[tokio::test]
    async fn delete_album_removes_album_and_media_links() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("To Delete").await.unwrap();
        let media_id = MediaId::new("d".repeat(64));
        media.insert(&test_record(media_id.clone())).await.unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        repo.delete(&album_id).await.unwrap();
        assert!(repo.list().await.unwrap().is_empty());
        assert!(media.exists(&media_id).await.unwrap());
    }

    #[tokio::test]
    async fn add_to_album_and_list_media() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("My Album").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album_id, &[id_a.clone(), id_b.clone()])
            .await
            .unwrap();
        let items = repo.list_media(&album_id, None, 50).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_a);
    }

    #[tokio::test]
    async fn add_duplicate_to_album_is_idempotent() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Dupes").await.unwrap();
        let media_id = MediaId::new("e".repeat(64));
        media.insert(&test_record(media_id.clone())).await.unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        assert_eq!(repo.list_media(&album_id, None, 50).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn remove_from_album() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Remove Test").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album_id, &[id_a.clone(), id_b.clone()])
            .await
            .unwrap();
        repo.remove_media(&album_id, &[id_a]).await.unwrap();
        let items = repo.list_media(&album_id, None, 50).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id_b);
    }

    #[tokio::test]
    async fn list_albums_includes_media_count() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Counting").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album_id, &[id_a, id_b]).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].media_count, 2);
    }

    #[tokio::test]
    async fn list_albums_includes_cover_media_id() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Cover").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&id_a))
            .await
            .unwrap();
        assert_eq!(repo.list().await.unwrap()[0].cover_media_id, Some(id_a));
    }

    #[tokio::test]
    async fn list_album_media_excludes_trashed() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Trash Test").await.unwrap();
        let media_id = MediaId::new("t".repeat(64));
        media.insert(&test_record(media_id.clone())).await.unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        media.trash(&[media_id]).await.unwrap();
        assert!(repo
            .list_media(&album_id, None, 50)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn list_album_media_cursor_pagination() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Paging").await.unwrap();
        let ids: Vec<MediaId> = (1..=5)
            .map(|i| MediaId::new(format!("{:0>64}", i)))
            .collect();
        for (i, id) in ids.iter().enumerate() {
            let ts = (5 - i as i64) * 1000;
            media
                .insert(&record_with_taken_at(
                    id.clone(),
                    &format!("{i}.jpg"),
                    Some(ts),
                ))
                .await
                .unwrap();
        }
        repo.add_media(&album_id, &ids).await.unwrap();
        let page1 = repo.list_media(&album_id, None, 3).await.unwrap();
        assert_eq!(page1.len(), 3);
        let last = &page1[2];
        let cursor = MediaCursor {
            sort_key: last.taken_at.unwrap_or(0),
            id: last.id.clone(),
        };
        let page2 = repo.list_media(&album_id, Some(&cursor), 3).await.unwrap();
        assert_eq!(page2.len(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_empty_input() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let result = repo.containing_media(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn albums_containing_media_single_album() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Test").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album_id, &[id_a.clone(), id_b.clone()])
            .await
            .unwrap();

        let result = repo.containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(*result.get(&album_id).unwrap(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_multiple_albums() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album1 = repo.create("Album 1").await.unwrap();
        let album2 = repo.create("Album 2").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album1, std::slice::from_ref(&id_a))
            .await
            .unwrap();
        repo.add_media(&album2, &[id_a.clone(), id_b.clone()])
            .await
            .unwrap();

        let result = repo.containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(*result.get(&album1).unwrap(), 1);
        assert_eq!(*result.get(&album2).unwrap(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_partial_membership() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Partial").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        media
            .insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&id_a))
            .await
            .unwrap();

        let result = repo.containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(*result.get(&album_id).unwrap(), 1);
    }

    #[tokio::test]
    async fn albums_containing_media_no_matches() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        repo.create("Empty Album").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        media
            .insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();

        let result = repo.containing_media(&[id_a]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn delete_media_removes_from_album() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;
        let album_id = repo.create("Cascade").await.unwrap();
        let media_id = MediaId::new("c".repeat(64));
        media.insert(&test_record(media_id.clone())).await.unwrap();
        repo.add_media(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        media.delete_permanently(&[media_id]).await.unwrap();
        assert!(repo
            .list_media(&album_id, None, 50)
            .await
            .unwrap()
            .is_empty());
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
    async fn set_pinned_and_read_back() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        let id = repo.create("Pin Me").await.unwrap();
        assert!(!repo.get(&id).await.unwrap().unwrap().is_pinned);

        repo.set_pinned(&id, true).await.unwrap();
        assert!(repo.get(&id).await.unwrap().unwrap().is_pinned);

        repo.set_pinned(&id, false).await.unwrap();
        assert!(!repo.get(&id).await.unwrap().unwrap().is_pinned);
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
