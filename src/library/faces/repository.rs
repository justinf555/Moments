// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::library::db::Database;
use crate::library::error::LibraryError;

use super::model::{Person, PersonId};

/// Internal row type for person queries.
#[derive(sqlx::FromRow)]
struct PersonRow {
    id: String,
    name: String,
    face_count: i64,
    is_hidden: bool,
}

/// Internal row type for asset face upserts (from sync).
#[derive(Clone)]
pub(crate) struct AssetFaceRow {
    pub id: String,
    pub asset_id: String,
    pub person_id: Option<String>,
    pub image_width: i32,
    pub image_height: i32,
    pub bbox_x1: i32,
    pub bbox_y1: i32,
    pub bbox_x2: i32,
    pub bbox_y2: i32,
    pub source_type: String,
    /// Issue #680: `false` when Immich hides the face in the asset.
    pub is_visible: bool,
    /// Issue #680: server-side soft-deletion timestamp, or `None` while
    /// the face is live.
    pub deleted_at: Option<i64>,
}

/// Faces/people persistence layer.
///
/// Encapsulates all people and asset_faces SQL queries. Used by the
/// `FacesService` and by the sync manager for sync-specific operations.
#[derive(Clone)]
pub struct FacesRepository {
    db: Database,
}

impl FacesRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    // ── Read queries ────────────────────────────────────────────────

    /// List all people, ordered by face count descending.
    ///
    /// Returns every person (hidden, unnamed, all). Filtering is done
    /// at the widget layer via `gtk::FilterListModel`.
    pub async fn list_people(&self) -> Result<Vec<Person>, LibraryError> {
        let rows: Vec<PersonRow> = sqlx::query_as(
            "SELECT id, name, face_count, is_hidden FROM people ORDER BY face_count DESC, name ASC",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows
            .into_iter()
            .map(|r| Person {
                id: PersonId::from_raw(r.id),
                name: r.name,
                face_count: r.face_count as u32,
                is_hidden: r.is_hidden,
            })
            .collect())
    }

    /// Fetch a single person by ID.
    pub async fn get_person(&self, id: &str) -> Result<Option<Person>, LibraryError> {
        let row: Option<PersonRow> =
            sqlx::query_as("SELECT id, name, face_count, is_hidden FROM people WHERE id = ?")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        Ok(row.map(|r| Person {
            id: PersonId::from_raw(r.id),
            name: r.name,
            face_count: r.face_count as u32,
            is_hidden: r.is_hidden,
        }))
    }

    /// List media IDs for all assets containing a specific person.
    ///
    /// Issue #680: faces hidden (`is_visible = 0`) or soft-deleted
    /// (`deleted_at` set) on the server don't put their asset in the
    /// person's grid. The rows stay — Immich can reverse either state —
    /// so this is a filter, not a delete.
    pub async fn list_media_for_person(
        &self,
        person_id: &str,
    ) -> Result<Vec<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT af.asset_id FROM asset_faces af
             INNER JOIN media m ON m.id = af.asset_id
             LEFT JOIN stacks s ON m.stack_id = s.id
             LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
             WHERE af.person_id = ? AND m.is_trashed = 0
               AND af.is_visible = 1 AND af.deleted_at IS NULL
               AND (s.id IS NULL AND m.is_moments_render = 0
                    OR (render.id IS NULL AND s.primary_asset_id = m.id)
                    OR (render.id IS NOT NULL AND m.is_moments_render = 0))
             ORDER BY COALESCE(m.taken_at, m.imported_at) DESC",
        )
        .bind(person_id)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    // ── Write queries ───────────────────────────────────────────────

    /// Rename a person.
    pub async fn rename_person(&self, id: &str, name: &str) -> Result<(), LibraryError> {
        sqlx::query("UPDATE people SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Set a person's hidden status.
    pub async fn set_person_hidden(&self, id: &str, hidden: bool) -> Result<(), LibraryError> {
        sqlx::query("UPDATE people SET is_hidden = ? WHERE id = ?")
            .bind(hidden)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Sync-specific operations ────────────────────────────────────

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
    /// soft-deleted server-side, matching `list_media_for_person`.
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
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, record_with_taken_at, test_record};
    use crate::library::media::repository::MediaRepository;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

    async fn test_repo(dir: &std::path::Path) -> (FacesRepository, MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = FacesRepository::new(db.clone());
        let media = MediaRepository::new(db.clone());
        (repo, media, db)
    }

    /// A visible, live face row — what every test assumed before #680
    /// gave `asset_faces` a visibility state. Override with struct
    /// update syntax for the hidden/soft-deleted cases.
    fn face_row(id: &str, asset_id: &str, person_id: Option<&str>) -> AssetFaceRow {
        AssetFaceRow {
            id: id.to_string(),
            asset_id: asset_id.to_string(),
            person_id: person_id.map(str::to_string),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
            is_visible: true,
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn upsert_and_list_people() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.upsert_person("p2", "Bob", None, false, false, None, None, None)
            .await
            .unwrap();

        let people = repo.list_people().await.unwrap();
        assert_eq!(people.len(), 2);
    }

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
    async fn list_people_includes_hidden_and_unnamed() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.upsert_person("p2", "Hidden", None, true, false, None, None, None)
            .await
            .unwrap();
        repo.upsert_person("p3", "", None, false, false, None, None, None)
            .await
            .unwrap();

        let all = repo.list_people().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn list_people_sorted_by_face_count() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.upsert_person("p2", "Bob", None, false, false, None, None, None)
            .await
            .unwrap();

        let rec1 = record_with_taken_at(MediaId::new("m1".to_string()), "a/photo1.jpg", Some(1000));
        let rec2 = record_with_taken_at(MediaId::new("m2".to_string()), "a/photo2.jpg", Some(2000));
        media.insert(&rec1).await.unwrap();
        media.insert(&rec2).await.unwrap();

        let face1 = face_row("f1", "m1", Some("p2"));
        let face2 = face_row("f2", "m2", Some("p2"));
        let face3 = face_row("f3", "m1", Some("p1"));
        repo.upsert_asset_face(&face1).await.unwrap();
        repo.upsert_asset_face(&face2).await.unwrap();
        repo.upsert_asset_face(&face3).await.unwrap();

        repo.update_face_count("p1").await.unwrap();
        repo.update_face_count("p2").await.unwrap();

        let people = repo.list_people().await.unwrap();
        assert_eq!(people[0].name, "Bob"); // 2 faces
        assert_eq!(people[0].face_count, 2);
        assert_eq!(people[1].name, "Alice"); // 1 face
        assert_eq!(people[1].face_count, 1);
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
    async fn rename_person() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.rename_person("p1", "Alice Smith").await.unwrap();

        let people = repo.list_people().await.unwrap();
        assert_eq!(people[0].name, "Alice Smith");
    }

    #[tokio::test]
    async fn set_person_hidden() {
        let dir = tempdir().unwrap();
        let (repo, _media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        repo.set_person_hidden("p1", true).await.unwrap();

        let all = repo.list_people().await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].is_hidden);
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

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert_eq!(media, vec!["m1"]);

        let deleted_person = repo.delete_asset_face("f1").await.unwrap();
        assert_eq!(deleted_person, Some("p1".to_string()));
        repo.update_face_count("p1").await.unwrap();

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert!(media.is_empty());

        let people = repo.list_people().await.unwrap();
        assert_eq!(people[0].face_count, 0);
    }

    // ── #680: server-side face visibility ─────────────────────────

    #[tokio::test]
    async fn hidden_and_soft_deleted_faces_are_excluded() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        for (n, taken) in [(1, 1000), (2, 2000), (3, 3000)] {
            let rec = record_with_taken_at(
                MediaId::new(format!("m{n}")),
                &format!("a/photo{n}.jpg"),
                Some(taken),
            );
            media.insert(&rec).await.unwrap();
        }

        repo.upsert_asset_face(&face_row("f1", "m1", Some("p1")))
            .await
            .unwrap();
        repo.upsert_asset_face(&AssetFaceRow {
            is_visible: false,
            ..face_row("f2", "m2", Some("p1"))
        })
        .await
        .unwrap();
        repo.upsert_asset_face(&AssetFaceRow {
            deleted_at: Some(12345),
            ..face_row("f3", "m3", Some("p1"))
        })
        .await
        .unwrap();
        repo.update_face_count("p1").await.unwrap();

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert_eq!(media, vec!["m1"], "only the visible, live face counts");

        let people = repo.list_people().await.unwrap();
        assert_eq!(people[0].face_count, 1);
    }

    /// Hiding a face server-side must drop it from the person on the
    /// next sync, and un-hiding must bring it back — the row is filtered,
    /// never deleted, so no resync is needed either way.
    #[tokio::test]
    async fn hiding_and_unhiding_a_face_round_trips() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;

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
        repo.update_face_count("p1").await.unwrap();
        assert_eq!(repo.list_media_for_person("p1").await.unwrap(), vec!["m1"]);

        repo.upsert_asset_face(&AssetFaceRow {
            is_visible: false,
            ..face_row("f1", "m1", Some("p1"))
        })
        .await
        .unwrap();
        repo.update_face_count("p1").await.unwrap();
        assert!(repo.list_media_for_person("p1").await.unwrap().is_empty());
        assert_eq!(repo.list_people().await.unwrap()[0].face_count, 0);

        let surviving: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM asset_faces WHERE id = 'f1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(surviving.0, 1, "hidden face is filtered, not deleted");

        repo.upsert_asset_face(&face_row("f1", "m1", Some("p1")))
            .await
            .unwrap();
        repo.update_face_count("p1").await.unwrap();
        assert_eq!(repo.list_media_for_person("p1").await.unwrap(), vec!["m1"]);
        assert_eq!(repo.list_people().await.unwrap()[0].face_count, 1);
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

    /// Rows written before migration 027 must keep their old behaviour:
    /// the column defaults leave them visible and live.
    #[tokio::test]
    async fn pre_migration_rows_default_to_visible_and_live() {
        let dir = tempdir().unwrap();
        let (repo, media, db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();

        // Insert without the #680 columns, as migration 026 would have.
        sqlx::query(
            "INSERT INTO asset_faces (id, asset_id, person_id, image_width, image_height,
                                      bbox_x1, bbox_y1, bbox_x2, bbox_y2, source_type)
             VALUES ('f1', 'm1', 'p1', 100, 100, 0, 0, 50, 50, 'MachineLearning')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        repo.update_face_count("p1").await.unwrap();

        assert_eq!(repo.list_media_for_person("p1").await.unwrap(), vec!["m1"]);
        assert_eq!(repo.list_people().await.unwrap()[0].face_count, 1);
    }

    #[tokio::test]
    async fn list_media_for_person_excludes_trashed() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();

        let rec1 = record_with_taken_at(MediaId::new("m1".to_string()), "a/photo1.jpg", Some(1000));
        let mut rec2 =
            record_with_taken_at(MediaId::new("m2".to_string()), "a/photo2.jpg", Some(2000));
        rec2.is_trashed = true;
        rec2.trashed_at = Some(chrono::Utc::now().timestamp());
        media.insert(&rec1).await.unwrap();
        media.insert(&rec2).await.unwrap();

        let face1 = face_row("f1", "m1", Some("p1"));
        let face2 = face_row("f2", "m2", Some("p1"));
        repo.upsert_asset_face(&face1).await.unwrap();
        repo.upsert_asset_face(&face2).await.unwrap();

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert_eq!(media, vec!["m1"]); // m2 is trashed
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
        let media = repo.list_media_for_person("p1").await.unwrap();
        assert!(media.is_empty());
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
