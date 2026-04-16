use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord, MediaType};
use crate::library::db::{id_placeholders, Database, LibraryStats};
use crate::library::error::LibraryError;

/// Internal row type for `list_media` — maps SQLite columns to Rust types.
///
/// `pub(crate)` so that per-concept repository modules (e.g. `AlbumRepository`)
/// can reuse the same row mapping and `into_item()` conversion.
#[derive(sqlx::FromRow)]
pub(crate) struct MediaRow {
    id: String,
    taken_at: Option<i64>,
    imported_at: i64,
    original_filename: String,
    width: Option<i64>,
    height: Option<i64>,
    orientation: i64,
    media_type: i64,
    is_favorite: i64,
    is_trashed: i64,
    trashed_at: Option<i64>,
    duration_ms: Option<i64>,
}

impl MediaRow {
    pub(crate) fn into_item(self) -> MediaItem {
        MediaItem {
            id: MediaId::new(self.id),
            taken_at: self.taken_at,
            imported_at: self.imported_at,
            original_filename: self.original_filename,
            width: self.width,
            height: self.height,
            orientation: self.orientation as u8,
            media_type: if self.media_type == 1 {
                MediaType::Video
            } else {
                MediaType::Image
            },
            is_favorite: self.is_favorite != 0,
            is_trashed: self.is_trashed != 0,
            trashed_at: self.trashed_at,
            duration_ms: self.duration_ms.map(|v| v as u64),
        }
    }
}

/// Media persistence layer.
///
/// Encapsulates all `media`-table SQL queries. Used by the `MediaService`
/// (and by sync extensions) — never accessed from the UI layer directly.
#[derive(Clone)]
pub struct MediaRepository {
    db: Database,
}

