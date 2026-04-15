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
    /// Uses two-level sharding: `originals/{hex[0..2]}/{hex[2..4]}/{id}.bin`
    fn cache_path(&self, id: &MediaId) -> PathBuf {
        let hex = id.as_str();
        if hex.len() < 4 {
            return self.originals_dir.join(format!("{hex}.bin"));
        }
        self.originals_dir
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(format!("{hex}.bin"))
    }
}

#[async_trait]
impl OriginalResolver for CachedResolver {
    #[instrument(skip(self), fields(id = %id))]
    async fn resolve(
        &self,
        id: &MediaId,
        _relative_path: &str,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let path = self.cache_path(id);

        // Cache hit — return immediately.
        if path.exists() {
            debug!("original cache hit");
            return Ok(Some(path));
        }

        // Cache miss — fetch from Immich.
        debug!("original cache miss, fetching from server");

        // Use external_id if the ID format is a local UUID; for Immich-synced
        // assets the id IS the Immich UUID so we can use it directly.
        let api_path = format!("/assets/{}/original", id.as_str());
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
