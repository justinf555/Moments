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
        handle_asset(asset, ctx).await?;
        Ok(HandlerResult {
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

    // Issue #626: keep the locally-owned `MediaId` stable across the
    // upload→pull round-trip. The Immich UUID lives only in
    // `external_id` from now on — never as the primary key.
    //
    // Resolution order:
    //   1. `external_id` — already-stamped row from a previous sync
    //      cycle or a completed push. This is the steady-state path.
    //   2. `content_hash` (only if Immich emitted `checksum` and only
    //      against rows with `external_id IS NULL`) — adopt a local
    //      row whose push hasn't yet stamped the server id. This
    //      closes the import-then-pull race that would otherwise
    //      create a parallel stub row and wedge push's external_id
    //      stamp behind a UNIQUE-constraint violation.
    //   3. Generate a fresh `MediaId` — server-origin asset that has
    //      no local twin.
    let external_id = asset.id.clone();
    let media_id =
        if let Some(existing) = ctx.library.media().id_by_external_id(&external_id).await? {
            existing
        } else if let Some(hash) = asset.checksum.as_deref() {
            match ctx
                .library
                .media()
                .id_by_content_hash_pending_push(hash)
                .await?
            {
                Some(local) => local,
                None => MediaId::generate(),
            }
        } else {
            MediaId::generate()
        };

    // Issue #626 follow-up: populate `content_hash` from Immich's own
    // `checksum` field (SHA-1 base64). The local importer hashes the
    // same way, so a file pulled from Immich and then re-selected in
    // the import dialog is rejected as a duplicate without needing to
    // download the original first.
    let record = MediaRecord {
        id: media_id.clone(),
        content_hash: asset.checksum,
        external_id: Some(external_id),
        relative_path: sharded_original_relative(&media_id),
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
        // Phase C (#224): set on the pull side from `tags[]` once tag
        // sync lands. Default-false here covers the bootstrap before
        // the tag stream is wired (Phase D scope).
        is_moments_render: false,
    };

    let server_id = record.external_id.clone().expect("external_id set above");
    ctx.library.media().upsert_media(&record).await?;

    // Issue #628: bump heartbeat after the upsert so reset-cycle
    // orphan sweeps see this asset as "still alive on the server".
    // INSERT OR REPLACE resets last_seen_at to its DEFAULT (0); this
    // restores it to the cycle's wall time.
    let now = chrono::Utc::now().timestamp();
    ctx.library
        .media()
        .bump_last_seen_at(&media_id, now)
        .await?;

    // Issue #224: reflect server-side stack membership locally. The
    // matching `SyncStackV1` (carrying the primary asset id) arrives
    // on its own stream and is handled by `StackHandler` — here we
    // just record the asset's `stackId` pointer. `now` is shared
    // with the stub heartbeat so a same-cycle reset sweep doesn't
    // delete a stub that hasn't yet seen its real StackV1.
    apply_stack_membership(asset.stack_id.as_deref(), &media_id, ctx, now).await?;

    if let Err(e) = download_thumbnail(
        &ctx.client,
        &ctx.library,
        &ctx.thumbnails_dir,
        &media_id,
        &server_id,
    )
    .await
    {
        debug!(id = %media_id, "thumbnail download failed: {e}");
    }

    Ok(())
}

/// Bind or clear the asset's `media.stack_id` pointer based on
/// `AssetV1.stackId`. The authoritative `stacks` row is upserted by
/// `StackHandler` on the `StackV1` stream, not here — `AssetV1` and
/// `StackV1` arrive independently and may interleave in any order.
///
/// FK on `media.stack_id REFERENCES stacks(id)` is enforced
/// (`PRAGMA foreign_keys` is on by default in sqlx 0.8), so we
/// can't bind to a non-existent stack. If the matching `StackV1`
/// hasn't arrived, we create a stub `stacks` row pointing at the
/// current asset as a placeholder primary; `StackV1` later
/// overwrites the primary via `upsert_stack`'s ON CONFLICT clause.
#[instrument(skip(ctx))]
async fn apply_stack_membership(
    stack_id: Option<&str>,
    media_id: &MediaId,
    ctx: &SyncContext,
    now: i64,
) -> Result<(), LibraryError> {
    match stack_id {
        Some(id) => {
            ctx.library
                .media()
                .ensure_stack_stub(id, media_id, now)
                .await?;
            ctx.library.media().set_media_stack_id(media_id, id).await
        }
        None => ctx.library.media().clear_media_stack_id(media_id).await,
    }
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
        let external_id = delete.asset_id.clone();
        // Issue #626: the stream carries the Immich UUID; translate to
        // the local `MediaId` before deleting. Missing row is a no-op —
        // the asset was already gone or never reached us.
        match ctx.library.media().id_by_external_id(&external_id).await? {
            Some(media_id) => {
                ctx.library
                    .delete_permanently_from_sync(std::slice::from_ref(&media_id))
                    .await?;
            }
            None => {
                debug!(external_id = %external_id, "asset delete: no local row, skipping");
            }
        }
        Ok(HandlerResult {
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
    server_id: &str,
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

    let api_path = format!("/assets/{server_id}/thumbnail?size=thumbnail");
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
