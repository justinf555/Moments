use std::collections::HashMap;

use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaId, MediaItem};

use super::media::MediaRow;
use super::{id_placeholders, Database};

/// Internal row type for album queries.
#[derive(sqlx::FromRow)]
struct AlbumRow {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    media_count: i64,
    cover_media_id: Option<String>,
}

#[async_trait::async_trait]
impl LibraryAlbums for Database {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        let rows: Vec<AlbumRow> = sqlx::query_as(
            "SELECT a.id, a.name, a.created_at, a.updated_at,
                    COUNT(am.media_id) as media_count,
                    (SELECT am2.media_id FROM album_media am2
                     JOIN media m ON m.id = am2.media_id AND m.is_trashed = 0
                     WHERE am2.album_id = a.id
                     ORDER BY am2.added_at DESC LIMIT 1) as cover_media_id
             FROM albums a
             LEFT JOIN album_media am ON a.id = am.album_id
                 LEFT JOIN media m2 ON am.media_id = m2.id AND m2.is_trashed = 0
             GROUP BY a.id
             ORDER BY a.updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows
            .into_iter()
            .map(|r| Album {
                id: AlbumId::from_raw(r.id),
                name: r.name,
                created_at: r.created_at,
                updated_at: r.updated_at,
                media_count: r.media_count as u32,
                cover_media_id: r.cover_media_id.map(MediaId::new),
            })
            .collect())
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        let id = AlbumId::new();
        let now = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO albums (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(id.as_str())
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(id)
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE albums SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(now)
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM album_media WHERE album_id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        sqlx::query("DELETE FROM albums WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn add_to_album(
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
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;

        sqlx::query("UPDATE albums SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(album_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        if media_ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(media_ids.len());
        let sql = format!(
            "DELETE FROM album_media WHERE album_id = ? AND media_id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        query = query.bind(album_id.as_str());
        for media_id in media_ids {
            query = query.bind(media_id.as_str());
        }
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE albums SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(album_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        let rows = match cursor {
            None => {
                sqlx::query_as::<_, MediaRow>(
                    "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                            m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                            m.is_trashed, m.trashed_at, m.duration_ms
                     FROM media m
                     JOIN album_media am ON m.id = am.media_id
                     WHERE am.album_id = ? AND m.is_trashed = 0
                     ORDER BY COALESCE(m.taken_at, 0) DESC, m.id DESC
                     LIMIT ?",
                )
                .bind(album_id.as_str())
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(LibraryError::Db)?
            }
            Some(cur) => {
                sqlx::query_as::<_, MediaRow>(
                    "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                            m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                            m.is_trashed, m.trashed_at, m.duration_ms
                     FROM media m
                     JOIN album_media am ON m.id = am.media_id
                     WHERE am.album_id = ?
                       AND (COALESCE(m.taken_at, 0) < ?
                            OR (COALESCE(m.taken_at, 0) = ? AND m.id < ?))
                       AND m.is_trashed = 0
                     ORDER BY COALESCE(m.taken_at, 0) DESC, m.id DESC
                     LIMIT ?",
                )
                .bind(album_id.as_str())
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

    async fn albums_containing_media(
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
        let rows = query.fetch_all(&self.pool).await.map_err(LibraryError::Db)?;
        Ok(rows
            .into_iter()
            .map(|(aid, cnt)| (AlbumId::from_raw(aid), cnt as usize))
            .collect())
    }
    async fn album_cover_media_ids(
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
        .fetch_all(&self.pool)
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::library::album::LibraryAlbums;
    use crate::library::db::test_helpers::*;
    use crate::library::media::{LibraryMedia, MediaCursor, MediaId};
    use tempfile::tempdir;

    #[tokio::test]
    async fn create_album_and_list() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = db.create_album("Vacation").await.unwrap();
        let albums = db.list_albums().await.unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].id, id);
        assert_eq!(albums[0].name, "Vacation");
        assert_eq!(albums[0].media_count, 0);
    }

    #[tokio::test]
    async fn create_album_generates_unique_ids() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id1 = db.create_album("Album 1").await.unwrap();
        let id2 = db.create_album("Album 2").await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn rename_album_updates_name() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = db.create_album("Old Name").await.unwrap();
        db.rename_album(&id, "New Name").await.unwrap();
        assert_eq!(db.list_albums().await.unwrap()[0].name, "New Name");
    }

    #[tokio::test]
    async fn delete_album_removes_album_and_media_links() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("To Delete").await.unwrap();
        let media_id = MediaId::new("d".repeat(64));
        db.insert_media(&test_record(media_id.clone())).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id)).await.unwrap();
        db.delete_album(&album_id).await.unwrap();
        assert!(db.list_albums().await.unwrap().is_empty());
        assert!(db.media_exists(&media_id).await.unwrap());
    }

    #[tokio::test]
    async fn add_to_album_and_list_media() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("My Album").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album_id, &[id_a.clone(), id_b.clone()]).await.unwrap();
        let items = db.list_album_media(&album_id, None, 50).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_a);
    }

    #[tokio::test]
    async fn add_duplicate_to_album_is_idempotent() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Dupes").await.unwrap();
        let media_id = MediaId::new("e".repeat(64));
        db.insert_media(&test_record(media_id.clone())).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id)).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id)).await.unwrap();
        assert_eq!(db.list_album_media(&album_id, None, 50).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn remove_from_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Remove Test").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album_id, &[id_a.clone(), id_b.clone()]).await.unwrap();
        db.remove_from_album(&album_id, &[id_a]).await.unwrap();
        let items = db.list_album_media(&album_id, None, 50).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id_b);
    }

    #[tokio::test]
    async fn list_albums_includes_media_count() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Counting").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album_id, &[id_a, id_b]).await.unwrap();
        assert_eq!(db.list_albums().await.unwrap()[0].media_count, 2);
    }

    #[tokio::test]
    async fn list_albums_includes_cover_media_id() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Cover").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&id_a)).await.unwrap();
        assert_eq!(db.list_albums().await.unwrap()[0].cover_media_id, Some(id_a));
    }

    #[tokio::test]
    async fn list_album_media_excludes_trashed() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Trash Test").await.unwrap();
        let media_id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(media_id.clone())).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id)).await.unwrap();
        db.trash(&[media_id]).await.unwrap();
        assert!(db.list_album_media(&album_id, None, 50).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_album_media_cursor_pagination() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Paging").await.unwrap();
        let ids: Vec<MediaId> = (1..=5).map(|i| MediaId::new(format!("{:0>64}", i))).collect();
        for (i, id) in ids.iter().enumerate() {
            let ts = (5 - i as i64) * 1000;
            db.insert_media(&record_with_taken_at(id.clone(), &format!("{i}.jpg"), Some(ts))).await.unwrap();
        }
        db.add_to_album(&album_id, &ids).await.unwrap();
        let page1 = db.list_album_media(&album_id, None, 3).await.unwrap();
        assert_eq!(page1.len(), 3);
        let last = &page1[2];
        let cursor = MediaCursor { sort_key: last.taken_at.unwrap_or(0), id: last.id.clone() };
        let page2 = db.list_album_media(&album_id, Some(&cursor), 3).await.unwrap();
        assert_eq!(page2.len(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_empty_input() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let result = db.albums_containing_media(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn albums_containing_media_single_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Test").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album_id, &[id_a.clone(), id_b.clone()]).await.unwrap();

        let result = db.albums_containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(*result.get(&album_id).unwrap(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_multiple_albums() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album1 = db.create_album("Album 1").await.unwrap();
        let album2 = db.create_album("Album 2").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album1, std::slice::from_ref(&id_a)).await.unwrap();
        db.add_to_album(&album2, &[id_a.clone(), id_b.clone()]).await.unwrap();

        let result = db.albums_containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(*result.get(&album1).unwrap(), 1);
        assert_eq!(*result.get(&album2).unwrap(), 2);
    }

    #[tokio::test]
    async fn albums_containing_media_partial_membership() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Partial").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(2000))).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&id_a)).await.unwrap();

        let result = db.albums_containing_media(&[id_a, id_b]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(*result.get(&album_id).unwrap(), 1);
    }

    #[tokio::test]
    async fn albums_containing_media_no_matches() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        db.create_album("Empty Album").await.unwrap();
        let id_a = MediaId::new("a".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000))).await.unwrap();

        let result = db.albums_containing_media(&[id_a]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn delete_media_removes_from_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let album_id = db.create_album("Cascade").await.unwrap();
        let media_id = MediaId::new("c".repeat(64));
        db.insert_media(&test_record(media_id.clone())).await.unwrap();
        db.add_to_album(&album_id, std::slice::from_ref(&media_id)).await.unwrap();
        db.delete_permanently(&[media_id]).await.unwrap();
        assert!(db.list_album_media(&album_id, None, 50).await.unwrap().is_empty());
    }
}
