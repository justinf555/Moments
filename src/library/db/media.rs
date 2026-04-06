use crate::library::error::LibraryError;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
    MediaType,
};

use super::Database;

/// Internal row type for `media_metadata` queries.
#[derive(sqlx::FromRow)]
pub(super) struct MetadataRow {
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
pub(super) struct MediaRow {
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
    pub(super) fn into_item(self) -> MediaItem {
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
    /// Fetch a single media item by ID.
    pub async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, taken_at, imported_at, original_filename,
                    width, height, orientation, media_type, is_favorite,
                    is_trashed, trashed_at, duration_ms
             FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.map(MediaRow::into_item))
    }

    /// Return the `original_filename` column for `id`, or `None` if no row exists.
    pub async fn media_original_filename(
        &self,
        id: &MediaId,
    ) -> Result<Option<String>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<String> =
            sqlx::query_scalar("SELECT original_filename FROM media WHERE id = ?")
                .bind(id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Used by `LocalLibrary` to construct the absolute original-file path.
    pub async fn media_relative_path(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<String> =
            sqlx::query_scalar("SELECT relative_path FROM media WHERE id = ?")
                .bind(id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }
}

#[async_trait::async_trait]
impl LibraryMedia for Database {
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        Database::get_media_item(self, id).await
    }

    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        let id_str = id.as_str();
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM media WHERE id = ?")
            .bind(id_str)
            .fetch_optional(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.insert_media_record(record).await
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        // Each filter defines its own WHERE clause and sort expression.
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
                    .fetch_all(&self.pool)
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
        let row: Option<MetadataRow> = sqlx::query_as(
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

    async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        self.set_favorite_ids(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.trash_ids(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.restore_ids(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.delete_permanently_ids(ids).await
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM media WHERE is_trashed = 1 AND trashed_at < ?")
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
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(media_id) DO UPDATE SET
                camera_make = excluded.camera_make,
                camera_model = excluded.camera_model,
                lens_model = excluded.lens_model,
                aperture = excluded.aperture,
                shutter_str = excluded.shutter_str,
                iso = excluded.iso,
                focal_length = excluded.focal_length,
                gps_lat = excluded.gps_lat,
                gps_lon = excluded.gps_lon,
                gps_alt = excluded.gps_alt,
                color_space = excluded.color_space",
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

    async fn library_stats(&self) -> Result<super::LibraryStats, LibraryError> {
        self.library_stats().await
    }
}

#[cfg(test)]
mod tests {
    use crate::library::album::LibraryAlbums;
    use crate::library::db::test_helpers::*;
    use crate::library::media::{LibraryMedia, MediaCursor, MediaFilter, MediaId};
    use tempfile::tempdir;

    #[tokio::test]
    async fn media_does_not_exist_initially() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!()))
            .await
            .unwrap();
        assert!(!db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn insert_and_exists_roundtrip() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!()))
            .await
            .unwrap();
        db.insert_media(&test_record(id.clone())).await.unwrap();
        assert!(db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn duplicate_insert_returns_error() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!()))
            .await
            .unwrap();
        let record = test_record(id.clone());
        db.insert_media(&record).await.unwrap();
        assert!(db.insert_media(&record).await.is_err());
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
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        let id_c = MediaId::new("c".repeat(64));
        db.insert_media(&record_with_taken_at(
            id_a.clone(),
            "2025/01/01/a.jpg",
            Some(1_000),
        ))
        .await
        .unwrap();
        db.insert_media(&record_with_taken_at(
            id_b.clone(),
            "2025/01/02/b.jpg",
            Some(3_000),
        ))
        .await
        .unwrap();
        db.insert_media(&record_with_taken_at(
            id_c.clone(),
            "2025/01/03/c.jpg",
            Some(2_000),
        ))
        .await
        .unwrap();
        let items = db.list_media(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items.len(), 3);
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
        db.insert_media(&record_with_taken_at(
            id_dated.clone(),
            "dated.jpg",
            Some(5_000),
        ))
        .await
        .unwrap();
        db.insert_media(&record_with_taken_at(
            id_undated.clone(),
            "undated.jpg",
            None,
        ))
        .await
        .unwrap();
        let items = db.list_media(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, id_dated);
        assert_eq!(items[1].id, id_undated);
    }

    #[tokio::test]
    async fn list_media_cursor_returns_next_page() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let ids: Vec<MediaId> = (1..=5)
            .map(|i| MediaId::new(format!("{:0>64}", i)))
            .collect();
        for (i, id) in ids.iter().enumerate() {
            let ts = (5 - i as i64) * 1000;
            db.insert_media(&record_with_taken_at(
                id.clone(),
                &format!("{i}.jpg"),
                Some(ts),
            ))
            .await
            .unwrap();
        }
        let page1 = db.list_media(MediaFilter::All, None, 3).await.unwrap();
        assert_eq!(page1.len(), 3);
        let last = &page1[2];
        let cursor = MediaCursor {
            sort_key: last.taken_at.unwrap_or(0),
            id: last.id.clone(),
        };
        let page2 = db
            .list_media(MediaFilter::All, Some(&cursor), 3)
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
    }

