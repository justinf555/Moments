use std::path::Path;

use async_trait::async_trait;
use tracing::instrument;

use super::error::LibraryError;

/// Content-addressable identity for every media asset in the library.
///
/// The value is the lowercase hex-encoded BLAKE3 hash of the file's raw bytes.
/// This is stable across renames and re-imports of the same content, and serves
/// as the primary key in the `media` database table and the thumbnail filename.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaId(String);

impl MediaId {
    /// Hash `path` and return its [`MediaId`].
    ///
    /// Uses [`tokio::task::spawn_blocking`] with a streaming [`blake3::Hasher`]
    /// so that large video files are never fully loaded into memory.
    #[instrument(skip_all, fields(path = %path.display()))]
    pub async fn from_file(path: &Path) -> Result<Self, LibraryError> {
        let path = path.to_path_buf();
        let hex = tokio::task::spawn_blocking(move || -> Result<String, LibraryError> {
            let file = std::fs::File::open(&path).map_err(LibraryError::Io)?;
            let mut reader = std::io::BufReader::new(file);
            let mut hasher = blake3::Hasher::new();
            std::io::copy(&mut reader, &mut hasher).map_err(LibraryError::Io)?;
            Ok(hasher.finalize().to_hex().to_string())
        })
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        Ok(Self(hex))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct a `MediaId` from a pre-computed hex string.
    ///
    /// Use this inside a `spawn_blocking` closure where hashing is done
    /// manually with `blake3::Hasher`. For general use, prefer [`MediaId::from_file`].
    pub(crate) fn new(hex: String) -> Self {
        Self(hex)
    }

    /// For use in tests only — constructs a `MediaId` from a raw string
    /// without hashing. Prefixed `__test_` to make its purpose obvious.
    #[cfg(test)]
    pub fn __test_new(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Media type stored in the `media.media_type` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum MediaType {
    Image = 0,
    Video = 1,
}

/// The read model returned by [`LibraryMedia::list_media`].
///
/// Contains everything the photo grid needs to display a cell — id for
/// thumbnail lookup, dimensions for aspect-ratio placeholder, and capture
/// time for grouping. Full EXIF detail is in `media_metadata` and is
/// fetched separately when the detail view needs it.
#[derive(Debug, Clone)]
pub struct MediaItem {
    pub id: MediaId,
    /// EXIF capture timestamp (UTC Unix seconds). `None` if unavailable.
    pub taken_at: Option<i64>,
    pub imported_at: i64,
    pub original_filename: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// EXIF orientation tag (1–8).
    pub orientation: u8,
    pub media_type: MediaType,
    pub is_favorite: bool,
    pub is_trashed: bool,
    /// Unix timestamp when the item was trashed. `None` if not trashed.
    pub trashed_at: Option<i64>,
    /// Video duration in milliseconds. `None` for images.
    pub duration_ms: Option<u64>,
}

/// Filter for [`LibraryMedia::list_media`] queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaFilter {
    /// All media items (excludes trashed).
    #[default]
    All,
    /// Only items marked as favourite (excludes trashed).
    Favorites,
    /// Only trashed items.
    Trashed,
    /// Items imported after `since` (Unix timestamp). Excludes trashed.
    /// Sorted by `imported_at DESC` instead of `taken_at`.
    RecentImports { since: i64 },
}

/// Opaque cursor for keyset pagination in [`LibraryMedia::list_media`].
///
/// Encodes the position of the last item seen so the next page continues
/// exactly where the previous one left off — no `OFFSET` scans.
///
/// `sort_key` is `COALESCE(taken_at, 0)` so items without EXIF dates
/// sort to the end of the timeline.
#[derive(Debug, Clone)]
pub struct MediaCursor {
    /// `COALESCE(taken_at, 0)` of the last seen item.
    pub sort_key: i64,
    /// `id` of the last seen item — tiebreaker within the same timestamp.
    pub id: MediaId,
}

/// Feature trait for media asset persistence.
///
/// Implemented by every backend that stores media records.
///
/// `Database` implements this trait with the SQL logic. `LocalLibrary`
/// delegates to its `Database`. The GTK layer calls these methods through
/// the `Library` supertrait — it never touches `Database` directly.
#[async_trait]
pub trait LibraryMedia: Send + Sync {
    /// Return `true` if an asset with this [`MediaId`] is already stored.
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError>;

    /// Persist a newly imported media asset record.
    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError>;

    /// Persist the full EXIF detail row. No-op if `record.has_data()` is false.
    async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError>;

    /// Return a page of [`MediaItem`]s in reverse chronological order.
    ///
    /// Pass `cursor: None` for the first page. Pass the cursor from a previous
    /// result to fetch the next page. Returns an empty `Vec` when exhausted.
    ///
    /// Items without a `taken_at` date sort to the end (treated as timestamp 0).
    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError>;

    /// Fetch the full EXIF metadata record for `id`.
    ///
    /// Returns `None` if no metadata row was stored (e.g. the asset has no EXIF
    /// data, or metadata extraction failed silently at import time).
    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError>;

    /// Set or clear the favourite flag on one or more assets.
    async fn set_favorite(
        &self,
        ids: &[MediaId],
        favorite: bool,
    ) -> Result<(), LibraryError>;

    /// Move assets to the trash (soft delete).
    ///
    /// Sets `is_trashed = 1` and records the current timestamp in `trashed_at`.
    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Restore trashed assets back to the library.
    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Permanently delete assets: removes the DB row, original file, and thumbnail.
    ///
    /// This is irreversible. Callers should confirm with the user before calling.
    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError>;

    /// Return IDs of items trashed longer than `max_age_secs` ago.
    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError>;
}

/// A row in the `media` table.
#[derive(Debug, Clone)]
pub struct MediaRecord {
    pub id: MediaId,
    /// Path relative to the bundle's `originals/` directory.
    pub relative_path: String,
    pub original_filename: String,
    pub file_size: i64,
    /// Unix timestamp (seconds since epoch).
    pub imported_at: i64,
    pub media_type: MediaType,
    /// Capture timestamp from EXIF (UTC Unix seconds). `None` if unavailable.
    pub taken_at: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// EXIF orientation tag (1–8). Defaults to 1 (normal).
    pub orientation: u8,
    /// Video duration in milliseconds. `None` for images.
    pub duration_ms: Option<u64>,
}

/// A row in the `media_metadata` table — full EXIF detail, loaded on demand.
#[derive(Debug, Clone)]
pub struct MediaMetadataRecord {
    pub media_id: MediaId,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub aperture: Option<f32>,
    pub shutter_str: Option<String>,
    pub iso: Option<u32>,
    pub focal_length: Option<f32>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub gps_alt: Option<f64>,
    pub color_space: Option<String>,
}

impl MediaMetadataRecord {
    /// Returns `true` if at least one field is populated.
    ///
    /// Used to skip inserting an empty row for assets with no EXIF metadata.
    pub fn has_data(&self) -> bool {
        self.camera_make.is_some()
            || self.camera_model.is_some()
            || self.lens_model.is_some()
            || self.aperture.is_some()
            || self.shutter_str.is_some()
            || self.iso.is_some()
            || self.focal_length.is_some()
            || self.gps_lat.is_some()
            || self.gps_lon.is_some()
            || self.gps_alt.is_some()
            || self.color_space.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn same_content_produces_same_id() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"hello moments").unwrap();
        f2.write_all(b"hello moments").unwrap();

        let id1 = MediaId::from_file(f1.path()).await.unwrap();
        let id2 = MediaId::from_file(f2.path()).await.unwrap();
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn different_content_produces_different_id() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"photo a").unwrap();
        f2.write_all(b"photo b").unwrap();

        let id1 = MediaId::from_file(f1.path()).await.unwrap();
        let id2 = MediaId::from_file(f2.path()).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn id_is_64_char_hex() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"test").unwrap();
        let id = MediaId::from_file(f.path()).await.unwrap();
        assert_eq!(id.as_str().len(), 64);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
