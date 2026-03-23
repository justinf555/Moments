use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::{info, instrument};

use super::error::LibraryError;
use super::media::{LibraryMedia, MediaId, MediaRecord};
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
            "INSERT INTO media (id, relative_path, original_filename, file_size, imported_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(record.imported_at)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::media::LibraryMedia;
    use tempfile::tempdir;

    async fn open_test_db(dir: &std::path::Path) -> Database {
        Database::open(&dir.join("test.db")).await.unwrap()
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

        db.insert_media(&MediaRecord {
            id: id.clone(),
            relative_path: "2025/01/15/photo.jpg".to_string(),
            original_filename: "photo.jpg".to_string(),
            file_size: 1024,
            imported_at: 1_700_000_000,
        })
        .await
        .unwrap();

        assert!(db.media_exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn duplicate_insert_returns_error() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let id = MediaId::from_file(std::path::Path::new(file!())).await.unwrap();

        let record = MediaRecord {
            id: id.clone(),
            relative_path: "2025/01/15/photo.jpg".to_string(),
            original_filename: "photo.jpg".to_string(),
            file_size: 1024,
            imported_at: 1_700_000_000,
        };

        db.insert_media(&record).await.unwrap();
        assert!(db.insert_media(&record).await.is_err());
    }
}
