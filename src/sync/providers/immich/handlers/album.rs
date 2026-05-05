use async_trait::async_trait;
use tracing::instrument;

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
        let external_id = album.id.clone();

        let created_at = parse_datetime(&Some(album.created_at)).unwrap_or(0);
        let updated_at = parse_datetime(&Some(album.updated_at)).unwrap_or(0);

        // Issue #585: keep the locally-owned `AlbumId` stable across the
        // create→push→pull round-trip. The Immich UUID lives only in
        // `external_id` — never as the primary key. If a local row already
        // carries this `external_id` (i.e. push has stamped it), upsert
        // under the existing local id; otherwise mint a fresh one for the
        // server-origin album.
        let local_id = match ctx.library.albums().id_by_external_id(&external_id).await? {
            Some(existing) => existing,
            None => AlbumId::new(),
        };

        ctx.library
            .albums()
            .upsert_album(
                local_id.as_str(),
                &album.name,
                created_at,
                updated_at,
                Some(&external_id),
            )
            .await?;

        Ok(HandlerResult {
            entity_id: external_id,
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
        let delete: SyncAlbumDeleteV1 = deserialize_entity(data, "AlbumDeleteV1", line_number)?;
        let id_str = delete.album_id.clone();
        let id = AlbumId::from_raw(id_str.clone());
        ctx.library.albums().delete_album(&id).await?;
        Ok(HandlerResult {
            entity_id: id_str,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
