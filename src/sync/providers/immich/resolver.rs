//! Cached original resolver for the Immich backend.
//!
//! Checks if the original file is cached locally. On miss, fetches from
//! the Immich server (`GET /assets/{id}/original`) and writes to the
//! local cache before returning the path.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::resolver::OriginalResolver;

use super::client::ImmichClient;

/// Resolves originals by checking the local cache, fetching on miss.
pub struct CachedResolver {
    client: Arc<ImmichClient>,
    originals_dir: PathBuf,
}

impl CachedResolver {
    pub fn new(client: Arc<ImmichClient>, originals_dir: PathBuf) -> Self {
        Self {
            client,
            originals_dir,
        }
    }

    /// Compute the local cache path for an asset.
    ///
    /// Uses two-level sharding: `originals/{hex[0..2]}/{hex[2..4]}/{id}.{ext}`
    fn cache_path(&self, id: &MediaId, ext: &str) -> PathBuf {
        let hex = id.as_str();
        if hex.len() < 4 {
            return self.originals_dir.join(format!("{hex}.{ext}"));
        }
        self.originals_dir
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(format!("{hex}.{ext}"))
    }
}

#[async_trait]
impl OriginalResolver for CachedResolver {
    #[instrument(skip(self), fields(id = %id))]
    async fn resolve(
        &self,
        id: &MediaId,
        _relative_path: &str,
        original_filename: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let ext = original_filename
            .and_then(|f| std::path::Path::new(f).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let path = self.cache_path(id, ext);

        // Cache hit — return immediately.
        if path.exists() {
            debug!("original cache hit");
            return Ok(Some(path));
        }

        // Cache miss — fetch from Immich.
        debug!("original cache miss, fetching from server");

        // Prefer external_id (the Immich server UUID) for the API call.
        // For assets synced from Immich, id == external_id. For locally
        // imported assets, id is a local UUID and external_id is the
        // server-assigned UUID after upload.
        let server_id = external_id.unwrap_or_else(|| id.as_str());
        let api_path = format!("/assets/{server_id}/original");
        let bytes = match self.client.get_bytes(&api_path).await {
            Ok(b) => b,
            Err(e) => {
                debug!("failed to fetch original: {e}");
                return Ok(None);
            }
        };

        // Write to cache.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LibraryError::Io)?;
        }
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(LibraryError::Io)?;

        debug!(size = bytes.len(), "original cached");
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_resolver(originals_dir: PathBuf) -> CachedResolver {
        let client = Arc::new(
            ImmichClient::new("https://test.example.com", "fake-token").unwrap(),
        );
        CachedResolver::new(client, originals_dir)
    }

    #[test]
    fn cache_path_uses_two_level_sharding() {
        let resolver = make_resolver(PathBuf::from("/data/originals"));
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = resolver.cache_path(&id, "jpeg");
        assert_eq!(
            path,
            PathBuf::from("/data/originals/ab/cd/abcdef1234567890.jpeg")
        );
    }

    #[test]
    fn cache_path_short_id_falls_back_to_flat() {
        let resolver = make_resolver(PathBuf::from("/data/originals"));
        let id = MediaId::new("abc".to_string());
        let path = resolver.cache_path(&id, "png");
        assert_eq!(path, PathBuf::from("/data/originals/abc.png"));
    }

    #[test]
    fn cache_path_exactly_four_chars() {
        let resolver = make_resolver(PathBuf::from("/data/originals"));
        let id = MediaId::new("abcd".to_string());
        let path = resolver.cache_path(&id, "jpg");
        assert_eq!(
            path,
            PathBuf::from("/data/originals/ab/cd/abcd.jpg")
        );
    }

    #[test]
    fn cache_path_uuid_format() {
        let resolver = make_resolver(PathBuf::from("/cache"));
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        let path = resolver.cache_path(&id, "heic");
        assert_eq!(
            path,
            PathBuf::from("/cache/55/0e/550e8400-e29b-41d4-a716-446655440000.heic")
        );
    }

    #[tokio::test]
    async fn resolve_returns_cached_file() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = make_resolver(dir.path().to_path_buf());
        let id = MediaId::new("aabbccdd11223344".to_string());

        // Pre-populate cache with correct extension.
        let cache_path = resolver.cache_path(&id, "jpeg");
        tokio::fs::create_dir_all(cache_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&cache_path, b"cached data")
            .await
            .unwrap();

        let result = resolver
            .resolve(&id, "irrelevant/path", Some("photo.jpeg"), None)
            .await
            .unwrap();
        assert_eq!(result, Some(cache_path));
    }

    #[tokio::test]
    async fn resolve_cache_miss_with_unreachable_server_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let client = Arc::new(
            ImmichClient::new("http://127.0.0.1:1", "fake-token").unwrap(),
        );
        let resolver = CachedResolver::new(client, dir.path().to_path_buf());
        let id = MediaId::new("aabbccdd11223344".to_string());

        let result = resolver
            .resolve(&id, "some/path", Some("photo.jpg"), None)
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
