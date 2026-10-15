// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::library::db::Database;
use crate::library::error::LibraryError;

use super::model::{Person, PersonId};

mod sync;

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
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, record_with_taken_at, test_record};
    use crate::library::media::repository::MediaRepository;
    use crate::library::media::MediaId;
    use tempfile::tempdir;

    pub(super) async fn test_repo(
        dir: &std::path::Path,
    ) -> (FacesRepository, MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = FacesRepository::new(db.clone());
        let media = MediaRepository::new(db.clone());
        (repo, media, db)
    }

    /// A visible, live face row — what every test assumed before #680
    /// gave `asset_faces` a visibility state. Override with struct
    /// update syntax for the hidden/soft-deleted cases.
    pub(super) fn face_row(id: &str, asset_id: &str, person_id: Option<&str>) -> AssetFaceRow {
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
}
