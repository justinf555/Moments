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

/// Feature trait for media asset persistence.
///
/// Implemented by every backend that stores media records. Exposes the
/// operations needed by the import pipeline today and will grow to include
/// query methods (e.g. `list_media`, `get_media`) when the photo grid
/// (issue #8) needs them.
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
