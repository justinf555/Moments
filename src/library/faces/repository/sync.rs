// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Queries used only by Immich sync: pull upserts, external-id lookups
//! and the `last_seen_at` heartbeat.

use super::{AssetFaceRow, FacesRepository};
use crate::library::error::LibraryError;

impl FacesRepository {
    /// Upsert a person record (from sync).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_person(
        &self,
        id: &str,
        name: &str,
        birth_date: Option<&str>,
        is_hidden: bool,
        is_favorite: bool,
        color: Option<&str>,
        face_asset_id: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO people (id, name, birth_date, is_hidden, is_favorite, color, face_asset_id, synced_at, external_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 birth_date = excluded.birth_date,
                 is_hidden = excluded.is_hidden,
                 is_favorite = excluded.is_favorite,
                 color = excluded.color,
                 face_asset_id = excluded.face_asset_id,
                 synced_at = excluded.synced_at,
                 external_id = excluded.external_id",
        )
        .bind(id)
        .bind(name)
        .bind(birth_date)
        .bind(is_hidden)
        .bind(is_favorite)
        .bind(color)
        .bind(face_asset_id)
        .bind(now)
        .bind(external_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete a person by ID.
    pub async fn delete_person(&self, id: &str) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM people WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Upsert an asset face record (from sync).
    pub(crate) async fn upsert_asset_face(&self, face: &AssetFaceRow) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO asset_faces (id, asset_id, person_id, image_width, image_height, bbox_x1, bbox_y1, bbox_x2, bbox_y2, source_type, is_visible, deleted_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 asset_id = excluded.asset_id,
                 person_id = excluded.person_id,
                 image_width = excluded.image_width,
                 image_height = excluded.image_height,
                 bbox_x1 = excluded.bbox_x1,
                 bbox_y1 = excluded.bbox_y1,
                 bbox_x2 = excluded.bbox_x2,
                 bbox_y2 = excluded.bbox_y2,
                 source_type = excluded.source_type,
                 is_visible = excluded.is_visible,
                 deleted_at = excluded.deleted_at",
        )
        .bind(&face.id)
        .bind(&face.asset_id)
        .bind(&face.person_id)
        .bind(face.image_width)
        .bind(face.image_height)
        .bind(face.bbox_x1)
        .bind(face.bbox_y1)
        .bind(face.bbox_x2)
        .bind(face.bbox_y2)
        .bind(&face.source_type)
        .bind(face.is_visible)
        .bind(face.deleted_at)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Look up the person an asset face currently contributes media to.
    ///
    /// Returns `None` if the face row does not exist, if it has a null
    /// `person_id`, or — issue #680 — if it is hidden or soft-deleted
    /// server-side and so contributes to nobody. Callers that need to
    /// distinguish those cases must query separately.
    ///
    /// Folding visibility into the answer is what lets
    /// `FacesService::upsert_asset_face` emit `PersonMediaChanged` when a
    /// face is hidden or un-hidden without its `person_id` changing.
    pub async fn get_asset_face_effective_person_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, LibraryError> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT person_id FROM asset_faces
             WHERE id = ? AND is_visible = 1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.and_then(|(p,)| p))
    }

    /// Delete an asset face by ID, returning the `person_id` of the deleted
    /// row if any. Returns `None` if no row matched, or if the row existed
    /// with a null `person_id`.
    ///
    /// The returned value lets `FacesService` emit `PersonMediaChanged`
    /// without a second query.
    pub async fn delete_asset_face(&self, id: &str) -> Result<Option<String>, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("DELETE FROM asset_faces WHERE id = ? RETURNING person_id")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row.and_then(|(p,)| p))
    }

    /// Recount faces for a person and update the denormalised face_count.
    ///
    /// Issue #680: counts only faces that are visible and not
    /// soft-deleted server-side, matching the `MediaFilter::Person` filter
    /// in `MediaRepository::list`.
    pub async fn update_face_count(&self, person_id: &str) -> Result<(), LibraryError> {
        sqlx::query(
            "UPDATE people SET face_count = (
                SELECT COUNT(*) FROM asset_faces
                WHERE person_id = ? AND is_visible = 1 AND deleted_at IS NULL
            ) WHERE id = ?",
        )
        .bind(person_id)
        .bind(person_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete people whose heartbeat lags the given checkpoint.
    ///
    /// Issue #628: the reset-cycle orphan sweep on the `people` table.
    /// People are always server-sourced (no local-only counterpart),
    /// so the sweep doesn't gate on `external_id`. Returns the deleted
    /// person ids so the caller can emit `PersonRemoved` events.
    ///
    /// Asset face rows that referenced any of the deleted people have
    /// their `person_id` set to NULL via the FK's ON DELETE SET NULL
    /// — the face stays, just unattributed.
    pub async fn delete_people_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM people WHERE last_seen_at < ?")
            .bind(checkpoint)
            .fetch_all(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = rows.into_iter().map(|(id,)| id).collect();
        sqlx::query("DELETE FROM people WHERE last_seen_at < ?")
            .bind(checkpoint)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(ids)
    }

    /// Distinct non-null `person_id` values among asset_face rows
    /// whose heartbeat lags the checkpoint.
    ///
    /// Issue #628: the reset-cycle face sweep needs to know which
    /// surviving people had faces removed so their denormalised
    /// `face_count` can be recomputed. Called by the service before
    /// `delete_asset_faces_with_stale_heartbeat`.
    pub async fn persons_with_stale_faces(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT person_id FROM asset_faces
             WHERE last_seen_at < ? AND person_id IS NOT NULL",
        )
        .bind(checkpoint)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(p,)| p).collect())
    }

    /// Delete asset face rows whose heartbeat lags the given checkpoint.
    ///
    /// Issue #628: the reset-cycle orphan sweep on the `asset_faces`
    /// table. As with people, all rows are server-sourced.
    pub async fn delete_asset_faces_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<u64, LibraryError> {
        let result = sqlx::query("DELETE FROM asset_faces WHERE last_seen_at < ?")
            .bind(checkpoint)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(result.rows_affected())
    }

    /// Update `last_seen_at` to the given unix timestamp for one person row.
    ///
    /// Issue #628: heartbeat for the reset-cycle orphan sweep on
    /// `people`. Bumped from `PersonHandler` (pull). People are
    /// always server-sourced (no local-only counterpart), so the
    /// sweep deletes without an `external_id` filter.
    pub async fn bump_person_last_seen_at(&self, id: &str, now: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE people SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Update `last_seen_at` to the given unix timestamp for one asset face row.
    ///
    /// Issue #628: heartbeat for the reset-cycle orphan sweep on
    /// `asset_faces`. Bumped from `AssetFaceHandler` (pull).
    pub async fn bump_asset_face_last_seen_at(
        &self,
        id: &str,
        now: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE asset_faces SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{face_row, person_media, test_repo};
    use super::*;
    use crate::library::db::test_helpers::test_record;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

    #[tokio::test]
    async fn upsert_person_updates_on_conflict() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.upsert_person("p1", "Alice Smith", None, false, false, None, None, None)
            .await
            .unwrap();

        let people = repo.list_people().await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Alice Smith");
    }

    #[tokio::test]
    async fn delete_person() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.delete_person("p1").await.unwrap();

        let people = repo.list_people().await.unwrap();
        assert!(people.is_empty());
    }

    #[tokio::test]
    async fn upsert_and_delete_asset_face() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        let rec = test_record(MediaId::new("m1".to_string()));
        media.insert(&rec).await.unwrap();

        let face = face_row("f1", "m1", Some("p1"));
        repo.upsert_asset_face(&face).await.unwrap();
        repo.update_face_count("p1").await.unwrap();

        let ids = person_media(&media, "p1").await;
        assert_eq!(ids, vec!["m1"]);

        let deleted_person = repo.delete_asset_face("f1").await.unwrap();
        assert_eq!(deleted_person, Some("p1".to_string()));
        repo.update_face_count("p1").await.unwrap();

        let ids = person_media(&media, "p1").await;
        assert!(ids.is_empty());

        let people = repo.list_people().await.unwrap();
        assert_eq!(people[0].face_count, 0);
    }

    #[tokio::test]
    async fn effective_person_id_is_none_for_inactive_faces() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();

        repo.upsert_asset_face(&face_row("f1", "m1", Some("p1")))
            .await
            .unwrap();
        assert_eq!(
            repo.get_asset_face_effective_person_id("f1").await.unwrap(),
            Some("p1".to_string())
        );

        repo.upsert_asset_face(&AssetFaceRow {
            is_visible: false,
            ..face_row("f1", "m1", Some("p1"))
        })
        .await
        .unwrap();
        assert!(repo
            .get_asset_face_effective_person_id("f1")
            .await
            .unwrap()
            .is_none());

        repo.upsert_asset_face(&AssetFaceRow {
            deleted_at: Some(12345),
            ..face_row("f1", "m1", Some("p1"))
        })
        .await
        .unwrap();
        assert!(repo
            .get_asset_face_effective_person_id("f1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn delete_person_nullifies_face_person_id() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        let rec = test_record(MediaId::new("m1".to_string()));
        media.insert(&rec).await.unwrap();

        let face = face_row("f1", "m1", Some("p1"));
        repo.upsert_asset_face(&face).await.unwrap();

        // Deleting person should SET NULL on the face, not delete it.
        repo.delete_person("p1").await.unwrap();

        // Face still exists but with no person.
        let ids = person_media(&media, "p1").await;
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn bump_person_last_seen_at_writes_value() {
        let dir = tempdir().unwrap();
        let (repo, _media, db) = test_repo(dir.path()).await;
        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();

        repo.bump_person_last_seen_at("p1", 12345).await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT last_seen_at FROM people WHERE id = 'p1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 12345);
    }

    #[tokio::test]
    async fn bump_person_last_seen_at_missing_id_is_noop() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        repo.bump_person_last_seen_at("ghost", 12345).await.unwrap();
    }

    #[tokio::test]
    async fn bump_asset_face_last_seen_at_writes_value() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;
        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();
        let face = face_row("f1", "m1", None);
        repo.upsert_asset_face(&face).await.unwrap();

        repo.bump_asset_face_last_seen_at("f1", 12345)
            .await
            .unwrap();

        let row: (i64,) = sqlx::query_as("SELECT last_seen_at FROM asset_faces WHERE id = 'f1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 12345);
    }

    #[tokio::test]
    async fn bump_asset_face_last_seen_at_missing_id_is_noop() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;
        repo.bump_asset_face_last_seen_at("ghost", 12345)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_people_with_stale_heartbeat_removes_only_eligible_rows() {
        let dir = tempdir().unwrap();
        let (repo, _media, db) = test_repo(dir.path()).await;
        repo.upsert_person("p1", "Stale", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.bump_person_last_seen_at("p1", 100).await.unwrap();
        repo.upsert_person("p2", "Fresh", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.bump_person_last_seen_at("p2", 300).await.unwrap();

        let removed = repo.delete_people_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(removed, vec!["p1".to_string()]);

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM people")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 1, "only the fresh person remains");
    }

    #[tokio::test]
    async fn delete_people_with_stale_heartbeat_nullifies_referencing_face_person_id() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;

        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();
        repo.upsert_person("p1", "Stale", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.bump_person_last_seen_at("p1", 100).await.unwrap();

        let face = face_row("f1", "m1", Some("p1"));
        repo.upsert_asset_face(&face).await.unwrap();

        repo.delete_people_with_stale_heartbeat(200).await.unwrap();

        // Face row survives but is detached from the deleted person.
        let person_id: (Option<String>,) =
            sqlx::query_as("SELECT person_id FROM asset_faces WHERE id = 'f1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(person_id.0, None);
    }

    #[tokio::test]
    async fn delete_asset_faces_with_stale_heartbeat_removes_only_eligible_rows() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;

        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();

        for (id, beat) in [("stale", 100), ("fresh", 300)] {
            let face = face_row(id, "m1", None);
            repo.upsert_asset_face(&face).await.unwrap();
            repo.bump_asset_face_last_seen_at(id, beat).await.unwrap();
        }

        let removed = repo
            .delete_asset_faces_with_stale_heartbeat(200)
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let surviving: (String,) = sqlx::query_as("SELECT id FROM asset_faces")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(surviving.0, "fresh");
    }
}
