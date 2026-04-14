use super::model::MediaMetadataRecord;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

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

/// Metadata persistence layer.
///
/// Encapsulates all `media_metadata`-table SQL queries. Used by the
/// `MetadataService` (and by sync extensions) — never accessed from
/// the UI layer directly.
#[derive(Clone)]
pub struct MetadataRepository {
    db: Database,
}

impl MetadataRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Fetch the full EXIF metadata record for `id`.
    ///
    /// Returns `None` if no metadata row was stored (e.g. the asset has no EXIF
    /// data, or metadata extraction failed silently at import time).
    pub async fn get(&self, id: &MediaId) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        let row: Option<MetadataRow> = sqlx::query_as(
            "SELECT camera_make, camera_model, lens_model, aperture, shutter_str,
                    iso, focal_length, gps_lat, gps_lon, gps_alt, color_space
             FROM media_metadata WHERE media_id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.db.pool)
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

    /// Persist the full EXIF detail row. No-op if `record.has_data()` is false.
    ///
    /// Uses `ON CONFLICT DO UPDATE` so re-importing the same asset refreshes
    /// metadata without requiring a separate EXISTS check.
    pub async fn insert(&self, record: &MediaMetadataRecord) -> Result<(), LibraryError> {
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
        .execute(&self.db.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Upsert a metadata record (used by sync).
    ///
    /// Uses `INSERT OR REPLACE` so existing records are fully overwritten.
    /// No-op if `record.has_data()` is false.
    pub async fn upsert(&self, record: &MediaMetadataRecord) -> Result<(), LibraryError> {
        if !record.has_data() {
            return Ok(());
        }
        sqlx::query(
            "INSERT OR REPLACE INTO media_metadata
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
        .execute(&self.db.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::repository::MediaRepository;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

    /// Create a `MetadataRepository` backed by a test database.
    async fn test_repo(dir: &std::path::Path) -> (MetadataRepository, MediaRepository) {
        let db = open_test_db(dir).await;
        let repo = MetadataRepository::new(db.clone());
        let media = MediaRepository::new(db);
        (repo, media)
    }

    fn sample_record(media_id: MediaId) -> MediaMetadataRecord {
        MediaMetadataRecord {
            media_id,
            camera_make: Some("Canon".to_string()),
            camera_model: Some("EOS R5".to_string()),
            lens_model: Some("RF 50mm F1.2L".to_string()),
            aperture: Some(1.2),
            shutter_str: Some("1/500".to_string()),
            iso: Some(400),
            focal_length: Some(50.0),
            gps_lat: Some(48.8566),
            gps_lon: Some(2.3522),
            gps_alt: Some(35.0),
            color_space: Some("sRGB".to_string()),
        }
    }

    #[tokio::test]
    async fn get_returns_none_when_no_metadata() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("a".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();
        assert!(repo.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn insert_and_get_roundtrip() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("b".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();

        let record = sample_record(id.clone());
        repo.insert(&record).await.unwrap();

        let fetched = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.camera_make.as_deref(), Some("Canon"));
        assert_eq!(fetched.camera_model.as_deref(), Some("EOS R5"));
        assert_eq!(fetched.iso, Some(400));
        assert!((fetched.gps_lat.unwrap() - 48.8566).abs() < 0.001);
    }

    #[tokio::test]
    async fn insert_skips_empty_record() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("c".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();

        let empty = MediaMetadataRecord {
            media_id: id.clone(),
            camera_make: None,
            camera_model: None,
            lens_model: None,
            aperture: None,
            shutter_str: None,
            iso: None,
            focal_length: None,
            gps_lat: None,
            gps_lon: None,
            gps_alt: None,
            color_space: None,
        };
        repo.insert(&empty).await.unwrap();
        assert!(repo.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn insert_updates_on_conflict() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("d".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();

        repo.insert(&sample_record(id.clone())).await.unwrap();

        let updated = MediaMetadataRecord {
            media_id: id.clone(),
            camera_make: Some("Nikon".to_string()),
            camera_model: None,
            lens_model: None,
            aperture: None,
            shutter_str: None,
            iso: Some(800),
            focal_length: None,
            gps_lat: None,
            gps_lon: None,
            gps_alt: None,
            color_space: None,
        };
        repo.insert(&updated).await.unwrap();

        let fetched = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.camera_make.as_deref(), Some("Nikon"));
        assert_eq!(fetched.iso, Some(800));
        assert!(fetched.camera_model.is_none());
    }

    #[tokio::test]
    async fn upsert_inserts_new_record() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("e".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();

        repo.upsert(&sample_record(id.clone())).await.unwrap();

        let fetched = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.camera_make.as_deref(), Some("Canon"));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_record() {
        let dir = tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("f".repeat(64));
        media.insert(&test_record(id.clone())).await.unwrap();

        repo.insert(&sample_record(id.clone())).await.unwrap();

        let replacement = MediaMetadataRecord {
            media_id: id.clone(),
            camera_make: Some("Sony".to_string()),
            camera_model: Some("A7IV".to_string()),
            lens_model: None,
            aperture: None,
            shutter_str: None,
            iso: None,
            focal_length: None,
            gps_lat: None,
            gps_lon: None,
            gps_alt: None,
            color_space: None,
        };
        repo.upsert(&replacement).await.unwrap();

        let fetched = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.camera_make.as_deref(), Some("Sony"));
        assert_eq!(fetched.camera_model.as_deref(), Some("A7IV"));
        assert!(fetched.lens_model.is_none());
    }
}
