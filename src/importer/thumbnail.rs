//! Thumbnail generation for imported assets.
//!
//! Wraps the render pipeline with I/O and DB bookkeeping:
//! mark pending → render → write to disk → mark ready.
//!
//! On failure the DB row is marked "Failed" — thumbnail failures never
//! abort the import pipeline.

use std::path::Path;
use std::sync::Arc;

use tracing::{debug, instrument, warn};

use super::error::ImportError;
use crate::library::media::MediaId;
use crate::library::thumbnail::{sharded_thumbnail_path, ThumbnailService};
use crate::renderer::output;
use crate::renderer::pipeline::{RenderOptions, RenderPipeline, RenderSize};

/// Longest edge in pixels for the grid thumbnail.
const GRID_SIZE: u32 = 360;

/// Generate a thumbnail for a single imported asset.
#[instrument(skip_all, fields(media_id = %media_id))]
pub async fn generate_thumbnail(
    media_id: &MediaId,
    source: &Path,
    thumbnails_dir: &Path,
    thumbnail_svc: &ThumbnailService,
    pipeline: &Arc<RenderPipeline>,
) {
    if let Err(e) = try_generate(media_id, source, thumbnails_dir, thumbnail_svc, pipeline).await {
        warn!(%media_id, error = %e, "thumbnail generation failed");
        let _ = thumbnail_svc.set_thumbnail_failed(media_id).await;
    }
}

async fn try_generate(
    media_id: &MediaId,
    source: &Path,
    thumbnails_dir: &Path,
    thumbnail_svc: &ThumbnailService,
    pipeline: &Arc<RenderPipeline>,
) -> Result<(), ImportError> {
    // 1. Mark pending in DB.
    thumbnail_svc.insert_thumbnail_pending(media_id).await?;

    // 2. Compute paths.
    let final_path = sharded_thumbnail_path(thumbnails_dir, media_id);
    let tmp_path = thumbnails_dir
        .join("tmp")
        .join(format!("{}.webp", media_id.as_str()));

    if let Some(p) = tmp_path.parent() {
        tokio::fs::create_dir_all(p)
            .await
            .map_err(ImportError::Io)?;
    }
    if let Some(p) = final_path.parent() {
        tokio::fs::create_dir_all(p)
            .await
            .map_err(ImportError::Io)?;
    }

    // 3. Render via pipeline on a blocking thread.
    let source = source.to_path_buf();
    let tmp_clone = tmp_path.clone();
    let pipeline = Arc::clone(pipeline);
    tokio::task::spawn_blocking(move || {
        let options = RenderOptions {
            size: RenderSize::Thumbnail(GRID_SIZE),
            edits: None,
        };
        let img = pipeline.render(&source, &options)?;
        let webp_bytes = output::to_webp(&img)?;
        std::fs::write(&tmp_clone, &webp_bytes)?;
        Ok::<(), ImportError>(())
    })
    .await
    .map_err(|e| ImportError::Runtime(e.to_string()))??;

    // 4. Atomic rename to final path.
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(ImportError::Io)?;

    // 5. Mark ready in DB.
    let relative: String = final_path
        .strip_prefix(thumbnails_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| final_path.to_string_lossy().into_owned());

    let generated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    thumbnail_svc
        .set_thumbnail_ready(media_id, &relative, generated_at)
        .await?;

    debug!(%media_id, "thumbnail ready");
    Ok(())
}