impl MediaRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    // ── Read queries ─────────────────────────────────────────────────

    /// Return `true` if an asset with this [`MediaId`] is already stored.
    pub async fn exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM media WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    /// Return `true` if an asset with this content hash already exists (dedup check).
    pub async fn exists_by_content_hash(&self, hash: &str) -> Result<bool, LibraryError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM media WHERE content_hash = ?")
            .bind(hash)
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    /// Fetch a single media item by ID.
    pub async fn get(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, taken_at, imported_at, original_filename,
                    width, height, orientation, media_type, is_favorite,
                    is_trashed, trashed_at, duration_ms
             FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.map(MediaRow::into_item))
    }

    /// Return the `original_filename` column for `id`, or `None` if no row exists.
    pub async fn original_filename(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT original_filename FROM media WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return the `relative_path` column for `id`, or `None` if no row exists.
    ///
    /// Used by backends to construct the absolute original-file path.
    pub async fn relative_path(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT relative_path FROM media WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return path resolution fields for a single asset in one query.
    pub async fn resolve_info(
        &self,
        id: &MediaId,
    ) -> Result<Option<(String, String, Option<String>)>, LibraryError> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT relative_path, original_filename, external_id FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return a page of [`MediaItem`]s in reverse chronological order.
    pub async fn list(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        let (filter_clause, sort_expr) = match &filter {
            MediaFilter::All => (" AND is_trashed = 0", "COALESCE(taken_at, 0)"),
            MediaFilter::Favorites => (
                " AND is_trashed = 0 AND is_favorite = 1",
                "COALESCE(taken_at, 0)",
            ),
            MediaFilter::Trashed => (" AND is_trashed = 1", "COALESCE(trashed_at, 0)"),
            MediaFilter::RecentImports { .. } => (
                " AND is_trashed = 0 AND imported_at > ?",
                "imported_at",
            ),
            MediaFilter::Album { .. } => (
                " AND is_trashed = 0 AND id IN (SELECT media_id FROM album_media WHERE album_id = ?)",
                "COALESCE(taken_at, 0)",
            ),
            MediaFilter::Person { .. } => (
                " AND is_trashed = 0 AND id IN (SELECT DISTINCT asset_id FROM asset_faces WHERE person_id = ?)",
                "COALESCE(taken_at, 0)",
            ),
        };

        let extra_bind: Option<String> = match &filter {
            MediaFilter::RecentImports { since } => Some(since.to_string()),
            MediaFilter::Album { album_id } => Some(album_id.as_str().to_owned()),
            MediaFilter::Person { person_id } => Some(person_id.as_str().to_owned()),
            _ => None,
        };

        let columns = "id, taken_at, imported_at, original_filename,
                        width, height, orientation, media_type, is_favorite,
                        is_trashed, trashed_at, duration_ms";

        let rows = match cursor {
            None => {
                let sql = format!(
                    "SELECT {columns}
                     FROM media
                     WHERE 1=1{filter_clause}
                     ORDER BY {sort_expr} DESC, id DESC
                     LIMIT ?"
                );
                let mut q = sqlx::query_as::<_, MediaRow>(&sql);
                if let Some(ref val) = extra_bind {
                    q = q.bind(val.as_str());
                }
                q.bind(limit as i64)
                    .fetch_all(self.db.pool())
                    .await
                    .map_err(LibraryError::Db)?
            }
            Some(cur) => {
                let sql = format!(
                    "SELECT {columns}
                     FROM media
                     WHERE ({sort_expr} < ?
                        OR ({sort_expr} = ? AND id < ?)){filter_clause}
                     ORDER BY {sort_expr} DESC, id DESC
                     LIMIT ?"
                );
                let mut q = sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(cur.sort_key)
                    .bind(cur.sort_key)
                    .bind(cur.id.as_str());
                if let Some(ref val) = extra_bind {
                    q = q.bind(val.as_str());
                }
                q.bind(limit as i64)
                    .fetch_all(self.db.pool())
                    .await
                    .map_err(LibraryError::Db)?
            }
        };

        Ok(rows.into_iter().map(MediaRow::into_item).collect())
    }

    /// Return IDs of items trashed longer than `max_age_secs` ago.
    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM media WHERE is_trashed = 1 AND trashed_at < ?")
                .bind(cutoff)
                .fetch_all(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Return aggregate library statistics for the preferences overview.
    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                COUNT(CASE WHEN media_type = 0 AND is_trashed = 0 THEN 1 END),
                COUNT(CASE WHEN media_type = 1 AND is_trashed = 0 THEN 1 END),
                COALESCE(SUM(CASE WHEN is_trashed = 0 THEN file_size ELSE 0 END), 0),
                COUNT(CASE WHEN is_trashed = 1 THEN 1 END)
             FROM media",
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        let album_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM albums")
            .fetch_one(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;

        let people_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM people WHERE name != '' AND is_hidden = 0")
                .fetch_one(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        Ok(LibraryStats {
            photo_count: row.0 as u64,
            video_count: row.1 as u64,
            album_count: album_count.0 as u64,
            total_file_size: row.2 as u64,
            trashed_count: row.3 as u64,
            cache_used_bytes: 0,
            people_count: people_count.0 as u64,
            server: None,
        })
    }

    // ── Write queries ────────────────────────────────────────────────

    /// Persist a newly imported media asset record.
    pub async fn insert(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO media (id, content_hash, external_id, relative_path,
                                original_filename, file_size, imported_at, media_type,
                                taken_at, width, height, orientation, duration_ms,
                                is_favorite, is_trashed, trashed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.content_hash)
        .bind(&record.external_id)
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(record.imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Upsert a media record (used by sync).
    ///
    /// Uses `INSERT OR REPLACE` so existing records are fully overwritten.
    /// Before inserting, removes any existing row whose `external_id`
    /// matches the incoming `id` — this handles the case where a locally
    /// imported asset (local UUID) was uploaded to Immich and the server
    /// now streams it back with its own UUID as the `id`.
    pub async fn upsert(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        // If a local row was uploaded and assigned this server ID as its
        // external_id, remove it so the server-keyed row takes over.
        sqlx::query("DELETE FROM media WHERE external_id = ? AND id != ?")
            .bind(record.id.as_str())
            .bind(record.id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;

        sqlx::query(
            "INSERT OR REPLACE INTO media (id, content_hash, external_id, relative_path,
                                           original_filename, file_size, imported_at,
                                           media_type, taken_at, width, height,
                                           orientation, duration_ms, is_favorite,
                                           is_trashed, trashed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.content_hash)
        .bind(&record.external_id)
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(record.imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Set or clear the favourite flag on one or more assets.
    pub async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let value: i64 = if favorite { 1 } else { 0 };
        let placeholders = id_placeholders(ids.len());
        let sql = format!("UPDATE media SET is_favorite = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(value);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Look up external_ids for a batch of media IDs.
    ///
    /// Returns `(local_id, external_id)` pairs. IDs without an external_id
    /// are omitted from the result.
    pub async fn external_ids(
        &self,
        ids: &[MediaId],
    ) -> Result<Vec<(String, String)>, LibraryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "SELECT id, external_id FROM media WHERE id IN ({placeholders}) AND external_id IS NOT NULL"
        );
        let mut query = sqlx::query_as::<_, (String, String)>(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)
    }

    /// Move assets to the trash (soft delete).
    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let placeholders = id_placeholders(ids.len());
        let sql =
            format!("UPDATE media SET is_trashed = 1, trashed_at = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(now);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Restore trashed assets back to the library.
    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "UPDATE media SET is_trashed = 0, trashed_at = NULL WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Permanently delete assets and all related rows in a transaction.
    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        for (table, col) in [
            ("edits", "media_id"),
            ("asset_faces", "asset_id"),
            ("media_metadata", "media_id"),
            ("thumbnails", "media_id"),
            ("album_media", "media_id"),
            ("media", "id"),
        ] {
            let sql = format!("DELETE FROM {table} WHERE {col} IN ({placeholders})");
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(id.as_str());
            }
            query.execute(&mut *tx).await.map_err(LibraryError::Db)?;
        }
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::*;
    use tempfile::tempdir;

    async fn test_repo(dir: &std::path::Path) -> (MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = MediaRepository::new(db.clone());
        (repo, db)
    }

    #[tokio::test]
    async fn exists_returns_false_initially() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("a".repeat(64));
        assert!(!repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn insert_and_exists_roundtrip() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("b".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        assert!(repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn get_returns_inserted_item() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("c".repeat(64));
        repo.insert(&record_with_taken_at(id.clone(), "photo.jpg", Some(5000)))
            .await
            .unwrap();
        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.id, id);
        assert_eq!(item.taken_at, Some(5000));
    }

    #[tokio::test]
    async fn list_ordered_reverse_chronological() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(3000)))
            .await
            .unwrap();
        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_a);
    }

    #[tokio::test]
    async fn set_favorite_and_read_back() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("d".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        assert!(!repo.list(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
        repo.set_favorite(std::slice::from_ref(&id), true)
            .await
            .unwrap();
        assert!(repo.list(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("e".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.trash(std::slice::from_ref(&id)).await.unwrap();
        assert!(repo
            .list(MediaFilter::All, None, 10)
            .await
            .unwrap()
            .is_empty());
        repo.restore(&[id]).await.unwrap();
        assert_eq!(
            repo.list(MediaFilter::All, None, 10).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn delete_permanently_removes_row() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("f".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.delete_permanently(std::slice::from_ref(&id))
            .await
            .unwrap();
        assert!(!repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn upsert_inserts_and_replaces() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("g".repeat(64));
        let mut record = test_record(id.clone());
        repo.upsert(&record).await.unwrap();
        assert!(repo.exists(&id).await.unwrap());

        record.original_filename = "updated.jpg".to_string();
        repo.upsert(&record).await.unwrap();
        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.original_filename, "updated.jpg");
    }

    #[tokio::test]
    async fn library_stats_counts() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        repo.insert(&record_with_taken_at(
            MediaId::new("a".repeat(64)),
            "a.jpg",
            Some(1000),
        ))
        .await
        .unwrap();
        repo.insert(&record_with_taken_at(
            MediaId::new("b".repeat(64)),
            "b.jpg",
            Some(2000),
        ))
        .await
        .unwrap();
        let stats = repo.library_stats().await.unwrap();
        assert_eq!(stats.photo_count, 2);
    }
}
