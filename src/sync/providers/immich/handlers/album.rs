use async_trait::async_trait;
use tracing::instrument;

use crate::app_event::AppEvent;
use crate::library::album::AlbumId;
use crate::library::error::LibraryError;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct AlbumHandler;

#[async_trait]
impl SyncEntityHandler for AlbumHandler {
    fn entity_type(&self) -> &'static str {
        "AlbumV1"
    }

    #[instrument(skip(self, data, ctx), fields(entity = "AlbumV1"))]
    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let album: SyncAlbumV1 = deserialize_entity(data, "AlbumV1", line_number)?;
        let id = album.id.clone();

        let created_at = parse_datetime(&Some(album.created_at)).unwrap_or(0);
        let updated_at = parse_datetime(&Some(album.updated_at)).unwrap_or(0);

        ctx.library
            .albums()
            .upsert_album(&album.id, &album.name, created_at, updated_at, Some(&album.id))
            .await?;

        ctx.events.send(AppEvent::AlbumCreated {
            id: AlbumId::from_raw(album.id),
            name: album.name,
        });

        Ok(HandlerResult {
            entity_id: id,
            audit_action: "upsert",
            counter: CounterKind::Albums,
        })
    }
}

pub struct AlbumDeleteHandler;

#[async_trait]
impl SyncEntityHandler for AlbumDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "AlbumDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let delete: SyncAlbumDeleteV1 =
            deserialize_entity(data, "AlbumDeleteV1", line_number)?;
        let id_str = delete.album_id.clone();
        let id = AlbumId::from_raw(id_str.clone());
        ctx.library.albums().delete_album(&id).await?;
        ctx.events.send(AppEvent::AlbumDeleted { id });
        Ok(HandlerResult {
            entity_id: id_str,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
