use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gtk::gio;
use tracing::{debug, instrument, warn};

use super::db::Database;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::media::MediaId;
use super::thumbnail::sharded_thumbnail_path;

/// Longest edge in pixels for the grid thumbnail.
const GRID_SIZE: u32 = 360;

/// Drives thumbnail generation for a single imported asset.
///
/// `ThumbnailJob::generate` is **async** and must be spawned on the Tokio
/// runtime. The CPU-bound resize/encode step runs on a blocking thread via
/// [`tokio::task::spawn_blocking`]. Image decoding is handled by glycin
/// (sandboxed, handles JPEG/PNG/WebP/TIFF/HEIC/RAW/etc.) on the Tokio
/// executor. Results flow back through [`LibraryEvent::ThumbnailReady`].
pub struct ThumbnailJob {
    thumbnails_dir: PathBuf,
    db: Database,
    events: std::sync::mpsc::Sender<LibraryEvent>,
}

impl ThumbnailJob {
    pub fn new(
        thumbnails_dir: PathBuf,
        db: Database,
        events: std::sync::mpsc::Sender<LibraryEvent>,
    ) -> Self {
        Self {
            thumbnails_dir,
            db,
            events,
        }
    }

    /// Generate and persist the grid thumbnail for `source`.
    ///
    /// 1. Insert a `Pending` DB row (idempotent).
    /// 2. Decode the source image with glycin on the Tokio executor.
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

        // ── 3. Decode with glycin (async, Tokio executor) ─────────────────────
        // glycin applies EXIF orientation automatically — no manual correction needed.
        let file = gio::File::for_path(source);
        let img = glycin::Loader::new(file)
            .load()
            .await
            .map_err(|e| LibraryError::Thumbnail(e.to_string()))?;
        let frame = img
            .next_frame()
            .await
            .map_err(|e| LibraryError::Thumbnail(e.to_string()))?;

        let raw_bytes = frame.buf_slice().to_vec();
        let width = frame.width();
        let height = frame.height();
        let stride = frame.stride();
        let memory_format = frame.memory_format();

        // ── 4. Resize and encode as WebP — blocking ───────────────────────────
        let tmp_clone = tmp_path.clone();
        tokio::task::spawn_blocking(move || {
            let dyn_img = frame_bytes_to_image(raw_bytes, width, height, stride, memory_format)?;
            let thumb = dyn_img.thumbnail(GRID_SIZE, GRID_SIZE);
            thumb
                .save_with_format(&tmp_clone, image::ImageFormat::WebP)
                .map_err(|e| LibraryError::Thumbnail(e.to_string()))
        })
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        // ── 5. Atomic rename to final path ────────────────────────────────────
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(LibraryError::Io)?;

        // ── 6. Update DB and emit event ───────────────────────────────────────
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

/// Convert raw pixel bytes from a glycin frame into an [`image::DynamicImage`].
///
/// Handles non-tight stride by copying each row. Supports the RGB and RGBA
/// 8-bit formats that glycin returns for standard and RAW images.
fn frame_bytes_to_image(
    data: Vec<u8>,
    width: u32,
    height: u32,
    stride: u32,
    fmt: glycin::MemoryFormat,
) -> Result<image::DynamicImage, LibraryError> {
    let (bytes_per_pixel, has_alpha) = match fmt {
        glycin::MemoryFormat::R8g8b8 => (3u32, false),
        glycin::MemoryFormat::R8g8b8a8 | glycin::MemoryFormat::R8g8b8a8Premultiplied => {
            (4u32, true)
        }
        other => {
            return Err(LibraryError::Thumbnail(format!(
                "unsupported pixel format from glycin: {other:?}"
            )))
        }
    };

    let row_bytes = (width * bytes_per_pixel) as usize;
    let packed: Vec<u8> = if stride as usize == row_bytes {
        data
    } else {
        let mut out = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let start = row * stride as usize;
            out.extend_from_slice(&data[start..start + row_bytes]);
        }
        out
    };

    let img = if has_alpha {
        let buf = image::RgbaImage::from_raw(width, height, packed)
            .ok_or_else(|| LibraryError::Thumbnail("failed to build RGBA image".into()))?;
        image::DynamicImage::ImageRgba8(buf)
    } else {
        let buf = image::RgbImage::from_raw(width, height, packed)
            .ok_or_else(|| LibraryError::Thumbnail("failed to build RGB image".into()))?;
        image::DynamicImage::ImageRgb8(buf)
    };

    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bytes_to_image_rgb_tight() {
        // 2×2 RGB image, tightly packed (stride == width * 3)
        let data = vec![255u8; 2 * 2 * 3];
        let img = frame_bytes_to_image(data, 2, 2, 6, glycin::MemoryFormat::R8g8b8).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
    }

    #[test]
    fn frame_bytes_to_image_rgba_tight() {
        let data = vec![255u8; 4 * 4 * 4];
        let img =
            frame_bytes_to_image(data, 4, 4, 16, glycin::MemoryFormat::R8g8b8a8).unwrap();
        assert_eq!((img.width(), img.height()), (4, 4));
    }

    #[test]
    fn frame_bytes_to_image_rgb_with_stride_padding() {
        // 2×2 RGB, stride = 8 (2 bytes padding per row)
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[255u8, 0, 0]); // pixel 1
            data.extend_from_slice(&[0u8, 255, 0]); // pixel 2
            data.extend_from_slice(&[0u8, 0]);       // 2 bytes padding
        }
        let img = frame_bytes_to_image(data, 2, 2, 8, glycin::MemoryFormat::R8g8b8).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
    }

    #[test]
    fn frame_bytes_to_image_unsupported_format_returns_error() {
        let data = vec![0u8; 4];
        let result = frame_bytes_to_image(data, 1, 1, 2, glycin::MemoryFormat::G8);
        assert!(result.is_err());
    }
}
