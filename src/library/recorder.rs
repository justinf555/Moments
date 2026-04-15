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
mod tests {
    use super::*;
    use crate::library::media::MediaId;
    use crate::sync::outbox::NoOpRecorder;
    use std::sync::Arc;

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
