use std::path::Path;

use tracing::instrument;

use super::error::ImportError;
use crate::library::media::MediaId;

/// Compute the BLAKE3 content hash of a file and return it as a [`MediaId`].
///
/// Runs on a blocking thread via [`tokio::task::spawn_blocking`] so the
/// async executor stays free during the streaming hash. Large video files
/// are never fully loaded into memory.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn hash_file(path: &Path) -> Result<MediaId, ImportError> {
    let path = path.to_path_buf();
    let hex = tokio::task::spawn_blocking(move || -> Result<String, ImportError> {
        let file = std::fs::File::open(&path).map_err(ImportError::Io)?;
        let mut reader = std::io::BufReader::new(file);
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut reader, &mut hasher).map_err(ImportError::Io)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|e| ImportError::Runtime(e.to_string()))??;

    Ok(MediaId::new(hex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn same_content_produces_same_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"identical content").unwrap();
        f2.write_all(b"identical content").unwrap();

        let id1 = hash_file(f1.path()).await.unwrap();
        let id2 = hash_file(f2.path()).await.unwrap();
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn different_content_produces_different_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"content a").unwrap();
        f2.write_all(b"content b").unwrap();

        let id1 = hash_file(f1.path()).await.unwrap();
        let id2 = hash_file(f2.path()).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn hash_is_64_char_hex() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"test").unwrap();
        let id = hash_file(f.path()).await.unwrap();
        assert_eq!(id.as_str().len(), 64);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let result = hash_file(Path::new("/nonexistent/file.jpg")).await;
        assert!(result.is_err());
    }
}
