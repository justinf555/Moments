use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::instrument;

use super::error::ImportError;
use crate::library::config::LocalStorageMode;
use crate::library::media::{LibraryMedia, MediaId, MediaRecord, MediaService, MediaType};
use crate::library::metadata::exif::ExifInfo;
use crate::library::metadata::{LibraryMetadata, MediaMetadataRecord, MetadataService};

/// Input parameters for the persist step.
pub struct PersistParams<'a> {
    pub source: &'a Path,
    pub media_id: &'a MediaId,
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
/// In **Managed** mode: copies the file into `originals/YYYY/MM/DD/filename.ext`.
/// In **Referenced** mode: stores the absolute path without copying.
///
/// Inserts both the `MediaRecord` (via MediaService) and the
/// `MediaMetadataRecord` (via MetadataService).
#[instrument(skip_all, fields(media_id = %params.media_id, path = %params.source.display()))]
pub async fn persist(params: PersistParams<'_>) -> Result<PersistResult, ImportError> {
    let PersistParams {
        source,
        media_id,
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
            let base_target = compute_base_target(source, originals_dir, exif.captured_at).await?;
            let target = resolve_collision(base_target);

            let rel = target
                .strip_prefix(originals_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| target.to_string_lossy().into_owned());

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

/// Compute the base destination path (`YYYY/MM/DD/filename.ext`) for `source`
/// inside `originals_dir`.
async fn compute_base_target(
    source: &Path,
    originals_dir: &Path,
    exif_captured_at: Option<i64>,
) -> Result<PathBuf, ImportError> {
    let datetime: chrono::DateTime<chrono::Local> = if let Some(ts) = exif_captured_at {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|utc| utc.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now)
    } else {
        let metadata = tokio::fs::metadata(source).await.map_err(ImportError::Io)?;
        let modified = metadata.modified().map_err(ImportError::Io)?;
        modified.into()
    };

    let date_dir = originals_dir
        .join(datetime.format("%Y").to_string())
        .join(datetime.format("%m").to_string())
        .join(datetime.format("%d").to_string());

    let file_name = source.file_name().ok_or_else(|| {
        ImportError::InvalidSource(format!("source has no filename: {}", source.display()))
    })?;

    Ok(date_dir.join(file_name))
}

/// Resolve filename collisions by appending `_2`, `_3`, … suffixes.
fn resolve_collision(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }

    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = base
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let dir = base.parent().unwrap_or(Path::new(""));

    let mut counter = 2u32;
    loop {
        let candidate = dir.join(format!("{stem}_{counter}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolve_collision_returns_base_when_no_conflict() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("photo.jpg");
        assert_eq!(resolve_collision(base.clone()), base);
    }

    #[test]
    fn resolve_collision_appends_suffix_on_conflict() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("photo.jpg");
        std::fs::write(&base, b"existing").unwrap();

        let resolved = resolve_collision(base);
        assert!(resolved.to_string_lossy().contains("photo_2.jpg"));
    }

    #[test]
    fn resolve_collision_increments_suffix() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("photo.jpg");
        std::fs::write(&base, b"existing").unwrap();
        std::fs::write(dir.path().join("photo_2.jpg"), b"existing").unwrap();

        let resolved = resolve_collision(base);
        assert!(resolved.to_string_lossy().contains("photo_3.jpg"));
    }
}
