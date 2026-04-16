use async_trait::async_trait;

use crate::app_event::AppEvent;
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

        let now = chrono::Utc::now().timestamp();
        ctx.db
            .upsert_album_media(&assoc.album_id, &assoc.asset_id, now)
            .await?;
        ctx.events.send(AppEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

        Ok(HandlerResult {
            entity_id: id,
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

        ctx.db
            .delete_album_media_entry(&assoc.album_id, &assoc.asset_id)
            .await?;
        ctx.events.send(AppEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

        Ok(HandlerResult {
            entity_id: id,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
