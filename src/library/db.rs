use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::{info, instrument};

use super::error::LibraryError;
use super::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
    MediaType,
};
use super::thumbnail::ThumbnailStatus;

/// Manages the library's SQLite database.
///
/// Wraps a [`SqlitePool`] and provides typed CRUD methods. Backend-agnostic —
/// both `LocalLibrary` and future backends share this type.
///
/// Obtain via [`Database::open`], which creates the database file if needed
/// and runs all outstanding migrations before returning.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Open (or create) the database at `db_path`.
    ///
    /// Creates the parent directory if it does not exist, then runs all
    /// pending migrations. Must be called from a Tokio async context.
    #[instrument(fields(path = %db_path.display()))]
    pub async fn open(db_path: &Path) -> Result<Self, LibraryError> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LibraryError::Io)?;
        }

        info!("opening database");

        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .map_err(LibraryError::Db)?;

        sqlx::migrate!("src/library/db/migrations")
            .run(&pool)
            .await
            .map_err(|e| LibraryError::Db(e.into()))?;

        info!("database ready");
        Ok(Self { pool })
    }
}

/// Internal row type for `media_metadata` queries.
#[derive(sqlx::FromRow)]
struct MetadataRow {
    camera_make: Option<String>,
    camera_model: Option<String>,
    lens_model: Option<String>,
    aperture: Option<f32>,
    shutter_str: Option<String>,
    iso: Option<i64>,
    focal_length: Option<f32>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    gps_alt: Option<f64>,
    color_space: Option<String>,
}

