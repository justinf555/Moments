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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::thumbnail::sharded_thumbnail_path;

    #[test]
    fn sharded_path_uses_webp_extension() {
        let dir = std::path::PathBuf::from("/thumbnails");
        let id = MediaId::new("aabbccdd11223344".to_string());
        let path = sharded_thumbnail_path(&dir, &id);
        assert!(path.to_str().unwrap().ends_with(".webp"));
    }

    #[test]
    fn sharded_path_creates_two_level_shard() {
        let dir = std::path::PathBuf::from("/thumbnails");
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_thumbnail_path(&dir, &id);
        assert_eq!(
            path,
            std::path::PathBuf::from("/thumbnails/ab/cd/abcdef1234567890.webp")
        );
    }

    #[test]
    fn sharded_path_short_id_flat() {
        let dir = std::path::PathBuf::from("/thumbnails");
        let id = MediaId::new("abc".to_string());
        let path = sharded_thumbnail_path(&dir, &id);
        assert_eq!(path, std::path::PathBuf::from("/thumbnails/abc.webp"));
    }

    #[tokio::test]
    async fn downloader_finishes_when_channel_closed() {
        // When the sender is dropped, the downloader should finish.
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let client = ImmichClient::new("http://127.0.0.1:1", "fake").unwrap();

        // We need a Library to construct the downloader. Create a minimal one.
        let dir = tempfile::tempdir().unwrap();
        let bundle = crate::library::bundle::Bundle::create(
            &dir.path().join("Test.library"),
            &crate::library::config::LibraryConfig::Local {
                mode: crate::library::config::LocalStorageMode::Managed,
            },
        )
        .unwrap();
        let library = Arc::new(
            crate::library::Library::open(
                bundle,
                crate::library::config::LocalStorageMode::Managed,
                crate::library::db::Database::new(),
                Arc::new(crate::sync::outbox::NoOpRecorder),
                Arc::new(crate::library::resolver::LocalResolver::new(
                    std::path::PathBuf::new(),
                    crate::library::config::LocalStorageMode::Managed,
                )),
            )
            .await
            .unwrap(),
        );

        let events = EventSender::no_op();

        let downloader = ThumbnailDownloader {
            client,
            library,
            events,
            thumbnails_dir: dir.path().to_path_buf(),
            rx,
            semaphore: Arc::new(Semaphore::new(2)),
        };

        // Drop the sender — the downloader's loop should terminate.
        drop(tx);

        // Run with a timeout to ensure it doesn't hang.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            downloader.run(),
        )
        .await;
        assert!(result.is_ok());
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
