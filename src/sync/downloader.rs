//! Thumbnail download worker pool.
//!
//! Receives [`MediaId`]s on a bounded channel and downloads thumbnails
//! from the Immich server with bounded concurrency via a semaphore.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tracing::{debug, info, instrument};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::thumbnail::sharded_thumbnail_path;
use crate::library::Library;

use super::client::ImmichClient;

/// Thumbnail download worker pool.
pub(crate) struct ThumbnailDownloader {
    pub client: ImmichClient,
    pub library: Arc<Library>,
    pub events: EventSender,
    pub thumbnails_dir: PathBuf,
    pub rx: tokio::sync::mpsc::Receiver<MediaId>,
    pub semaphore: Arc<Semaphore>,
}

impl ThumbnailDownloader {
    /// Process thumbnail download requests from the channel.
    ///
    /// Runs until the sender side is dropped. Each download is bounded
    /// by the semaphore (max concurrent set by caller).
    pub async fn run(mut self) {
        info!("thumbnail downloader started");
        let mut download_count: usize = 0;

        while let Some(media_id) = self.rx.recv().await {
            let permit = match self.semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };

            let client = self.client.clone();
            let library = Arc::clone(&self.library);
            let events = self.events.clone();
            let thumbnails_dir = self.thumbnails_dir.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    download_thumbnail(&client, &library, &events, &thumbnails_dir, &media_id)
                        .await
                {
                    debug!(id = %media_id, "thumbnail download failed: {e}");
                }
                drop(permit);
            });

            download_count += 1;

            if download_count % 10 == 0 {
                self.events.send(AppEvent::ThumbnailDownloadProgress {
                    completed: download_count,
                    total: download_count,
                });
            }

            if download_count % 100 == 0 {
                info!(queued = download_count, "thumbnail download progress");
            }

            // Throttle to avoid overloading the Immich server.
            tokio::time::sleep(super::THUMBNAIL_THROTTLE).await;
        }

        self.events.send(AppEvent::ThumbnailDownloadsComplete {
            total: download_count,
        });
        info!(total = download_count, "thumbnail downloader finished");
    }
}

/// Download a single thumbnail from Immich and write it to the local cache.
#[instrument(skip(client, library, events, thumbnails_dir))]
async fn download_thumbnail(
    client: &ImmichClient,
    library: &Library,
    events: &EventSender,
    thumbnails_dir: &std::path::Path,
    media_id: &MediaId,
) -> Result<(), LibraryError> {
    let path = sharded_thumbnail_path(thumbnails_dir, media_id);

    // Skip if already cached on disk.
    if path.exists() {
        debug!("thumbnail already cached, skipping download");
        let now = chrono::Utc::now().timestamp();
        library
            .thumbnails()
            .set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
            .await?;
        events.send(AppEvent::ThumbnailReady {
            media_id: media_id.clone(),
        });
        return Ok(());
    }

    // Download from Immich.
    let api_path = format!("/assets/{}/thumbnail?size=thumbnail", media_id.as_str());
    let bytes = client.get_bytes(&api_path).await?;

    // Create shard directories and write file.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(LibraryError::Io)?;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(LibraryError::Io)?;

    // Update DB status and emit event.
    let now = chrono::Utc::now().timestamp();
    library
        .thumbnails()
        .set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
        .await?;
    events.send(AppEvent::ThumbnailReady {
        media_id: media_id.clone(),
    });

    Ok(())
}
