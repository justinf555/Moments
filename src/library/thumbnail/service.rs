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

/// Thumbnail path resolution and persistence service.
#[derive(Clone)]
pub struct ThumbnailService {
    pub(crate) repo: ThumbnailRepository,
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
}