/// Internal row type for `list_media` — maps SQLite columns to Rust types.
#[derive(sqlx::FromRow)]
struct MediaRow {
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
    fn into_item(self) -> MediaItem {
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

impl Database {
    /// Insert a `Pending` thumbnail row. No-op if a row already exists.
    pub async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query(
            "INSERT OR IGNORE INTO thumbnails (media_id, status) VALUES (?, 0)",
        )
        .bind(id_str)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a thumbnail `Ready` and record its relative `file_path`.
    pub async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query(
            "UPDATE thumbnails SET status = 1, file_path = ?, generated_at = ? WHERE media_id = ?",
        )
        .bind(file_path)
        .bind(generated_at)
        .bind(id_str)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a thumbnail `Failed`.
    pub async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query("UPDATE thumbnails SET status = 2 WHERE media_id = ?")
            .bind(id_str)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return the `relative_path` column for `id`, or `None` if no row exists.
    ///
    /// Used by `LocalLibrary` to construct the absolute original-file path.
    pub async fn media_relative_path(
        &self,
        id: &MediaId,
    ) -> Result<Option<String>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<String> =
            sqlx::query_scalar("SELECT relative_path FROM media WHERE id = ?")
                .bind(id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return the stored [`ThumbnailStatus`] for `id`, or `None` if no row exists.
    pub async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<i64> =
            sqlx::query_scalar("SELECT status FROM thumbnails WHERE media_id = ?")
                .bind(id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        Ok(row.map(ThumbnailStatus::from_i64))
    }
}

#[async_trait::async_trait]
impl LibraryMedia for Database {
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        let id_str = id.as_str();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE id = ?")
            .bind(id_str)
            .fetch_one(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(count > 0)
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO media (id, relative_path, original_filename, file_size, imported_at,
                                media_type, taken_at, width, height, orientation, duration_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
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
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        let filter_clause = match filter {
            MediaFilter::All => " AND is_trashed = 0",
            MediaFilter::Favorites => " AND is_trashed = 0 AND is_favorite = 1",
            MediaFilter::Trashed => " AND is_trashed = 1",
        };

        let rows = match cursor {
            None => {
                let sql = format!(
                    "SELECT id, taken_at, imported_at, original_filename,
                            width, height, orientation, media_type, is_favorite,
                            is_trashed, trashed_at, duration_ms
                     FROM media
                     WHERE 1=1{filter_clause}
                     ORDER BY COALESCE(taken_at, 0) DESC, id DESC
                     LIMIT ?"
                );
                sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(LibraryError::Db)?
            }
            Some(cur) => {
                let sql = format!(
                    "SELECT id, taken_at, imported_at, original_filename,
                            width, height, orientation, media_type, is_favorite,
                            is_trashed, trashed_at, duration_ms
                     FROM media
                     WHERE (COALESCE(taken_at, 0) < ?
                        OR (COALESCE(taken_at, 0) = ? AND id < ?)){filter_clause}
                     ORDER BY COALESCE(taken_at, 0) DESC, id DESC
                     LIMIT ?"
                );
                sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(cur.sort_key)
                    .bind(cur.sort_key)
                    .bind(cur.id.as_str())
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(LibraryError::Db)?
            }
        };

        Ok(rows.into_iter().map(MediaRow::into_item).collect())
    }

    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        let row: Option<MetadataRow> = sqlx::query_as::<_, MetadataRow>(
            "SELECT camera_make, camera_model, lens_model, aperture, shutter_str,
                    iso, focal_length, gps_lat, gps_lon, gps_alt, color_space
             FROM media_metadata WHERE media_id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(LibraryError::Db)?;

        Ok(row.map(|r| MediaMetadataRecord {
            media_id: id.clone(),
            camera_make: r.camera_make,
            camera_model: r.camera_model,
            lens_model: r.lens_model,
            aperture: r.aperture,
            shutter_str: r.shutter_str,
            iso: r.iso.map(|v| v as u32),
            focal_length: r.focal_length,
            gps_lat: r.gps_lat,
            gps_lon: r.gps_lon,
            gps_alt: r.gps_alt,
            color_space: r.color_space,
        }))
    }

    async fn set_favorite(
        &self,
        ids: &[MediaId],
        favorite: bool,
    ) -> Result<(), LibraryError> {
        let value: i64 = if favorite { 1 } else { 0 };
        for id in ids {
            sqlx::query("UPDATE media SET is_favorite = ? WHERE id = ?")
                .bind(value)
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        }
        Ok(())
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        for id in ids {
            sqlx::query("UPDATE media SET is_trashed = 1, trashed_at = ? WHERE id = ?")
                .bind(now)
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        }
        Ok(())
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        for id in ids {
            sqlx::query("UPDATE media SET is_trashed = 0, trashed_at = NULL WHERE id = ?")
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        }
        Ok(())
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        for id in ids {
            // Delete metadata, thumbnail row, then media row (FK order).
            sqlx::query("DELETE FROM media_metadata WHERE media_id = ?")
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
            sqlx::query("DELETE FROM thumbnails WHERE media_id = ?")
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
            sqlx::query("DELETE FROM media WHERE id = ?")
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        }
        Ok(())
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM media WHERE is_trashed = 1 AND trashed_at < ?",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        if !record.has_data() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO media_metadata
                (media_id, camera_make, camera_model, lens_model, aperture, shutter_str,
                 iso, focal_length, gps_lat, gps_lon, gps_alt, color_space)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.media_id.as_str())
        .bind(&record.camera_make)
        .bind(&record.camera_model)
        .bind(&record.lens_model)
        .bind(record.aperture)
        .bind(&record.shutter_str)
        .bind(record.iso.map(|v| v as i64))
        .bind(record.focal_length)
        .bind(record.gps_lat)
        .bind(record.gps_lon)
        .bind(record.gps_alt)
        .bind(&record.color_space)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::media::{LibraryMedia, MediaType};
    use tempfile::tempdir;

    async fn open_test_db(dir: &std::path::Path) -> Database {
        Database::open(&dir.join("test.db")).await.unwrap()
    }

    fn test_record(id: MediaId) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: "2025/01/15/photo.jpg".to_string(),
            original_filename: "photo.jpg".to_string(),
            file_size: 1024,
            imported_at: 1_700_000_000,
            media_type: MediaType::Image,
            taken_at: None,
            width: None,
            height: None,
            orientation: 1,
            duration_ms: None,
        }
    }

    #[tokio::test]
    async fn open_creates_database_file() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sub").join("moments.db");
        Database::open(&db_path).await.unwrap();
        assert!(db_path.exists());
    }

    #[tokio::test]
    async fn media_does_not_exist_initially() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!())).await.unwrap();
        assert!(!db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn insert_and_exists_roundtrip() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!())).await.unwrap();

        db.insert_media(&test_record(id.clone())).await.unwrap();

        assert!(db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn duplicate_insert_returns_error() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!())).await.unwrap();

        let record = test_record(id.clone());

        db.insert_media(&record).await.unwrap();
        assert!(db.insert_media(&record).await.is_err());
    }

    // ── list_media tests ──────────────────────────────────────────────────────

    fn record_with_taken_at(id: MediaId, path: &str, taken_at: Option<i64>) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: path.to_string(),
            original_filename: path.split('/').last().unwrap_or("photo.jpg").to_string(),
            file_size: 512,
            imported_at: 1_700_000_000,
            media_type: MediaType::Image,
            taken_at,
            width: Some(1920),
            height: Some(1080),
            orientation: 1,
            duration_ms: None,
        }
    }

    #[tokio::test]
    async fn list_media_empty_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let result = db.list_media(MediaFilter::All, None, 50).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn list_media_first_page_ordered_reverse_chronological() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        // Insert three items with different taken_at values.
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        let id_c = MediaId::new("c".repeat(64));

        db.insert_media(&record_with_taken_at(id_a.clone(), "2025/01/01/a.jpg", Some(1_000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "2025/01/02/b.jpg", Some(3_000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_c.clone(), "2025/01/03/c.jpg", Some(2_000))).await.unwrap();

        let items = db.list_media(MediaFilter::All, None, 50).await.unwrap();

        assert_eq!(items.len(), 3);
        // Newest first: b (3000) → c (2000) → a (1000)
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_c);
        assert_eq!(items[2].id, id_a);
    }

    #[tokio::test]
    async fn list_media_null_taken_at_sorts_to_end() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let id_dated = MediaId::new("d".repeat(64));
        let id_undated = MediaId::new("e".repeat(64));

        db.insert_media(&record_with_taken_at(id_dated.clone(), "2025/01/01/dated.jpg", Some(5_000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_undated.clone(), "2025/01/01/undated.jpg", None)).await.unwrap();

        let items = db.list_media(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items.len(), 2);
        // Dated item first, undated last.
        assert_eq!(items[0].id, id_dated);
        assert_eq!(items[1].id, id_undated);
    }

    #[tokio::test]
    async fn list_media_cursor_returns_next_page() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        // Insert 5 items with descending timestamps.
        let ids: Vec<MediaId> = (1..=5)
            .map(|i| MediaId::new(format!("{:0>64}", i)))
            .collect();

        for (i, id) in ids.iter().enumerate() {
            let ts = (5 - i as i64) * 1000; // 5000, 4000, 3000, 2000, 1000
            db.insert_media(&record_with_taken_at(
                id.clone(),
                &format!("2025/01/0{}/photo.jpg", i + 1),
                Some(ts),
            ))
            .await
            .unwrap();
        }

        // First page: 3 items (newest 3: ids[0]=5000, ids[1]=4000, ids[2]=3000)
        let page1 = db.list_media(MediaFilter::All, None, 3).await.unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page1[0].taken_at, Some(5000));
        assert_eq!(page1[2].taken_at, Some(3000));

        // Build cursor from last item of page 1.
        let last = &page1[2];
        let cursor = MediaCursor {
            sort_key: last.taken_at.unwrap_or(0),
            id: last.id.clone(),
        };

        // Second page: remaining 2 items (ids[3]=2000, ids[4]=1000)
        let page2 = db.list_media(MediaFilter::All, Some(&cursor), 3).await.unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].taken_at, Some(2000));
        assert_eq!(page2[1].taken_at, Some(1000));
    }

    #[tokio::test]
    async fn list_media_respects_limit() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        for i in 0..10u64 {
            let id = MediaId::new(format!("{i:0>64}"));
            db.insert_media(&record_with_taken_at(
                id,
                &format!("2025/01/{i:02}/photo.jpg"),
                Some(i as i64 * 1000),
            ))
            .await
            .unwrap();
        }

        let items = db.list_media(MediaFilter::All, None, 4).await.unwrap();
        assert_eq!(items.len(), 4);
    }

    // ── set_favorite tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn set_favorite_and_read_back() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("f".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();

        // Initially not a favourite.
        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert!(!items[0].is_favorite);

        // Set favourite.
        db.set_favorite(&[id.clone()], true).await.unwrap();
        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert!(items[0].is_favorite);

        // Clear favourite.
        db.set_favorite(&[id], false).await.unwrap();
        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert!(!items[0].is_favorite);
    }

    #[tokio::test]
    async fn set_favorite_multiple_ids() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let id1 = MediaId::new("1".repeat(64));
        let id2 = MediaId::new("2".repeat(64));
        db.insert_media(&record_with_taken_at(id1.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id2.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();

        db.set_favorite(&[id1.clone(), id2.clone()], true)
            .await
            .unwrap();

        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert!(items.iter().all(|i| i.is_favorite));
    }

    #[tokio::test]
    async fn list_media_favorites_filter() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let id1 = MediaId::new("1".repeat(64));
        let id2 = MediaId::new("2".repeat(64));
        db.insert_media(&record_with_taken_at(id1.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id2.clone(), "b.jpg", Some(2000)))
            .await
            .unwrap();

        // Mark only id1 as favourite.
        db.set_favorite(&[id1.clone()], true).await.unwrap();

        // All filter returns both.
        let all = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert_eq!(all.len(), 2);

        // Favorites filter returns only id1.
        let favs = db.list_media(MediaFilter::Favorites, None, 10).await.unwrap();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].id, id1);
    }

    // ── trash tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();

        // Initially visible in All.
        assert_eq!(db.list_media(MediaFilter::All, None, 10).await.unwrap().len(), 1);

        // Trash it.
        db.trash(&[id.clone()]).await.unwrap();
        assert_eq!(db.list_media(MediaFilter::All, None, 10).await.unwrap().len(), 0);
        let trashed = db.list_media(MediaFilter::Trashed, None, 10).await.unwrap();
        assert_eq!(trashed.len(), 1);
        assert!(trashed[0].is_trashed);
        assert!(trashed[0].trashed_at.is_some());

        // Restore it.
        db.restore(&[id]).await.unwrap();
        assert_eq!(db.list_media(MediaFilter::All, None, 10).await.unwrap().len(), 1);
        assert_eq!(db.list_media(MediaFilter::Trashed, None, 10).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn trash_excludes_from_favorites() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("u".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        db.set_favorite(&[id.clone()], true).await.unwrap();

        assert_eq!(db.list_media(MediaFilter::Favorites, None, 10).await.unwrap().len(), 1);

        db.trash(&[id]).await.unwrap();
        assert_eq!(db.list_media(MediaFilter::Favorites, None, 10).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn delete_permanently_removes_row() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("v".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();

        db.delete_permanently(&[id.clone()]).await.unwrap();
        assert!(!db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn expired_trash_returns_old_items() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("w".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();

        db.trash(&[id.clone()]).await.unwrap();

        // Item was just trashed — not expired with any positive max_age.
        let expired = db.expired_trash(30 * 24 * 60 * 60).await.unwrap();
        assert!(expired.is_empty());

        // Manually backdate trashed_at to 31 days ago.
        let old_ts = chrono::Utc::now().timestamp() - (31 * 24 * 60 * 60);
        sqlx::query("UPDATE media SET trashed_at = ? WHERE id = ?")
            .bind(old_ts)
            .bind(id.as_str())
            .execute(&db.pool)
            .await
            .unwrap();

        // Now it's expired with a 30-day window.
        let expired = db.expired_trash(30 * 24 * 60 * 60).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], id);
    }
}
