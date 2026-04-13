use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tracing::{debug, info, instrument};

use super::super::db::Database;
use super::super::error::LibraryError;
use super::super::immich_client::ImmichClient;
use super::super::media::MediaId;
use super::super::thumbnail::sharded_thumbnail_path;
use crate::app_event::AppEvent;
use crate::event_bus::EventSender;

/// Thumbnail download worker pool.
///
/// Receives `MediaId`s on a bounded channel and downloads thumbnails from Immich
/// with bounded concurrency via a semaphore.
pub(crate) struct ThumbnailDownloader {
    pub client: ImmichClient,
    pub db: Database,
    pub events: EventSender,
    pub thumbnails_dir: PathBuf,
    pub rx: tokio::sync::mpsc::Receiver<MediaId>,
    pub semaphore: Arc<Semaphore>,
}

impl ThumbnailDownloader {
    /// Process thumbnail download requests from the channel.
    ///
    /// Runs until the sender side is dropped (SyncManager finishes or shuts down).
    /// Each download is bounded by the semaphore (max 4 concurrent).
    pub async fn run(mut self) {
        info!("thumbnail downloader started");
        let mut download_count: usize = 0;

        while let Some(media_id) = self.rx.recv().await {
            let permit = match self.semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break, // semaphore closed
            };

            let client = self.client.clone();
            let db = self.db.clone();
            let events = self.events.clone();
            let thumbnails_dir = self.thumbnails_dir.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    download_thumbnail(&client, &db, &events, &thumbnails_dir, &media_id).await
                {
                    debug!(id = %media_id, "thumbnail download failed: {e}");
                }
                drop(permit);
            });

            download_count += 1;

            // Emit progress every 10 thumbnails to update the status bar.
            if download_count.is_multiple_of(10) {
                self.events.send(AppEvent::ThumbnailDownloadProgress {
                    completed: download_count,
                    total: download_count, // Total not known upfront; shows running count.
                });
            }

            if download_count.is_multiple_of(100) {
                info!(queued = download_count, "thumbnail download progress");
            }

            // Throttle to avoid overloading the Immich server during bulk syncs.
            tokio::time::sleep(super::THUMBNAIL_THROTTLE).await;
        }

        self.events.send(AppEvent::ThumbnailDownloadsComplete {
            total: download_count,
        });
        info!(total = download_count, "thumbnail downloader finished");
    }
}

/// Download a single thumbnail from Immich and write it to the local cache.
#[instrument(skip(client, db, events, thumbnails_dir))]
async fn download_thumbnail(
    client: &ImmichClient,
    db: &Database,
    events: &EventSender,
    thumbnails_dir: &std::path::Path,
    media_id: &MediaId,
) -> Result<(), LibraryError> {
    let path = sharded_thumbnail_path(thumbnails_dir, media_id);

    // Skip if already cached on disk.
    if path.exists() {
        debug!("thumbnail already cached, skipping download");
        let now = chrono::Utc::now().timestamp();
        db.set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
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
    db.set_thumbnail_ready(media_id, &path.to_string_lossy(), now)
        .await?;
    events.send(AppEvent::ThumbnailReady {
        // receiver may be dropped during shutdown
        media_id: media_id.clone(),
    });

    Ok(())
}
