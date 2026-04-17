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
    pub async fn list_media_for_person(
        &self,
        person_id: &str,
    ) -> Result<Vec<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT af.asset_id FROM asset_faces af
             INNER JOIN media m ON m.id = af.asset_id
             WHERE af.person_id = ? AND m.is_trashed = 0
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
            "INSERT INTO asset_faces (id, asset_id, person_id, image_width, image_height, bbox_x1, bbox_y1, bbox_x2, bbox_y2, source_type)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 asset_id = excluded.asset_id,
                 person_id = excluded.person_id,
                 image_width = excluded.image_width,
                 image_height = excluded.image_height,
                 bbox_x1 = excluded.bbox_x1,
                 bbox_y1 = excluded.bbox_y1,
                 bbox_x2 = excluded.bbox_x2,
                 bbox_y2 = excluded.bbox_y2,
                 source_type = excluded.source_type",
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
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Look up the `person_id` currently assigned to an asset face.
    ///
    /// Returns `None` if the face row does not exist, or if the row exists
    /// with a null `person_id`. Callers that need to distinguish those two
    /// cases must query separately.
    pub async fn get_asset_face_person_id(&self, id: &str) -> Result<Option<String>, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT person_id FROM asset_faces WHERE id = ?")
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
    pub async fn update_face_count(&self, person_id: &str) -> Result<(), LibraryError> {
        sqlx::query(
            "UPDATE people SET face_count = (
                SELECT COUNT(*) FROM asset_faces WHERE person_id = ?
            ) WHERE id = ?",
        )
        .bind(person_id)
        .bind(person_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete all people (used during sync reset).
    pub async fn clear_people(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM people")
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete all asset faces (used during sync reset).
    pub async fn clear_asset_faces(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM asset_faces")
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

        let face1 = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p2".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        let face2 = AssetFaceRow {
            id: "f2".to_string(),
            asset_id: "m2".to_string(),
            person_id: Some("p2".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        let face3 = AssetFaceRow {
            id: "f3".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 60,
            bbox_y1: 60,
            bbox_x2: 90,
            bbox_y2: 90,
            source_type: "MachineLearning".to_string(),
        };
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

        let face = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 10,
            bbox_y1: 20,
            bbox_x2: 50,
            bbox_y2: 60,
            source_type: "MachineLearning".to_string(),
        };
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

        let face1 = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        let face2 = AssetFaceRow {
            id: "f2".to_string(),
            asset_id: "m2".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        repo.upsert_asset_face(&face1).await.unwrap();
        repo.upsert_asset_face(&face2).await.unwrap();

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert_eq!(media, vec!["m1"]); // m2 is trashed
    }

    #[tokio::test]
    async fn clear_people_and_faces() {
        let dir = tempdir().unwrap();
        let (repo, media, _db) = test_repo(dir.path()).await;

        repo.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        let rec = test_record(MediaId::new("m1".to_string()));
        media.insert(&rec).await.unwrap();

        let face = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        repo.upsert_asset_face(&face).await.unwrap();

        repo.clear_asset_faces().await.unwrap();
        repo.clear_people().await.unwrap();

        let people = repo.list_people().await.unwrap();
        assert!(people.is_empty());

        let media = repo.list_media_for_person("p1").await.unwrap();
        assert!(media.is_empty());
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

        let face = AssetFaceRow {
            id: "f1".to_string(),
            asset_id: "m1".to_string(),
            person_id: Some("p1".to_string()),
            image_width: 100,
            image_height: 100,
            bbox_x1: 0,
            bbox_y1: 0,
            bbox_x2: 50,
            bbox_y2: 50,
            source_type: "MachineLearning".to_string(),
        };
        repo.upsert_asset_face(&face).await.unwrap();

        // Deleting person should SET NULL on the face, not delete it.
        repo.delete_person("p1").await.unwrap();

        // Face still exists but with no person.
        let media = repo.list_media_for_person("p1").await.unwrap();
        assert!(media.is_empty());
    }
}
