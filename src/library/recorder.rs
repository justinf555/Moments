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
