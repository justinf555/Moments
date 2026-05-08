use async_trait::async_trait;
use tracing::{instrument, warn};

use crate::library::error::LibraryError;
use crate::library::media::Stack;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct StackHandler;

#[async_trait]
impl SyncEntityHandler for StackHandler {
    fn entity_type(&self) -> &'static str {
        "StackV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let stack: SyncStackV1 = deserialize_entity(data, "StackV1", line_number)?;
        handle_stack(stack, ctx).await?;
        Ok(HandlerResult {
            audit_action: "upsert",
            counter: CounterKind::None,
        })
    }
}

/// Upsert the stack row carrying its primary asset id.
///
/// `AssetV1.stackId` (handled separately by `AssetHandler`) records
/// only membership; the primary lives here. If the primary's local
/// row hasn't streamed yet we warn-and-skip — the next pull cycle
/// re-emits the `StackV1` once the primary is local. This is the
/// only place we translate the Immich primary UUID into the local
/// `MediaId`.
#[instrument(skip(ctx, stack), fields(stack_id = %stack.id))]
async fn handle_stack(stack: SyncStackV1, ctx: &SyncContext) -> Result<(), LibraryError> {
    let Some(primary_local) = ctx
        .library
        .media()
        .id_by_external_id(&stack.primary_asset_id)
        .await?
    else {
        warn!(
            primary_external_id = %stack.primary_asset_id,
            "stack primary not yet local; skipping upsert (will retry next cycle)"
        );
        return Ok(());
    };

    let now = chrono::Utc::now().timestamp();
    ctx.library
        .media()
        .upsert_stack(&Stack {
            id: stack.id.clone(),
            primary_asset_id: primary_local,
            last_seen_at: now,
        })
        .await?;
    ctx.library
        .media()
        .bump_stack_last_seen_at(&stack.id, now)
        .await?;
    Ok(())
}

pub struct StackDeleteHandler;

#[async_trait]
impl SyncEntityHandler for StackDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "StackDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let del: SyncStackDeleteV1 = deserialize_entity(data, "StackDeleteV1", line_number)?;
        ctx.library.media().delete_stack(&del.stack_id).await?;
        Ok(HandlerResult {
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
