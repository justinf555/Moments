// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use async_trait::async_trait;
use tracing::{instrument, warn};

use crate::library::error::LibraryError;
use crate::library::faces::repository::AssetFaceRow;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct AssetFaceHandler;

#[async_trait]
impl SyncEntityHandler for AssetFaceHandler {
    fn entity_type(&self) -> &'static str {
        "AssetFaceV2"
    }

    #[instrument(skip(self, data, ctx), fields(entity = "AssetFaceV2"))]
    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let face: SyncAssetFaceV2 = deserialize_entity(data, "AssetFaceV2", line_number)?;

        // Issue #626: `face.asset_id` is the Immich UUID; `asset_faces.asset_id`
        // references the local `MediaId`. Translate via `external_id` lookup;
        // skip with a warning if the parent asset hasn't been processed yet.
        let local_asset_id = match ctx
            .library
            .media()
            .id_by_external_id(&face.asset_id)
            .await?
        {
            Some(local) => local.as_str().to_string(),
            None => {
                warn!(
                    face_id = %face.id,
                    asset_id = %face.asset_id,
                    "AssetFaceV2: parent asset not found locally; skipping face row"
                );
                return Ok(HandlerResult {
                    audit_action: "upsert",
                    counter: CounterKind::Faces,
                });
            }
        };

        let row = AssetFaceRow {
            id: face.id,
            asset_id: local_asset_id,
            person_id: face.person_id.clone(),
            image_width: face.image_width,
            image_height: face.image_height,
            bbox_x1: face.bounding_box_x1,
            bbox_y1: face.bounding_box_y1,
            bbox_x2: face.bounding_box_x2,
            bbox_y2: face.bounding_box_y2,
            source_type: face
                .source_type
                .unwrap_or_else(|| "MachineLearning".to_string()),
            // Issue #680: a face the server hides or soft-deletes stays
            // in the table but stops contributing to its person's grid
            // and face_count. The hard delete still arrives separately
            // as `AssetFaceDeleteV1`.
            is_visible: face.is_visible,
            deleted_at: parse_datetime(&face.deleted_at),
        };

        ctx.library.faces().upsert_asset_face(&row).await?;

        // Issue #628: bump heartbeat so reset-cycle orphan sweeps see
        // this face as "still alive on the server".
        let now = chrono::Utc::now().timestamp();
        ctx.library
            .faces()
            .bump_asset_face_last_seen_at(&row.id, now)
            .await?;

        if let Some(ref person_id) = face.person_id {
            ctx.library.faces().update_face_count(person_id).await?;
        }

        Ok(HandlerResult {
            audit_action: "upsert",
            counter: CounterKind::Faces,
        })
    }
}

pub struct AssetFaceDeleteHandler;

#[async_trait]
impl SyncEntityHandler for AssetFaceDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "AssetFaceDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let delete: SyncAssetFaceDeleteV1 =
            deserialize_entity(data, "AssetFaceDeleteV1", line_number)?;
        let id = delete.asset_face_id.clone();
        ctx.library.faces().delete_asset_face(&id).await?;
        Ok(HandlerResult {
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
