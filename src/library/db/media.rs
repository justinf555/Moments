//! Thin forwarding layer for media operations on `Database`.
//!
//! All SQL lives in `MediaRepository` (`library/media/repository.rs`).
//! This module exists so that code holding a `Database` can still call
//! media methods directly. It will be removed when all features are
//! converted to repositories.

use crate::library::error::LibraryError;
use crate::library::media::repository::MediaRepository;
pub(crate) use crate::library::media::repository::MediaRow;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord,
};

use super::{Database, LibraryStats};

impl Database {
    /// Forwarding shim — delegates to `MediaRepository`.
    pub async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        MediaRepository::new(self.clone()).get(id).await
    }

    /// Forwarding shim — delegates to `MediaRepository`.
    pub async fn media_original_filename(
        &self,
        id: &MediaId,
    ) -> Result<Option<String>, LibraryError> {
        MediaRepository::new(self.clone())
            .original_filename(id)
            .await
    }

    /// Forwarding shim — delegates to `MediaRepository`.
    pub async fn media_relative_path(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        MediaRepository::new(self.clone()).relative_path(id).await
    }
}

#[async_trait::async_trait]
impl LibraryMedia for Database {
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        Database::get_media_item(self, id).await
    }

    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        MediaRepository::new(self.clone()).exists(id).await
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone()).insert(record).await
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        MediaRepository::new(self.clone())
            .list(filter, cursor, limit)
            .await
    }

    async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone())
            .set_favorite(ids, favorite)
            .await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone()).trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone()).restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone())
            .delete_permanently(ids)
            .await
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        MediaRepository::new(self.clone())
            .expired_trash(max_age_secs)
            .await
    }

    async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        MediaRepository::new(self.clone()).library_stats().await
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
    async fn list_media_first_page_ordered_reverse_chronological() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        let id_c = MediaId::new("c".repeat(64));
        db.insert_media(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1_000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id_b.clone(), "b.jpg", Some(3_000)))
            .await
            .unwrap();
        db.insert_media(&record_with_taken_at(id_c.clone(), "c.jpg", Some(2_000)))
            .await
            .unwrap();
        let items = db.list_media(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_c);
        assert_eq!(items[2].id, id_a);
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
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::new("t".repeat(64));
        db.insert_media(&test_record(id.clone())).await.unwrap();
        db.trash(std::slice::from_ref(&id)).await.unwrap();
        assert!(db
            .list_media(MediaFilter::All, None, 10)
            .await
            .unwrap()
            .is_empty());
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
}
