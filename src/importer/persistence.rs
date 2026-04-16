use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::instrument;

use super::error::ImportError;
use crate::library::config::LocalStorageMode;
use crate::library::media::{MediaId, MediaRecord, MediaService, MediaType};
use crate::library::metadata::exif::ExifInfo;
use crate::library::metadata::{MediaMetadataRecord, MetadataService};
use crate::library::thumbnail::sharded_original_relative;

/// Input parameters for the persist step.
pub struct PersistParams<'a> {
    pub source: &'a Path,
    pub media_id: &'a MediaId,
    pub content_hash: Option<&'a str>,
    pub media_type: MediaType,
    pub exif: ExifInfo,
    pub duration_ms: Option<u64>,
    pub originals_dir: &'a Path,
    pub mode: &'a LocalStorageMode,
    pub media: &'a MediaService,
    pub metadata: &'a MetadataService,
}

/// Result of persisting a single file to the library.
pub struct PersistResult {
    /// Path to the file to use as thumbnail source.
    pub thumbnail_source: PathBuf,
}

/// Persist a file and its records to the library.
///
/// In **Managed** mode: copies the file into a UUID-sharded path under
/// `originals/{id[0..2]}/{id[2..4]}/{id}` (no extension — decoders
/// detect format from magic bytes).
/// In **Referenced** mode: stores the absolute path without copying.
///
/// Inserts both the `MediaRecord` (via MediaService) and the
/// `MediaMetadataRecord` (via MetadataService).
#[instrument(skip_all, fields(media_id = %params.media_id, path = %params.source.display()))]
pub async fn persist(params: PersistParams<'_>) -> Result<PersistResult, ImportError> {
    let PersistParams {
        source,
        media_id,
        content_hash,
        media_type,
        exif,
        duration_ms,
        originals_dir,
        mode,
        media,
        metadata,
    } = params;

    // ── Store or copy file ────────────────────────────────────────────
    let (relative_path, file_size, thumbnail_source) = match mode {
        LocalStorageMode::Managed => {
            let rel = sharded_original_relative(media_id);
            let target = originals_dir.join(&rel);

            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(ImportError::Io)?;
            }
            let size = tokio::fs::copy(source, &target)
                .await
                .map_err(ImportError::Io)? as i64;

            (rel, size, target)
        }
        LocalStorageMode::Referenced => {
            let abs = source.to_string_lossy().into_owned();
            let size = tokio::fs::metadata(source)
                .await
                .map_err(ImportError::Io)?
                .len() as i64;

            (abs, size, source.to_path_buf())
        }
    };

    // ── Persist media record ──────────────────────────────────────────
    let original_filename = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let imported_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    media
        .insert_media(&MediaRecord {
            id: media_id.clone(),
            content_hash: content_hash.map(|s| s.to_string()),
            external_id: None,
            relative_path,
            original_filename,
            file_size,
            imported_at,
            media_type,
            taken_at: exif.captured_at,
            width: exif.width.map(|w| w as i64),
            height: exif.height.map(|h| h as i64),
            orientation: exif.orientation.unwrap_or(1),
            duration_ms,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        })
        .await?;

    // ── Persist metadata record ───────────────────────────────────────
    metadata
        .insert_media_metadata(&MediaMetadataRecord {
            media_id: media_id.clone(),
            camera_make: exif.camera_make,
            camera_model: exif.camera_model,
            lens_model: exif.lens_model,
            aperture: exif.aperture,
            shutter_str: exif.shutter_str,
            iso: exif.iso,
            focal_length: exif.focal_length,
            gps_lat: exif.gps_lat,
            gps_lon: exif.gps_lon,
            gps_alt: exif.gps_alt,
            color_space: exif.color_space,
        })
        .await?;

    Ok(PersistResult { thumbnail_source })
}