    #[tokio::test]
    async fn list_media_respects_limit() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        for i in 0..10u64 {
            let id = MediaId::new(format!("{i:0>64}"));
            db.insert_media(&record_with_taken_at(
                id,
                &format!("{i}.jpg"),
                Some(i as i64 * 1000),
            ))
            .await
            .unwrap();
        }
        let items = db.list_media(MediaFilter::All, None, 4).await.unwrap();
        assert_eq!(items.len(), 4);
    }

    #[tokio::test]
    async fn set_favorite_and_read_back() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("f".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        assert!(!db.list_media(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
        db.set_favorite(std::slice::from_ref(&id), true)
            .await
            .unwrap();
        assert!(db.list_media(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
        db.set_favorite(&[id], false).await.unwrap();
        assert!(!db.list_media(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
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
        db.set_favorite(&[id1, id2], true).await.unwrap();
        assert!(db
            .list_media(MediaFilter::All, None, 10)
            .await
            .unwrap()
            .iter()
            .all(|i| i.is_favorite));
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
        db.set_favorite(std::slice::from_ref(&id1), true)
            .await
            .unwrap();
        assert_eq!(
            db.list_media(MediaFilter::Favorites, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        assert_eq!(
            db.list_media(MediaFilter::All, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        db.trash(std::slice::from_ref(&id)).await.unwrap();
        assert_eq!(
            db.list_media(MediaFilter::All, None, 10)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            db.list_media(MediaFilter::Trashed, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        db.restore(&[id]).await.unwrap();
        assert_eq!(
            db.list_media(MediaFilter::All, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn trash_excludes_from_favorites() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("u".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        db.set_favorite(std::slice::from_ref(&id), true)
            .await
            .unwrap();
        db.trash(&[id]).await.unwrap();
        assert_eq!(
            db.list_media(MediaFilter::Favorites, None, 10)
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn delete_permanently_removes_row() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("v".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        db.delete_permanently(std::slice::from_ref(&id))
            .await
            .unwrap();
        assert!(!db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn expired_trash_returns_old_items() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("w".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        db.trash(std::slice::from_ref(&id)).await.unwrap();
        assert!(db
            .expired_trash(30 * 24 * 60 * 60)
            .await
            .unwrap()
            .is_empty());
        let old_ts = chrono::Utc::now().timestamp() - (31 * 24 * 60 * 60);
        sqlx::query("UPDATE media SET trashed_at = ? WHERE id = ?")
            .bind(old_ts)
            .bind(id.as_str())
            .execute(&db.pool)
            .await
            .unwrap();
        let expired = db.expired_trash(30 * 24 * 60 * 60).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], id);
    }

    #[tokio::test]
    async fn list_media_recent_imports_filter() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let now = chrono::Utc::now().timestamp();
        let id_recent = MediaId::new("r".repeat(64));
        let id_old = MediaId::new("o".repeat(64));
        db.insert_media(&record_with_imported_at(
            id_recent.clone(),
            "recent.jpg",
            now - 3600,
        ))
        .await
        .unwrap();
        db.insert_media(&record_with_imported_at(
            id_old.clone(),
            "old.jpg",
            now - 90 * 86400,
        ))
        .await
        .unwrap();
        let items = db
            .list_media(
                MediaFilter::RecentImports {
                    since: now - 30 * 86400,
                },
                None,
                50,
            )
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id_recent);
    }

    #[tokio::test]
    async fn list_media_recent_imports_excludes_trashed() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let now = chrono::Utc::now().timestamp();
        let id = MediaId::new("x".repeat(64));
        db.insert_media(&record_with_imported_at(
            id.clone(),
            "trashed.jpg",
            now - 3600,
        ))
        .await
        .unwrap();
        db.trash(&[id]).await.unwrap();
        assert!(db
            .list_media(
                MediaFilter::RecentImports {
                    since: now - 30 * 86400
                },
                None,
                50
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn list_media_recent_imports_sorted_by_imported_at() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let now = chrono::Utc::now().timestamp();
        let id_first = MediaId::new("1".repeat(64));
        let id_second = MediaId::new("2".repeat(64));
        db.insert_media(&record_with_imported_at(
            id_first.clone(),
            "first.jpg",
            now - 7200,
        ))
        .await
        .unwrap();
        db.insert_media(&record_with_imported_at(
            id_second.clone(),
            "second.jpg",
            now - 3600,
        ))
        .await
        .unwrap();
        let items = db
            .list_media(
                MediaFilter::RecentImports {
                    since: now - 30 * 86400,
                },
                None,
                50,
            )
            .await
            .unwrap();
        assert_eq!(items[0].id, id_second);
        assert_eq!(items[1].id, id_first);
    }

    #[tokio::test]
    async fn list_media_album_filter() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Filter Test").await.unwrap();
        let id_in = MediaId::new("i".repeat(64));
        let id_out = MediaId::new("o".repeat(64));
        db.insert_media(&record_with_taken_at(id_in.clone(), "in.jpg", Some(2000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id_out.clone(), "out.jpg", Some(1000)))
            .await
            .unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&id_in))
            .await
            .unwrap();
        let items = db
            .list_media(MediaFilter::Album { album_id }, None, 50)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id_in);
    }

    #[tokio::test]
    async fn list_media_album_filter_excludes_trashed() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Trash Filter").await.unwrap();
        let media_id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(media_id.clone()))
            .await
            .unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id))
            .await
            .unwrap();
        db.trash(&[media_id]).await.unwrap();
        assert!(db
            .list_media(MediaFilter::Album { album_id }, None, 50)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn list_media_person_filter() {
        use crate::library::db::faces::AssetFaceRow;
        use crate::library::faces::PersonId;

        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        db.upsert_person("p1", "Alice", None, false, false, None, None)
            .await
            .unwrap();

        let id_in = MediaId::new("i".repeat(64));
        let id_out = MediaId::new("o".repeat(64));
        db.insert_media(&record_with_taken_at(id_in.clone(), "in.jpg", Some(2000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id_out.clone(), "out.jpg", Some(1000)))
            .await
            .unwrap();

        let face = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "i".repeat(64),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        db.upsert_asset_face(&face).await.unwrap();

        let person_id = PersonId::from_raw("p1".to_string());
        let items = db
            .list_media(MediaFilter::Person { person_id }, None, 50)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id_in);
    }

    #[tokio::test]
    async fn list_media_person_filter_excludes_trashed() {
        use crate::library::db::faces::AssetFaceRow;
        use crate::library::faces::PersonId;

        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        db.upsert_person("p1", "Alice", None, false, false, None, None)
            .await
            .unwrap();

        let media_id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(media_id.clone()))
            .await
            .unwrap();

        let face = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "t".repeat(64),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        db.upsert_asset_face(&face).await.unwrap();

        db.trash(&[media_id]).await.unwrap();

        let person_id = PersonId::from_raw("p1".to_string());
        assert!(db
            .list_media(MediaFilter::Person { person_id }, None, 50)
            .await
            .unwrap()
            .is_empty());
    }
}
