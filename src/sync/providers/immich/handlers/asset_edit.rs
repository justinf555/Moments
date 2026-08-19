// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pull-side handlers for Immich's per-action edit stream.
//!
//! Each `SyncAssetEditV1` carries one geometric action (`crop`,
//! `rotate`, `mirror`) with a sequence number. We cache the actions in
//! `immich_asset_edits` keyed by Immich edit UUID, then recompose an
//! `EditState` from all rows for the asset and write it back to the
//! library's `edits` table. Issue #224 Phase B.

use async_trait::async_trait;
use tracing::{instrument, warn};

use crate::library::db::Database;
use crate::library::editing::repository::EditingRepository;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::edit_action::{recompose, ImageDims, ImmichEditAction};
use crate::sync::providers::immich::types::*;

pub struct AssetEditHandler;

#[async_trait]
impl SyncEntityHandler for AssetEditHandler {
    fn entity_type(&self) -> &'static str {
        "AssetEditV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let edit: SyncAssetEditV1 = deserialize_entity(data, "AssetEditV1", line_number)?;
        handle_asset_edit(edit, ctx).await?;
        Ok(HandlerResult {
            audit_action: "upsert",
            counter: CounterKind::None,
        })
    }
}

pub struct AssetEditDeleteHandler;

#[async_trait]
impl SyncEntityHandler for AssetEditDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "AssetEditDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let del: SyncAssetEditDeleteV1 =
            deserialize_entity(data, "AssetEditDeleteV1", line_number)?;
        handle_asset_edit_delete(del, ctx).await?;
        Ok(HandlerResult {
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}

#[instrument(skip(ctx, edit), fields(edit_id = %edit.id, asset_id = %edit.asset_id))]
async fn handle_asset_edit(edit: SyncAssetEditV1, ctx: &SyncContext) -> Result<(), LibraryError> {
    let Some(media_id) = ctx
        .library
        .media()
        .id_by_external_id(&edit.asset_id)
        .await?
    else {
        warn!(
            asset_id = %edit.asset_id,
            "asset edit arrived before parent asset; skipping (will retry next cycle)"
        );
        return Ok(());
    };

    let now = chrono::Utc::now().timestamp();
    upsert_action(&ctx.db, &edit, &media_id, now).await?;
    refresh_edit_state(&ctx.db, ctx, &media_id).await?;
    Ok(())
}

#[instrument(skip(ctx, del), fields(edit_id = %del.edit_id))]
async fn handle_asset_edit_delete(
    del: SyncAssetEditDeleteV1,
    ctx: &SyncContext,
) -> Result<(), LibraryError> {
    let Some(media_id) = lookup_media_id_by_edit_id(&ctx.db, &del.edit_id).await? else {
        // Row already gone (cascade or a duplicate delete).
        return Ok(());
    };
    delete_action(&ctx.db, &del.edit_id).await?;
    refresh_edit_state(&ctx.db, ctx, &media_id).await?;
    Ok(())
}

/// Read all cached actions for the asset, recompose an `EditState`,
/// and write it back to the user-facing `edits` table — bypassing the
/// recorder (this is a server-driven update). If the recomposed state
/// is the identity, the `edits` row is deleted.
async fn refresh_edit_state(
    db: &Database,
    ctx: &SyncContext,
    media_id: &MediaId,
) -> Result<(), LibraryError> {
    let actions = list_actions_for_media(db, media_id).await?;

    let editing_repo = EditingRepository::new(db.clone());

    if actions.is_empty() {
        editing_repo.delete_edit_state(media_id).await?;
        return Ok(());
    }

    // Crop translation needs the original image dims. If the media row
    // doesn't have them, keep the cached actions but skip the EditState
    // write — a later pull (after import populates dims) will retry.
    let Some(item) = ctx.library.media().get_media_item(media_id).await? else {
        warn!(id = %media_id, "media not found while recomposing edit state; skipping");
        return Ok(());
    };
    let (Some(w), Some(h)) = (item.width, item.height) else {
        warn!(id = %media_id, "media missing dims; skipping recomposed edit state write");
        return Ok(());
    };
    let dims = ImageDims {
        width: w as u32,
        height: h as u32,
    };

    let state = recompose(&actions, dims);
    if state.is_identity() {
        editing_repo.delete_edit_state(media_id).await?;
    } else {
        editing_repo.upsert_edit_state(media_id, &state).await?;
    }
    Ok(())
}

// ── DB helpers ──────────────────────────────────────────────────────

async fn upsert_action(
    db: &Database,
    edit: &SyncAssetEditV1,
    media_id: &MediaId,
    now: i64,
) -> Result<(), LibraryError> {
    let parameters_json = serde_json::to_string(&edit.parameters)
        .map_err(|e| LibraryError::Runtime(format!("serialize edit parameters: {e}")))?;

    sqlx::query(
        "INSERT INTO immich_asset_edits
            (id, media_id, action, parameters_json, sequence, last_seen_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            media_id = excluded.media_id,
            action = excluded.action,
            parameters_json = excluded.parameters_json,
            sequence = excluded.sequence,
            last_seen_at = excluded.last_seen_at",
    )
    .bind(&edit.id)
    .bind(media_id.as_str())
    .bind(&edit.action)
    .bind(&parameters_json)
    .bind(edit.sequence)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(LibraryError::Db)?;

    Ok(())
}

async fn delete_action(db: &Database, edit_id: &str) -> Result<(), LibraryError> {
    sqlx::query("DELETE FROM immich_asset_edits WHERE id = ?")
        .bind(edit_id)
        .execute(db.pool())
        .await
        .map_err(LibraryError::Db)?;
    Ok(())
}

async fn lookup_media_id_by_edit_id(
    db: &Database,
    edit_id: &str,
) -> Result<Option<MediaId>, LibraryError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT media_id FROM immich_asset_edits WHERE id = ?")
            .bind(edit_id)
            .fetch_optional(db.pool())
            .await
            .map_err(LibraryError::Db)?;
    Ok(row.map(|(s,)| MediaId::new(s)))
}

async fn list_actions_for_media(
    db: &Database,
    media_id: &MediaId,
) -> Result<Vec<ImmichEditAction>, LibraryError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, parameters_json FROM immich_asset_edits
         WHERE media_id = ?
         ORDER BY sequence ASC",
    )
    .bind(media_id.as_str())
    .fetch_all(db.pool())
    .await
    .map_err(LibraryError::Db)?;

    let mut actions = Vec::with_capacity(rows.len());
    for (action, parameters_json) in rows {
        let parameters: serde_json::Value = serde_json::from_str(&parameters_json)
            .map_err(|e| LibraryError::Runtime(format!("parse cached edit parameters: {e}")))?;
        // Round-trip via ImmichEditAction's tagged form.
        let value = serde_json::json!({ "action": action, "parameters": parameters });
        match serde_json::from_value::<ImmichEditAction>(value) {
            Ok(a) => actions.push(a),
            Err(e) => {
                warn!(action = %action, "skipping unrecognised cached edit action: {e}");
            }
        }
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};
    use crate::library::media::repository::MediaRepository;

    fn sample_edit(id: &str, asset_id: &str, action: &str, sequence: i64) -> SyncAssetEditV1 {
        SyncAssetEditV1 {
            id: id.into(),
            asset_id: asset_id.into(),
            action: action.into(),
            parameters: match action {
                "rotate" => serde_json::json!({"angle": 90}),
                "mirror" => serde_json::json!({"axis": "horizontal"}),
                "crop" => serde_json::json!({"x": 0, "y": 0, "width": 100, "height": 50}),
                _ => serde_json::json!({}),
            },
            sequence,
        }
    }

    #[tokio::test]
    async fn upsert_then_list_round_trips_in_sequence_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let mid = MediaId::new("photo-1".into());
        media.insert(&test_record(mid.clone())).await.unwrap();

        // Upsert out-of-order — list_actions_for_media must order by sequence.
        upsert_action(&db, &sample_edit("e2", "ext-1", "mirror", 1), &mid, 100)
            .await
            .unwrap();
        upsert_action(&db, &sample_edit("e1", "ext-1", "rotate", 0), &mid, 100)
            .await
            .unwrap();
        upsert_action(&db, &sample_edit("e3", "ext-1", "crop", 2), &mid, 100)
            .await
            .unwrap();

        let actions = list_actions_for_media(&db, &mid).await.unwrap();
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], ImmichEditAction::Rotate { .. }));
        assert!(matches!(actions[1], ImmichEditAction::Mirror { .. }));
        assert!(matches!(actions[2], ImmichEditAction::Crop { .. }));
    }

    #[tokio::test]
    async fn upsert_with_existing_id_replaces_payload() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let mid = MediaId::new("photo-1".into());
        media.insert(&test_record(mid.clone())).await.unwrap();

        let mut edit = sample_edit("e1", "ext-1", "rotate", 0);
        edit.parameters = serde_json::json!({"angle": 90});
        upsert_action(&db, &edit, &mid, 100).await.unwrap();

        edit.parameters = serde_json::json!({"angle": 270});
        upsert_action(&db, &edit, &mid, 200).await.unwrap();

        let actions = list_actions_for_media(&db, &mid).await.unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], ImmichEditAction::Rotate { angle: 270 });
    }

    #[tokio::test]
    async fn delete_action_removes_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let mid = MediaId::new("photo-1".into());
        media.insert(&test_record(mid.clone())).await.unwrap();

        upsert_action(&db, &sample_edit("e1", "ext-1", "rotate", 0), &mid, 100)
            .await
            .unwrap();
        delete_action(&db, "e1").await.unwrap();

        let actions = list_actions_for_media(&db, &mid).await.unwrap();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn lookup_media_id_by_edit_id_returns_owner() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let mid = MediaId::new("photo-1".into());
        media.insert(&test_record(mid.clone())).await.unwrap();

        upsert_action(&db, &sample_edit("e1", "ext-1", "rotate", 0), &mid, 100)
            .await
            .unwrap();

        let found = lookup_media_id_by_edit_id(&db, "e1").await.unwrap();
        assert_eq!(found.unwrap().as_str(), "photo-1");

        let missing = lookup_media_id_by_edit_id(&db, "nonexistent")
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn cached_actions_cascade_on_media_delete() {
        // FK is `ON DELETE CASCADE`; per the SQLite-FK memory, sqlx 0.8
        // enables PRAGMA foreign_keys per connection.
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let mid = MediaId::new("photo-1".into());
        media.insert(&test_record(mid.clone())).await.unwrap();

        upsert_action(&db, &sample_edit("e1", "ext-1", "rotate", 0), &mid, 100)
            .await
            .unwrap();

        sqlx::query("DELETE FROM media WHERE id = ?")
            .bind(mid.as_str())
            .execute(db.pool())
            .await
            .unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM immich_asset_edits")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[test]
    fn deserialises_sync_wire_shape() {
        // Spec: per-action record on /sync/stream as AssetEditV1.
        let json = serde_json::json!({
            "id": "edit-uuid-1",
            "assetId": "asset-uuid-1",
            "action": "rotate",
            "parameters": {"angle": 90},
            "sequence": 0
        });
        let edit: SyncAssetEditV1 = serde_json::from_value(json).unwrap();
        assert_eq!(edit.id, "edit-uuid-1");
        assert_eq!(edit.asset_id, "asset-uuid-1");
        assert_eq!(edit.action, "rotate");
        assert_eq!(edit.sequence, 0);
    }

    #[test]
    fn delete_payload_carries_edit_id() {
        // Verified live against v2.7.5: payload is `{"editId": "<uuid>"}`.
        // Other delete entities follow `<entity>Id` (assetId, stackId,
        // …) but this one uses the shorter `editId`.
        let json = serde_json::json!({"editId": "edit-uuid-1"});
        let del: SyncAssetEditDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.edit_id, "edit-uuid-1");
    }
}
