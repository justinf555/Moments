use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::library::error::LibraryError;
use crate::library::media::{MediaId, MediaRecord, MediaType};
use crate::library::thumbnail::{sharded_original_relative, sharded_thumbnail_path};

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct AssetHandler;

#[async_trait]
impl SyncEntityHandler for AssetHandler {
    fn entity_type(&self) -> &'static str {
        "AssetV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let asset: SyncAssetV1 = deserialize_entity(data, "AssetV1", line_number)?;
        let id = asset.id.clone();
        handle_asset(asset, ctx).await?;
        Ok(HandlerResult {
            entity_id: id,
            audit_action: "upsert",
            counter: CounterKind::Assets,
        })
    }
}

#[instrument(skip(ctx, asset), fields(asset_id = %asset.id))]
async fn handle_asset(asset: SyncAssetV1, ctx: &SyncContext) -> Result<(), LibraryError> {
    let media_type = match asset.asset_type.as_str() {
        "VIDEO" => MediaType::Video,
        _ => MediaType::Image,
    };

    let taken_at =
        parse_datetime(&asset.local_date_time).or_else(|| parse_datetime(&asset.file_created_at));

    // `imported_at` means "when did this asset enter the local library", not
    // "when was the photo taken". For a row we've never seen, that's now;
    // for a row that already exists, the repository preserves the prior
    // value during upsert. See issue #614 — previously this was set to the
    // server's `file_created_at`, which Immich's own EXIF-extraction job
    // overwrites with the photo's capture date, silently moving assets
    // out of the Recent Imports view as soon as they round-tripped.
    let imported_at = chrono::Utc::now().timestamp();

    let is_trashed = asset.deleted_at.is_some();
    let trashed_at = parse_datetime(&asset.deleted_at);
    let duration_ms = asset.duration.as_deref().and_then(parse_duration_ms);

    let id_str = asset.id.clone();
    let record = MediaRecord {
        id: MediaId::new(id_str.clone()),
        content_hash: None,
        external_id: Some(id_str.clone()),
        relative_path: sharded_original_relative(&MediaId::new(id_str.clone())),
        original_filename: asset.original_file_name,
        file_size: 0,
        imported_at,
        media_type,
        taken_at,
        width: asset.width,
        height: asset.height,
        orientation: 1,
        duration_ms,
        is_favorite: asset.is_favorite,
        is_trashed,
        trashed_at,
    };

    let media_id = record.id.clone();
    ctx.library.media().upsert_media(&record).await?;

    if let Err(e) =
        download_thumbnail(&ctx.client, &ctx.library, &ctx.thumbnails_dir, &media_id).await
    {
        debug!(id = %media_id, "thumbnail download failed: {e}");
    }

    Ok(())
}

pub struct AssetDeleteHandler;

#[async_trait]
impl SyncEntityHandler for AssetDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "AssetDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let delete: SyncAssetDeleteV1 = deserialize_entity(data, "AssetDeleteV1", line_number)?;
        let id = delete.asset_id.clone();
        let media_id = MediaId::new(id.clone());
        ctx.library
            .delete_permanently_from_sync(std::slice::from_ref(&media_id))
            .await?;
        Ok(HandlerResult {
            entity_id: id,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}

/// Download a single thumbnail from Immich and write it to the local cache.
///
/// `set_thumbnail_ready` on the thumbnail service emits
/// `ThumbnailEvent::Ready`, so subscribers (e.g. `MediaClientV2`) get
/// notified through the per-service channel — no bus emission needed.
#[instrument(skip(client, library, thumbnails_dir))]
async fn download_thumbnail(
    client: &super::super::client::ImmichClient,
    library: &crate::library::Library,
    thumbnails_dir: &std::path::Path,
    media_id: &MediaId,
) -> Result<(), LibraryError> {
    let path = sharded_thumbnail_path(thumbnails_dir, media_id);

    if path.exists() {
        debug!("thumbnail already cached, skipping download");
        let now = chrono::Utc::now().timestamp();
        library
            .thumbnails()
            .set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
            .await?;
        return Ok(());
    }

    let api_path = format!("/assets/{}/thumbnail?size=thumbnail", media_id.as_str());
    let bytes = client.get_bytes(&api_path).await?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(LibraryError::Io)?;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(LibraryError::Io)?;

    let now = chrono::Utc::now().timestamp();
    library
        .thumbnails()
        .set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
        .await?;

    Ok(())
}
