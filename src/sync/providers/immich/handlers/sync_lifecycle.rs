use async_trait::async_trait;

use crate::library::error::LibraryError;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};

/// Signals a full resync — clears faces/people and loads existing IDs.
///
/// The caller (PullManager) checks `entity_type == "SyncResetV1"` to set
/// its reset-tracking state *before* delegating here. This handler
/// performs the database cleanup.
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
        ctx.library.faces().clear_asset_faces().await?;
        ctx.library.faces().clear_people().await?;
        ctx.db.clear_sync_checkpoints().await?;
        Ok(HandlerResult {
            entity_id: String::new(),
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
            entity_id: String::new(),
            audit_action: "complete",
            counter: CounterKind::None,
        })
    }
}
