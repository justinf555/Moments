use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{debug, instrument, warn};

use super::db::Database;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::format::FormatRegistry;
use super::media::MediaId;
use super::thumbnail::sharded_thumbnail_path;

/// Longest edge in pixels for the grid thumbnail.
const GRID_SIZE: u32 = 360;

/// Drives thumbnail generation for a single imported asset.
///
/// `ThumbnailJob::generate` is **async** and must be spawned on the Tokio
/// runtime. The CPU-bound decode/resize/encode step runs on a blocking
/// thread via [`tokio::task::spawn_blocking`] so the async executor stays
/// free. Results flow back through [`LibraryEvent::ThumbnailReady`].
pub struct ThumbnailJob {
    thumbnails_dir: PathBuf,
    db: Database,
    events: std::sync::mpsc::Sender<LibraryEvent>,
    formats: Arc<FormatRegistry>,
}

impl ThumbnailJob {
    pub fn new(
        thumbnails_dir: PathBuf,
        db: Database,
        events: std::sync::mpsc::Sender<LibraryEvent>,
        formats: Arc<FormatRegistry>,
    ) -> Self {
        Self {
            thumbnails_dir,
            db,
            events,
            formats,
        }
    }

    /// Generate and persist the grid thumbnail for `source`.
    ///
    /// 1. Insert a `Pending` DB row (idempotent).
    /// 2. Decode the source image on a blocking thread.
    /// 3. Resize to [`GRID_SIZE`] on the longest edge, preserving aspect ratio.
    /// 4. Encode as WebP and write atomically (temp file → rename).
    /// 5. Mark the DB row `Ready` and emit [`LibraryEvent::ThumbnailReady`].
    ///
    /// On any failure the DB row is marked `Failed` and the error is logged
    /// but not propagated — a thumbnail failure must not abort an import.
    #[instrument(skip(self), fields(media_id = %media_id))]
    pub async fn generate(self, media_id: MediaId, source: PathBuf) {
        if let Err(e) = self.try_generate(&media_id, &source).await {
            warn!(%media_id, error = %e, "thumbnail generation failed");
            let _ = self.db.set_thumbnail_failed(&media_id).await;
        }
    }

    async fn try_generate(&self, media_id: &MediaId, source: &Path) -> Result<(), LibraryError> {
        // ── 1. Mark pending ───────────────────────────────────────────────────
        self.db.insert_thumbnail_pending(media_id).await?;

        // ── 2. Compute paths ──────────────────────────────────────────────────
        let final_path = sharded_thumbnail_path(&self.thumbnails_dir, media_id);
        let tmp_path = self
            .thumbnails_dir
            .join("tmp")
            .join(format!("{}.webp", media_id.as_str()));

        if let Some(p) = tmp_path.parent() {
            tokio::fs::create_dir_all(p)
                .await
                .map_err(LibraryError::Io)?;
        }
        if let Some(p) = final_path.parent() {
            tokio::fs::create_dir_all(p)
                .await
                .map_err(LibraryError::Io)?;
        }

        // ── 3. Decode, resize, encode — blocking ──────────────────────────────
        let source = source.to_path_buf();
        let tmp_clone = tmp_path.clone();
        let formats = Arc::clone(&self.formats);
        tokio::task::spawn_blocking(move || {
            generate_thumbnail(&source, &tmp_clone, GRID_SIZE, &formats)
        })
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        // ── 4. Atomic rename to final path ────────────────────────────────────
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(LibraryError::Io)?;

        // ── 5. Update DB and emit event ───────────────────────────────────────
        let relative = final_path
            .strip_prefix(&self.thumbnails_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| final_path.to_string_lossy().into_owned());

        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.db
            .set_thumbnail_ready(media_id, &relative, generated_at)
            .await?;

        debug!(%media_id, "thumbnail ready");
        self.events
            .send(LibraryEvent::ThumbnailReady {
                media_id: media_id.clone(),
            })
            .ok();

        Ok(())
    }
}

/// Decode `source`, resize to `max_edge` on the longest side, encode as WebP.
///
/// Applies EXIF orientation before resizing so thumbnails are always upright.
/// Runs on a blocking thread — never call from an async context directly.
fn generate_thumbnail(
    source: &Path,
    dest: &Path,
    max_edge: u32,
    formats: &FormatRegistry,
) -> Result<(), LibraryError> {
    let img = formats.decode(source)?;

    // Apply EXIF orientation for images only — videos don't have EXIF.
    let img = if formats.is_video(
        &source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default(),
    ) {
        img
    } else {
        let orientation = crate::library::exif::extract_exif(source)
            .orientation
            .unwrap_or(1);
        apply_orientation(img, orientation)
    };
    let thumb = img.thumbnail(max_edge, max_edge);
    thumb
        .save_with_format(dest, image::ImageFormat::WebP)
        .map_err(|e| LibraryError::Thumbnail(e.to_string()))?;
    Ok(())
}

