// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Queries used only by Immich sync: pull upserts, external-id lookups
//! and the `last_seen_at` heartbeat.

use super::MetadataRepository;
use crate::library::error::LibraryError;
use crate::library::metadata::model::MediaMetadataRecord;

impl MetadataRepository {
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
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{sample_record, test_repo};
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

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
