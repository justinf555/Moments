// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{instrument, warn};

use super::error::ImportError;
use crate::library::config::LocalStorageMode;
use crate::library::media::sharded_original_relative;
use crate::library::media::{MediaId, MediaRecord, MediaService, MediaType};
use crate::library::metadata::exif::ExifInfo;
use crate::library::metadata::{MediaMetadataRecord, MetadataService};

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

    // The offset is stored beside `taken_at`, never applied to it (#549),
    // and written in the same insert so the #677 backfill skips this row.
    let taken_at_offset_minutes = exif.captured_at_tz;
    let inserted = media
        .insert_media_with_taken_at_offset(
            &MediaRecord {
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
                is_moments_render: false,
            },
            taken_at_offset_minutes,
        )
        .await;
    if let Err(e) = inserted {
        // The insert (and its outbox row) rolled back, so nothing points
        // at the managed copy: remove it rather than leave an orphan in
        // `originals/`. Referenced originals belong to the user — never
        // touch them.
        if matches!(mode, LocalStorageMode::Managed) {
            remove_orphan_copy(&thumbnail_source).await;
        }
        return Err(e.into());
    }

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

/// Best-effort removal of a managed copy whose media row was never
/// committed. A missing file is not an error.
async fn remove_orphan_copy(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            path = %path.display(),
            error = %e,
            "could not remove managed copy after failed insert"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::open_test_db;
    use crate::library::recorder::tests::FailingRecorder;
    use crate::library::resolver::LocalResolver;
    use std::sync::Arc;

    /// Run `persist` for one source file against a service whose
    /// recorder fails, so the media insert rolls back.
    async fn persist_with_failing_insert(mode: LocalStorageMode) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();
        let source = dir.path().join("photo.jpg");
        std::fs::write(&source, b"jpeg bytes").unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaService::new(
            db.clone(),
            originals.clone(),
            mode.clone(),
            Arc::new(FailingRecorder::always(db.clone())),
            Arc::new(LocalResolver::new(originals.clone(), mode.clone())),
        );
        let metadata = MetadataService::new(db);
        let media_id = MediaId::generate();

        let result = persist(PersistParams {
            source: &source,
            media_id: &media_id,
            content_hash: None,
            media_type: MediaType::Image,
            exif: ExifInfo::default(),
            duration_ms: None,
            originals_dir: &originals,
            mode: &mode,
            media: &media,
            metadata: &metadata,
        })
        .await;
        assert!(matches!(result, Err(ImportError::Library(_))));
        let copy = originals.join(sharded_original_relative(&media_id));
        (dir, copy)
    }

    #[tokio::test]
    async fn failed_insert_removes_managed_copy() {
        let (_dir, copy) = persist_with_failing_insert(LocalStorageMode::Managed).await;
        assert!(!copy.exists(), "orphaned managed copy must be removed");
    }

    #[tokio::test]
    async fn failed_insert_keeps_referenced_original() {
        let (dir, _copy) = persist_with_failing_insert(LocalStorageMode::Referenced).await;
        assert!(
            dir.path().join("photo.jpg").exists(),
            "a referenced original belongs to the user"
        );
    }
}
