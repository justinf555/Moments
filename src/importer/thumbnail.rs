use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{debug, instrument, warn};

use super::error::ImportError;
use crate::library::format::FormatRegistry;
use crate::library::media::MediaId;
use crate::library::metadata::exif::extract_exif;
use crate::library::thumbnail::{sharded_thumbnail_path, ThumbnailService};

/// Longest edge in pixels for the grid thumbnail.
const GRID_SIZE: u32 = 360;

/// Generate a thumbnail for a single imported asset (inline pipeline step).
///
/// 1. Mark the DB row as "Pending".
/// 2. Decode the source image on a blocking thread.
/// 3. Resize to [`GRID_SIZE`] on the longest edge, preserving aspect ratio.
/// 4. Apply EXIF orientation (except videos and HEIC/HEIF).
/// 5. Encode as WebP and write atomically (temp file → rename).
/// 6. Mark the DB row as "Ready" and emit [`AppEvent::ThumbnailReady`].
///
/// On failure the DB row is marked "Failed" — thumbnail failures never
/// abort the import pipeline.
#[instrument(skip_all, fields(media_id = %media_id))]
pub async fn generate_thumbnail(
    media_id: &MediaId,
    source: &Path,
    thumbnails_dir: &Path,
    thumbnail_svc: &ThumbnailService,
    formats: &Arc<FormatRegistry>,
) {
    if let Err(e) = try_generate(media_id, source, thumbnails_dir, thumbnail_svc, formats).await {
        warn!(%media_id, error = %e, "thumbnail generation failed");
        let _ = thumbnail_svc.set_thumbnail_failed(media_id).await;
    }
}

async fn try_generate(
    media_id: &MediaId,
    source: &Path,
    thumbnails_dir: &Path,
    thumbnail_svc: &ThumbnailService,
    formats: &Arc<FormatRegistry>,
) -> Result<(), ImportError> {
    // ── 1. Mark pending ───────────────────────────────────────────────
    thumbnail_svc.insert_thumbnail_pending(media_id).await?;

    // ── 2. Compute paths ──────────────────────────────────────────────
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

    // ── 3. Decode, resize, orient, encode — blocking ──────────────────
    let source = source.to_path_buf();
    let tmp_clone = tmp_path.clone();
    let formats = Arc::clone(formats);
    tokio::task::spawn_blocking(move || encode_thumbnail(&source, &tmp_clone, GRID_SIZE, &formats))
        .await
        .map_err(|e| ImportError::Runtime(e.to_string()))??;

    // ── 4. Atomic rename to final path ────────────────────────────────
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(ImportError::Io)?;

    // ── 5. Update DB ──────────────────────────────────────────────────
    let relative: String = final_path
        .strip_prefix(thumbnails_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| final_path.to_string_lossy().into_owned());

    let generated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    thumbnail_svc
        .set_thumbnail_ready(media_id, &relative, generated_at)
        .await?;

    debug!(%media_id, "thumbnail ready");
    Ok(())
}

/// Decode `source`, apply orientation, resize to `max_edge`, encode as WebP.
///
/// Runs on a blocking thread — never call from an async context directly.
fn encode_thumbnail(
    source: &Path,
    dest: &Path,
    max_edge: u32,
    formats: &FormatRegistry,
) -> Result<(), ImportError> {
    let img = formats.decode(source)?;

    // Apply EXIF orientation for standard image formats only.
    // Skip for: videos (no EXIF), HEIC/HEIF (libheif applies orientation
    // automatically during decode — applying it again would double-rotate).
    let skip_orientation = formats.is_video_by_magic(source) || formats.is_heif_by_magic(source);
    let img = if skip_orientation {
        img
    } else {
        let orientation = extract_exif(source).orientation.unwrap_or(1);
        crate::renderer::orientation::apply_orientation(img, orientation)
    };

    let thumb = img.thumbnail(max_edge, max_edge);
    thumb
        .save_with_format(dest, image::ImageFormat::WebP)
        .map_err(|e| ImportError::Thumbnail(e.to_string()))?;
    Ok(())
}

// apply_orientation moved to crate::renderer::orientation
