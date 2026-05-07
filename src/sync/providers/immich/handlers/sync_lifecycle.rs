use async_trait::async_trait;

use crate::library::error::LibraryError;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};

/// Signals a full resync — clears stream-resume state.
///
/// The caller (PullManager) inspects `entity_type == "SyncResetV1"`
/// to set its in-memory reset checkpoint before delegating here. This
/// handler clears the per-entity-type ack checkpoints so the next
/// pull starts at the head of each stream.
///
/// Issue #628: previously this also wiped `asset_faces` and `people`
/// to be rebuilt by the stream. The heartbeat reconciliation makes
/// that destructive clear unnecessary — those tables now reconcile
/// the same way as `media` and `albums` (rows whose `last_seen_at`
/// lags the cycle's checkpoint are deleted at `SyncCompleteV1`),
/// preserving cached data through the reset window.
pub struct SyncResetHandler;

#[async_trait]
impl SyncEntityHandler for SyncResetHandler {
    fn entity_type(&self) -> &'static str {
        "SyncResetV1"
    }

    async fn handle(
        &self,
        _data: &serde_json::Value,
        _line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        ctx.state.clear_checkpoints().await?;
        Ok(HandlerResult {
            audit_action: "reset",
            counter: CounterKind::None,
        })
    }
}

/// Marks the end of the sync stream. The PullManager breaks the loop
/// when it sees this entity type. The handler itself is a no-op.
pub struct SyncCompleteHandler;

#[async_trait]
impl SyncEntityHandler for SyncCompleteHandler {
    fn entity_type(&self) -> &'static str {
        "SyncCompleteV1"
    }

    async fn handle(
        &self,
        _data: &serde_json::Value,
        _line_number: usize,
        _ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        Ok(HandlerResult {
            audit_action: "complete",
            counter: CounterKind::None,
        })
    }
}
