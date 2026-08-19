// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use async_trait::async_trait;
use tracing::{instrument, warn};

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

        // Issue #628: bump heartbeat so reset-cycle orphan sweeps see
        // this album as "still alive on the server".
        let now = chrono::Utc::now().timestamp();
        ctx.library
            .albums()
            .bump_last_seen_at(&local_id, now)
            .await?;

        Ok(HandlerResult {
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
        let external_id = delete.album_id.clone();

        // Issue #585: `delete.album_id` is the Immich UUID; the local
        // row is keyed by the locally-minted `AlbumId`. Translate via
        // `external_id` lookup before deleting. Warn-and-skip on miss
        // — the album either was never pulled locally or has already
        // been removed.
        let local_id = match ctx.library.albums().id_by_external_id(&external_id).await? {
            Some(id) => id,
            None => {
                warn!(
                    album_id = %external_id,
                    "AlbumDeleteV1: no local row for this external_id; nothing to delete"
                );
                return Ok(HandlerResult {
                    audit_action: "delete",
                    counter: CounterKind::Deletes,
                });
            }
        };

        ctx.library.albums().delete_album(&local_id).await?;
        Ok(HandlerResult {
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