/// Rotate/flip `img` to match the EXIF orientation tag value (1–8).
///
/// EXIF orientation defines how the sensor data maps to the upright image.
/// Value 1 means the pixel data is already correct; values 2–8 require a
/// combination of rotation and/or mirror to produce a visually upright image.
pub(crate) fn apply_orientation(img: image::DynamicImage, orientation: u8) -> image::DynamicImage {
    use image::imageops;
    match orientation {
        2 => image::DynamicImage::from(imageops::flip_horizontal(&img)),
        3 => image::DynamicImage::from(imageops::rotate180(&img)),
        4 => image::DynamicImage::from(imageops::flip_vertical(&img)),
        5 => image::DynamicImage::from(imageops::flip_horizontal(&imageops::rotate90(&img))),
        6 => image::DynamicImage::from(imageops::rotate90(&img)),
        7 => image::DynamicImage::from(imageops::flip_horizontal(&imageops::rotate270(&img))),
        8 => image::DynamicImage::from(imageops::rotate270(&img)),
        _ => img, // 1 or unknown — already upright
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::Database;
    use crate::library::format::StandardHandler;
    use crate::library::media::{LibraryMedia, MediaRecord, MediaType};
    use std::sync::mpsc;
    use tempfile::tempdir;

    async fn open_test_db(dir: &Path) -> Database {
        Database::open(&dir.join("db").join("test.db"))
            .await
            .unwrap()
    }

    fn test_record(id: MediaId, filename: &str) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: format!("2025/01/01/{filename}"),
            original_filename: filename.to_string(),
            file_size: 100,
            imported_at: 0,
            media_type: MediaType::Image,
            taken_at: None,
            width: None,
            height: None,
            orientation: 1,
        }
    }

    fn test_registry() -> Arc<FormatRegistry> {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(StandardHandler));
        Arc::new(reg)
    }

    fn write_test_jpeg(path: &Path) {
        // Minimal valid 1×1 white JPEG.
        let img = image::RgbImage::new(1, 1);
        image::DynamicImage::ImageRgb8(img)
            .save_with_format(path, image::ImageFormat::Jpeg)
            .unwrap();
    }

    #[tokio::test]
    async fn generate_creates_webp_thumbnail() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let thumbnails_dir = dir.path().join("thumbnails");
        let src_path = dir.path().join("photo.jpg");
        write_test_jpeg(&src_path);

        let id = MediaId::from_file(&src_path).await.unwrap();

        // Insert media record so FK constraint is satisfied.
        db.insert_media(&test_record(id.clone(), "photo.jpg"))
            .await
            .unwrap();

        let (tx, rx) = mpsc::channel();
        ThumbnailJob::new(thumbnails_dir.clone(), db.clone(), tx, test_registry())
            .generate(id.clone(), src_path)
            .await;

        // Thumbnail file exists.
        let thumb_path = sharded_thumbnail_path(&thumbnails_dir, &id);
        assert!(thumb_path.exists(), "thumbnail file not found at {thumb_path:?}");

        // Event was emitted.
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events
            .iter()
            .any(|e| matches!(e, LibraryEvent::ThumbnailReady { .. })));
    }

    #[test]
    fn apply_orientation_1_is_identity() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 1);
        assert_eq!((out.width(), out.height()), (4, 2));
    }

    #[test]
    fn apply_orientation_6_swaps_dimensions() {
        // Orientation 6 = rotate 90° CW: a 4×2 image becomes 2×4.
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 6);
        assert_eq!((out.width(), out.height()), (2, 4));
    }

    #[test]
    fn apply_orientation_8_swaps_dimensions() {
        // Orientation 8 = rotate 90° CCW: a 4×2 image becomes 2×4.
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 8);
        assert_eq!((out.width(), out.height()), (2, 4));
    }

    #[test]
    fn apply_orientation_3_preserves_dimensions() {
        // Orientation 3 = rotate 180°: dimensions stay the same.
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let out = apply_orientation(img, 3);
        assert_eq!((out.width(), out.height()), (4, 2));
    }

    #[tokio::test]
    async fn generate_marks_failed_on_corrupt_source() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let thumbnails_dir = dir.path().join("thumbnails");
        let src_path = dir.path().join("bad.jpg");
        std::fs::write(&src_path, b"not an image").unwrap();

        let id = MediaId::from_file(&src_path).await.unwrap();

        db.insert_media(&test_record(id.clone(), "bad.jpg"))
            .await
            .unwrap();

        let (tx, _rx) = mpsc::channel();
        ThumbnailJob::new(thumbnails_dir, db.clone(), tx, test_registry())
            .generate(id.clone(), src_path)
            .await;

        // DB row should be marked Failed.
        let status = db.thumbnail_status(&id).await.unwrap();
        assert_eq!(status, Some(crate::library::thumbnail::ThumbnailStatus::Failed));
    }
}
