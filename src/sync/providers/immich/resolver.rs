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
use crate::library::thumbnail::sharded_original_path;

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
}

#[async_trait]
impl OriginalResolver for CachedResolver {
    #[instrument(skip(self), fields(id = %id))]
    async fn resolve(
        &self,
        id: &MediaId,
        _relative_path: &str,
        _original_filename: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let path = sharded_original_path(&self.originals_dir, id);

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
    use crate::library::thumbnail::sharded_original_path;

    fn make_resolver(originals_dir: PathBuf) -> CachedResolver {
        let client = Arc::new(
            ImmichClient::new("https://test.example.com", "fake-token").unwrap(),
        );
        CachedResolver::new(client, originals_dir)
    }

    #[test]
    fn cache_path_uses_two_level_sharding() {
        let dir = PathBuf::from("/data/originals");
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_original_path(&dir, &id);
        assert_eq!(
            path,
            PathBuf::from("/data/originals/ab/cd/abcdef1234567890")
        );
    }

    #[test]
    fn cache_path_short_id_falls_back_to_flat() {
        let dir = PathBuf::from("/data/originals");
        let id = MediaId::new("abc".to_string());
        let path = sharded_original_path(&dir, &id);
        assert_eq!(path, PathBuf::from("/data/originals/abc"));
    }

    #[test]
    fn cache_path_exactly_four_chars() {
        let dir = PathBuf::from("/data/originals");
        let id = MediaId::new("abcd".to_string());
        let path = sharded_original_path(&dir, &id);
        assert_eq!(
            path,
            PathBuf::from("/data/originals/ab/cd/abcd")
        );
    }

    #[test]
    fn cache_path_uuid_format() {
        let dir = PathBuf::from("/cache");
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        let path = sharded_original_path(&dir, &id);
        assert_eq!(
            path,
            PathBuf::from("/cache/55/0e/550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[tokio::test]
    async fn resolve_returns_cached_file() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = make_resolver(dir.path().to_path_buf());
        let id = MediaId::new("aabbccdd11223344".to_string());

        // Pre-populate cache at the extensionless sharded path.
        let cache_path = sharded_original_path(dir.path(), &id);
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
