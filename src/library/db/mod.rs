// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Sqlite, SqlitePool};
use tracing::{info, instrument};

use super::error::LibraryError;

/// Manages the library's SQLite database.
///
/// Wraps a [`SqlitePool`] behind an [`Arc`] so that clones share the same
/// connection pool. Create with [`Database::new`], then call [`Database::open`]
/// to connect and run migrations. All clones become active once `open()` returns.
#[derive(Clone)]
pub struct Database {
    pool: std::sync::Arc<tokio::sync::OnceCell<SqlitePool>>,
}

impl Default for Database {
    fn default() -> Self {
        Self::new()
    }
}

impl Database {
    /// Create an uninitialised database handle.
    ///
    /// The handle can be cloned and shared immediately. Call [`open`] to
    /// connect and run migrations — all clones share the same pool.
    pub fn new() -> Self {
        Self {
            pool: std::sync::Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Connect to (or create) the database at `db_path` and run migrations.
    ///
    /// Must be called exactly once. All clones of this handle become active.
    #[instrument(skip(self), fields(path = %db_path.display()))]
    pub async fn open(&self, db_path: &Path) -> Result<(), LibraryError> {
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

        self.pool
            .set(pool)
            .map_err(|_| LibraryError::Runtime("database already opened".to_string()))?;

        info!("database ready");
        Ok(())
    }

    /// Access the connection pool.
    ///
    /// # Panics
    /// Panics if [`open`] has not been called — this is a programming error.
    pub(crate) fn pool(&self) -> &SqlitePool {
        self.pool.get().expect("database not opened")
    }

    /// Begin a write transaction (`BEGIN IMMEDIATE`).
    ///
    /// Services use this to make a business write and its outbox row
    /// (`MutationRecorder::record`) commit or roll back together (#655).
    /// `IMMEDIATE` takes SQLite's write lock up front, so a transaction
    /// that reads before it writes can't fail with `SQLITE_BUSY` when it
    /// upgrades, and it waits on `busy_timeout` behind other writers.
    ///
    /// While a transaction is open, every write in the same task must go
    /// through it: a write on another pool connection would wait for this
    /// one's lock until `busy_timeout` expires.
    pub(crate) async fn begin(&self) -> Result<Tx, LibraryError> {
        self.pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(LibraryError::Db)
    }

    /// Check out a pooled connection for a single non-transactional
    /// statement (used by the pool-based wrappers of `_in_tx` reads).
    pub(crate) async fn acquire(&self) -> Result<PoolConnection<Sqlite>, LibraryError> {
        self.pool().acquire().await.map_err(LibraryError::Db)
    }
}

/// An open write transaction on the library database.
///
/// Repository `_in_tx` methods take `&mut SqliteConnection`; pass
/// `&mut tx` (it derefs) so they run inside the caller's transaction.
pub type Tx = sqlx::Transaction<'static, Sqlite>;

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
        let db = Database::new();
        db.open(&dir.join("test.db")).await.unwrap();
        db
    }

    pub fn test_record(id: MediaId) -> MediaRecord {
        MediaRecord {
            id,
            content_hash: None,
            external_id: None,
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
            is_moments_render: false,
        }
    }

    pub fn record_with_taken_at(id: MediaId, path: &str, taken_at: Option<i64>) -> MediaRecord {
        MediaRecord {
            id,
            content_hash: None,
            external_id: None,
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
            is_moments_render: false,
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
        let db = Database::new();
        db.open(&db_path).await.unwrap();
        assert!(db_path.exists());
    }
}
