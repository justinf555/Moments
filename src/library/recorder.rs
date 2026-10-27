// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mutation recording trait.
//!
//! Services call [`MutationRecorder::record`] inside the transaction
//! that makes the change. The implementation decides what to do — write
//! to an outbox table (Immich) or do nothing (local backend).

use async_trait::async_trait;
use sqlx::Sqlite;

use super::error::LibraryError;
use super::mutation::Mutation;

/// Records library mutations for downstream consumers.
///
/// Injected into services at construction time via `Arc<dyn MutationRecorder>`.
///
/// This is a transactional outbox (#655): the caller opens a transaction
/// ([`crate::library::db::Database::begin`]), makes the business write
/// on it, calls `record` with the same transaction and then commits. An
/// error from `record` must propagate so the transaction rolls back —
/// the business write and its outbox rows land together or not at all.
#[async_trait]
pub trait MutationRecorder: Send + Sync {
    async fn record(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        mutation: &Mutation,
    ) -> Result<(), LibraryError>;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::library::db::Database;
    use crate::library::media::MediaId;
    use crate::sync::outbox::{NoOpRecorder, QueueWriterOutbox};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Test fixture: records every mutation passed to `record` so
    /// assertions can inspect the outbox-bound sequence emitted by
    /// services and cross-service orchestrators.
    #[derive(Default)]
    pub(crate) struct CapturingRecorder {
        recorded: Mutex<Vec<Mutation>>,
    }

    impl CapturingRecorder {
        pub(crate) fn snapshot(&self) -> Vec<Mutation> {
            self.recorded.lock().unwrap().clone()
        }

        pub(crate) fn clear(&self) {
            self.recorded.lock().unwrap().clear();
        }
    }

    #[async_trait]
    impl MutationRecorder for CapturingRecorder {
        async fn record(
            &self,
            _tx: &mut sqlx::Transaction<'_, Sqlite>,
            mutation: &Mutation,
        ) -> Result<(), LibraryError> {
            self.recorded.lock().unwrap().push(mutation.clone());
            Ok(())
        }
    }

    /// Test fixture for #655 / #709: a real outbox writer that fails
    /// *after* writing its rows for any mutation matching `fail_on`.
    ///
    /// Because the rows are already written on the caller's transaction
    /// when the error comes back, a test can assert that the rollback
    /// removed both the business write and the outbox rows — the same
    /// observable state as a process killed between `begin` and
    /// `commit`.
    pub(crate) struct FailingRecorder {
        inner: QueueWriterOutbox,
        fail_on: Box<dyn Fn(&Mutation) -> bool + Send + Sync>,
    }

    impl FailingRecorder {
        pub(crate) fn new(
            db: Database,
            fail_on: impl Fn(&Mutation) -> bool + Send + Sync + 'static,
        ) -> Self {
            Self {
                inner: QueueWriterOutbox::new(db),
                fail_on: Box::new(fail_on),
            }
        }

        /// Fail on every mutation.
        pub(crate) fn always(db: Database) -> Self {
            Self::new(db, |_| true)
        }
    }

    #[async_trait]
    impl MutationRecorder for FailingRecorder {
        async fn record(
            &self,
            tx: &mut sqlx::Transaction<'_, Sqlite>,
            mutation: &Mutation,
        ) -> Result<(), LibraryError> {
            self.inner.record(tx, mutation).await?;
            if (self.fail_on)(mutation) {
                return Err(LibraryError::Runtime(
                    "injected recorder failure".to_string(),
                ));
            }
            Ok(())
        }
    }

    /// Number of rows in `sync_outbox`.
    pub(crate) async fn outbox_count(db: &Database) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_outbox")
            .fetch_one(db.pool())
            .await
            .unwrap();
        n
    }

    #[tokio::test]
    async fn trait_is_object_safe() {
        // Verify MutationRecorder can be used as a trait object.
        let dir = tempfile::tempdir().unwrap();
        let db = crate::library::db::test_helpers::open_test_db(dir.path()).await;
        let recorder: Arc<dyn MutationRecorder> = Arc::new(NoOpRecorder);
        let mut tx = db.begin().await.unwrap();
        let result = recorder
            .record(
                &mut tx,
                &Mutation::AssetTrashed {
                    ids: vec![MediaId::new("test".to_string())],
                },
            )
            .await;
        assert!(result.is_ok());
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn trait_object_is_send_sync() {
        // Verify the trait object satisfies Send + Sync bounds.
        fn assert_send_sync<T: Send + Sync>(_val: &T) {}
        let recorder: Arc<dyn MutationRecorder> = Arc::new(NoOpRecorder);
        assert_send_sync(&recorder);
    }
}
