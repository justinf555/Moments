use async_trait::async_trait;
use tracing::warn;

use crate::library::album::AlbumId;
use crate::library::error::LibraryError;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct AlbumAssetHandler;

#[async_trait]
impl SyncEntityHandler for AlbumAssetHandler {
    fn entity_type(&self) -> &'static str {
        "AlbumToAssetV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let assoc: SyncAlbumToAssetV1 = deserialize_entity(data, "AlbumToAssetV1", line_number)?;
        let id = format!("{}:{}", assoc.album_id, assoc.asset_id);

        // Issue #626: `assoc.asset_id` is the Immich UUID; `album_media.media_id`
        // references the local `MediaId`. Translate via `external_id` lookup.
        // Assumes Immich emits the parent asset before its album-membership row
        // — if the lookup misses, we warn and skip rather than persist a
        // dangling FK pointing at the server-side UUID.
        let media_id = match ctx
            .library
            .media()
            .id_by_external_id(&assoc.asset_id)
            .await?
        {
            Some(id) => id,
            None => {
                warn!(
                    album_id = %assoc.album_id,
                    asset_id = %assoc.asset_id,
                    "AlbumToAssetV1: parent asset not found locally; skipping membership row"
                );
                return Ok(HandlerResult {
                    entity_id: id,
                    local_media_id: None,
                    audit_action: "upsert",
                    counter: CounterKind::Albums,
                });
            }
        };

        let now = chrono::Utc::now().timestamp();
        ctx.db
            .upsert_album_media(&assoc.album_id, media_id.as_str(), now)
            .await?;
        ctx.library
            .albums()
            .emit_album_media_changed(&AlbumId::from_raw(assoc.album_id));

        Ok(HandlerResult {
            entity_id: id,
            local_media_id: None,
            audit_action: "upsert",
            counter: CounterKind::Albums,
        })
    }
}

pub struct AlbumAssetDeleteHandler;

#[async_trait]
impl SyncEntityHandler for AlbumAssetDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "AlbumToAssetDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let assoc: SyncAlbumToAssetDeleteV1 =
            deserialize_entity(data, "AlbumToAssetDeleteV1", line_number)?;
        let id = format!("{}:{}", assoc.album_id, assoc.asset_id);

        // Issue #626: `assoc.asset_id` is the Immich UUID; the
        // album_media row references the local `MediaId`. Translate
        // before deleting. If the parent media row has already been
        // removed locally, the album_media join row went with it via
        // the manual cascade in `delete_permanently`, so there's
        // nothing left to clean up — match the add-path's pattern of
        // warn-and-skip rather than firing a delete with a key that
        // can't possibly hit anything.
        let media_id = match ctx
            .library
            .media()
            .id_by_external_id(&assoc.asset_id)
            .await?
        {
            Some(id) => id,
            None => {
                warn!(
                    album_id = %assoc.album_id,
                    asset_id = %assoc.asset_id,
                    "AlbumToAssetDeleteV1: parent asset not found locally; nothing to delete"
                );
                return Ok(HandlerResult {
                    entity_id: id,
                    local_media_id: None,
                    audit_action: "delete",
                    counter: CounterKind::Deletes,
                });
            }
        };

        ctx.db
            .delete_album_media_entry(&assoc.album_id, media_id.as_str())
            .await?;
        ctx.library
            .albums()
            .emit_album_media_changed(&AlbumId::from_raw(assoc.album_id));

        Ok(HandlerResult {
            entity_id: id,
            local_media_id: None,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
