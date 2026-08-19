// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mutation recording trait.
//!
//! Services call [`MutationRecorder::record`] after each successful
//! mutation. The implementation decides what to do — write to an outbox
//! table (Immich) or do nothing (local backend).

use async_trait::async_trait;

use super::error::LibraryError;
use super::mutation::Mutation;

/// Records library mutations for downstream consumers.
///
/// Injected into services at construction time via `Arc<dyn MutationRecorder>`.
#[async_trait]
pub trait MutationRecorder: Send + Sync {
    async fn record(&self, mutation: &Mutation) -> Result<(), LibraryError>;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::library::media::MediaId;
    use crate::sync::outbox::NoOpRecorder;
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
        async fn record(&self, mutation: &Mutation) -> Result<(), LibraryError> {
            self.recorded.lock().unwrap().push(mutation.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe() {
        // Verify MutationRecorder can be used as a trait object.
        let recorder: Arc<dyn MutationRecorder> = Arc::new(NoOpRecorder);
        let result = recorder
            .record(&Mutation::AssetTrashed {
                ids: vec![MediaId::new("test".to_string())],
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn trait_object_is_send_sync() {
        // Verify the trait object satisfies Send + Sync bounds.
        fn assert_send_sync<T: Send + Sync>(_val: &T) {}
        let recorder: Arc<dyn MutationRecorder> = Arc::new(NoOpRecorder);
        assert_send_sync(&recorder);
    }
}
