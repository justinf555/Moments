use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::{info, instrument};

use super::error::LibraryError;

mod albums;
mod edits;
pub(crate) mod faces;
pub(crate) mod media;
mod metadata;
mod sync;
mod thumbnails;
mod upload;

/// Aggregate library statistics for the preferences overview.
#[derive(Debug, Clone, Default)]
pub struct LibraryStats {
    pub photo_count: u64,
    pub video_count: u64,
    pub album_count: u64,
    pub total_file_size: u64,
    /// Number of items currently in the trash.
    pub trashed_count: u64,
    /// Disk usage of the originals cache (Immich only), in bytes.
    pub cache_used_bytes: u64,
    /// People count (named, non-hidden).
    pub people_count: u64,
    /// Server-side statistics (Immich only).
    pub server: Option<ServerStats>,
}

/// Statistics from the Immich server (populated via API calls).
#[derive(Debug, Clone)]
pub struct ServerStats {
    /// Server photo count for the authenticated user.
    pub server_photos: u64,
    /// Server video count for the authenticated user.
    pub server_videos: u64,
    /// Server total disk size in bytes.
    pub disk_size: u64,
    /// Server used disk space in bytes.
    pub disk_use: u64,
    /// Server disk usage percentage (0–100).
    pub disk_usage_percentage: f64,
}

/// Manages the library's SQLite database.
///
/// Wraps a [`SqlitePool`] and provides typed CRUD methods. Backend-agnostic —
/// both `LocalLibrary` and future backends share this type.
///
/// Obtain via [`Database::open`], which creates the database file if needed
/// and runs all outstanding migrations before returning.
#[derive(Clone)]
pub struct Database {
    pub(crate) pool: SqlitePool,
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
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
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

/// Build a comma-separated list of `?` placeholders for an `IN (...)` clause.
pub(crate) fn id_placeholders(count: usize) -> String {
    let mut s = String::with_capacity(count * 3);
    for i in 0..count {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('?');
    }
    s
}

/// Shared test utilities for all db submodules.
#[cfg(test)]
pub(crate) mod test_helpers {
    use super::Database;
    use crate::library::media::{MediaId, MediaRecord, MediaType};

    pub async fn open_test_db(dir: &std::path::Path) -> Database {
        Database::open(&dir.join("test.db")).await.unwrap()
    }

    pub fn test_record(id: MediaId) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: "2025/01/15/photo.jpg".to_string(),
            original_filename: "photo.jpg".to_string(),
            file_size: 1024,
            imported_at: 1_700_000_000,
            media_type: MediaType::Image,
            taken_at: None,
            width: None,
            height: None,
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        }
    }

    pub fn record_with_taken_at(id: MediaId, path: &str, taken_at: Option<i64>) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: path.to_string(),
            original_filename: path
                .split('/')
                .next_back()
                .unwrap_or("photo.jpg")
                .to_string(),
            file_size: 512,
            imported_at: 1_700_000_000,
            media_type: MediaType::Image,
            taken_at,
            width: Some(1920),
            height: Some(1080),
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        }
    }

    /// Query the audit action and error_msg for a given entity_id (test helper).
    pub async fn get_audit_record(
        db: &Database,
        entity_id: &str,
    ) -> Option<(String, Option<String>)> {
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT action, error_msg FROM sync_audit WHERE entity_id = ?",
        )
        .bind(entity_id)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
    }

    #[allow(dead_code)] // used by db/media.rs filter tests when re-added
    pub fn record_with_imported_at(id: MediaId, path: &str, imported_at: i64) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: path.to_string(),
            original_filename: path
                .split('/')
                .next_back()
                .unwrap_or("photo.jpg")
                .to_string(),
            file_size: 512,
            imported_at,
            media_type: MediaType::Image,
            taken_at: Some(1_000),
            width: Some(1920),
            height: Some(1080),
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_database_file() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("sub").join("moments.db");
        Database::open(&db_path).await.unwrap();
        assert!(db_path.exists());
    }
}
