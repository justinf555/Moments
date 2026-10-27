// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached original resolver for the Immich backend.
//!
//! Checks if the original file is cached locally. On miss, fetches from
//! the Immich server (`GET /assets/{id}/original`) and writes to the
//! local cache before returning the path. After each download the cache
//! is trimmed back under `originals-cache-max-mb` (see [`cache`]).

pub mod cache;
pub mod guard;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, instrument, warn};

use crate::library::error::LibraryError;
use crate::library::media::{sharded_original_path, MediaId};
use crate::library::resolver::{OriginalPins, OriginalResolver};

use super::client::ImmichClient;
use cache::{EvictionGuard, EvictionOutcome, OriginalsCache, PART_SUFFIX};

/// Makes temp-file names unique across concurrent downloads.
static DOWNLOAD_SEQ: AtomicU64 = AtomicU64::new(0);

/// Resolves originals by checking the local cache, fetching on miss.
pub struct CachedResolver {
    client: Arc<ImmichClient>,
    cache: Arc<OriginalsCache>,
}

impl CachedResolver {
    /// `limit_bytes` is shared with the settings watcher so a changed
    /// limit applies to the next pass; `pins` is shared with the clients
    /// that hold originals open.
    pub fn new(
        client: Arc<ImmichClient>,
        originals_dir: PathBuf,
        limit_bytes: Arc<AtomicU64>,
        pins: Arc<OriginalPins>,
    ) -> Self {
        Self {
            client,
            cache: Arc::new(OriginalsCache::new(originals_dir, limit_bytes, pins)),
        }
    }

    /// Enable eviction. Until this is called the cache only grows, so
    /// nothing is deleted before the library can answer the guard.
    pub fn set_eviction_guard(&self, guard: Arc<dyn EvictionGuard>) {
        self.cache.set_guard(guard);
    }

    /// Trim the cache to the current limit now (startup, limit changed).
    pub async fn enforce_limit(&self) -> Result<EvictionOutcome, LibraryError> {
        self.cache.enforce_limit().await
    }

    /// Bytes currently used by the originals directory (for #712).
    pub async fn cache_usage_bytes(&self) -> Result<u64, LibraryError> {
        self.cache.usage_bytes().await
    }

    /// Download `server_id`'s original to `path` via a temp file, so a
    /// crash or failed write never leaves a truncated "cache hit".
    /// Returns `false` when the server couldn't provide it.
    async fn download(&self, server_id: &str, path: &Path) -> Result<bool, LibraryError> {
        let api_path = format!("/assets/{server_id}/original");
        let bytes = match self.client.get_bytes(&api_path).await {
            Ok(b) => b,
            Err(e) => {
                debug!("failed to fetch original: {e}");
                return Ok(false);
            }
        };

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LibraryError::Io)?;
        }
        let seq = DOWNLOAD_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{seq}{PART_SUFFIX}"));
        let tmp = PathBuf::from(tmp);

        let written = match tokio::fs::write(&tmp, &bytes).await {
            Ok(()) => tokio::fs::rename(&tmp, path).await,
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(LibraryError::Io(e));
        }
        debug!(size = bytes.len(), "original cached");
        Ok(true)
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
        let path = sharded_original_path(self.cache.dir(), id);
        // Pin while resolving so an eviction pass can't delete the file
        // between the existence check (or download) and returning it.
        let pin = self.cache.pins().pin(id);

        // Cache hit — mark as recently used and return.
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            debug!("original cache hit");
            cache::touch(path.clone()).await;
            return Ok(Some(path));
        }

        // Cache miss — fetch from Immich.
        debug!("original cache miss, fetching from server");

        // Prefer external_id (the Immich server UUID) for the API call.
        // For assets synced from Immich, id == external_id. For locally
        // imported assets, id is a local UUID and external_id is the
        // server-assigned UUID after upload.
        let server_id = external_id.unwrap_or_else(|| id.as_str());
        if !self.download(server_id, &path).await? {
            return Ok(None);
        }

        // Trim the cache in the background. The new file stays pinned
        // until the pass is done, so it survives even a tiny limit.
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            let _pin = pin;
            if let Err(e) = cache.enforce_limit().await {
                warn!("originals cache eviction failed: {e}");
            }
        });

        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::media::sharded_original_path;

    fn make_resolver_with(server: &str, originals_dir: PathBuf) -> CachedResolver {
        let client = Arc::new(ImmichClient::new(server, "fake-token").unwrap());
        CachedResolver::new(
            client,
            originals_dir,
            Arc::new(AtomicU64::new(u64::MAX)),
            Arc::new(OriginalPins::default()),
        )
    }

    fn make_resolver(originals_dir: PathBuf) -> CachedResolver {
        make_resolver_with("https://test.example.com", originals_dir)
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
        assert_eq!(path, PathBuf::from("/data/originals/ab/cd/abcd"));
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
        tokio::fs::write(&cache_path, b"cached data").await.unwrap();

        let result = resolver
            .resolve(&id, "irrelevant/path", Some("photo.jpeg"), None)
            .await
            .unwrap();
        assert_eq!(result, Some(cache_path));
    }

    #[tokio::test]
    async fn resolve_hit_refreshes_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = make_resolver(dir.path().to_path_buf());
        let id = MediaId::new("aabbccdd11223344".to_string());

        let cache_path = sharded_original_path(dir.path(), &id);
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(&cache_path, b"cached data").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&cache_path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        resolver.resolve(&id, "", None, None).await.unwrap();

        let mtime = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
        assert!(mtime > old + std::time::Duration::from_secs(1800));
    }

    #[tokio::test]
    async fn resolve_cache_miss_with_unreachable_server_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = make_resolver_with("http://127.0.0.1:1", dir.path().to_path_buf());
        let id = MediaId::new("aabbccdd11223344".to_string());

        let result = resolver
            .resolve(&id, "some/path", Some("photo.jpg"), None)
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(resolver.cache_usage_bytes().await.unwrap(), 0);
    }
}
