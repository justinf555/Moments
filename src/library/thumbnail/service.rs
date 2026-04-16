use std::path::PathBuf;

use super::model::ThumbnailStatus;
use super::repository::ThumbnailRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Compute the two-level sharded thumbnail path.
///
/// Extracted as a free function so both the service and internal
/// components (thumbnailer, sync downloader) can use the same logic.
pub fn sharded_thumbnail_path(thumbnails_dir: &std::path::Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    if hex.len() < 4 {
        return thumbnails_dir.join(format!("{hex}.webp"));
    }
    thumbnails_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.webp"))
}

/// Compute the two-level sharded original path (no file extension).
///
/// Decoders detect format from magic bytes, so the extension is not
/// needed. The path is a pure function of `id` — no DB lookup required.
pub fn sharded_original_path(originals_dir: &std::path::Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    if hex.len() < 4 {
        return originals_dir.join(hex);
    }
    originals_dir.join(&hex[..2]).join(&hex[2..4]).join(hex)
}

/// Return the relative portion of a sharded original path (no directory prefix).
///
/// Stored in the `relative_path` DB column. Joining with `originals_dir`
/// gives the absolute path.
pub fn sharded_original_relative(id: &MediaId) -> String {
    let hex = id.as_str();
    if hex.len() < 4 {
        return hex.to_string();
    }
    format!("{}/{}/{hex}", &hex[..2], &hex[2..4])
}

/// Thumbnail path resolution and persistence service.
#[derive(Clone)]
pub struct ThumbnailService {
    repo: ThumbnailRepository,
    thumbnails_dir: PathBuf,
}

impl ThumbnailService {
    pub fn new(db: Database, thumbnails_dir: PathBuf) -> Self {
        Self {
            repo: ThumbnailRepository::new(db),
            thumbnails_dir,
        }
    }

    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        sharded_thumbnail_path(&self.thumbnails_dir, id)
    }

    pub async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.insert_pending(id).await
    }

    pub async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        self.repo.set_ready(id, file_path, generated_at).await
    }

    pub async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.set_failed(id).await
    }

    pub async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        self.repo.status(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sharded_path_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcdef1234567890.webp"));
    }

    #[test]
    fn sharded_path_short_id_no_panic() {
        let id = MediaId::new("ab".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab.webp"));
    }

    #[test]
    fn sharded_path_empty_id_no_panic() {
        let id = MediaId::new(String::new());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/.webp"));
    }

    #[test]
    fn sharded_path_exactly_four_chars() {
        let id = MediaId::new("abcd".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcd.webp"));
    }

    // ── Original path helpers ──────────────────────────────────────

    #[test]
    fn original_path_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(path, Path::new("/originals/ab/cd/abcdef1234567890"));
    }

    #[test]
    fn original_path_uuid_format() {
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(
            path,
            Path::new("/originals/55/0e/550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn original_path_short_id() {
        let id = MediaId::new("ab".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(path, Path::new("/originals/ab"));
    }

    #[test]
    fn original_relative_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        assert_eq!(sharded_original_relative(&id), "ab/cd/abcdef1234567890");
    }

    #[test]
    fn original_relative_uuid_format() {
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        assert_eq!(
            sharded_original_relative(&id),
            "55/0e/550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn original_relative_short_id() {
        let id = MediaId::new("ab".to_string());
        assert_eq!(sharded_original_relative(&id), "ab");
    }
}
