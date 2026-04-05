mod downloader;
mod manager;
mod types;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tracing::error;

use super::db::Database;
use super::event::LibraryEvent;
use super::immich_client::ImmichClient;
use super::media::MediaId;

use downloader::ThumbnailDownloader;
use manager::SyncManager;

/// Maximum concurrent thumbnail downloads.
const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Bounded channel capacity for thumbnail download queue.
const THUMBNAIL_QUEUE_SIZE: usize = 1000;
/// Delay between thumbnail download dispatches to avoid overloading the server.
const THUMBNAIL_THROTTLE: std::time::Duration = std::time::Duration::from_millis(5);
/// Number of acks to accumulate before flushing to server.
const ACK_FLUSH_THRESHOLD: usize = 500;

#[derive(Default)]
pub(crate) struct SyncCounters {
    pub assets: usize,
    pub exifs: usize,
    pub deletes: usize,
    pub people: usize,
    pub faces: usize,
    pub albums: usize,
    pub errors: usize,
}

/// Handle returned by [`SyncHandle::start`] to signal shutdown.
pub struct SyncHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    interval_tx: tokio::sync::watch::Sender<u64>,
}

impl SyncHandle {
    /// Spawn the sync manager and thumbnail downloader as background Tokio tasks.
    ///
    /// `initial_interval_secs` is the polling interval read from GSettings.
    /// Use [`set_interval`] to update it live from the preferences dialog.
    pub fn start(
        client: ImmichClient,
        db: Database,
        events: Sender<LibraryEvent>,
        thumbnails_dir: PathBuf,
        tokio: tokio::runtime::Handle,
        initial_interval_secs: u64,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (interval_tx, interval_rx) = tokio::sync::watch::channel(initial_interval_secs);
        let (thumb_tx, thumb_rx) = tokio::sync::mpsc::channel::<MediaId>(THUMBNAIL_QUEUE_SIZE);

        // Spawn the thumbnail downloader.
        let manager_thumbnails_dir = thumbnails_dir.clone();
        let downloader = ThumbnailDownloader {
            client: client.clone(),
            db: db.clone(),
            events: events.clone(),
            thumbnails_dir,
            rx: thumb_rx,
            semaphore: Arc::new(Semaphore::new(MAX_THUMBNAIL_WORKERS)),
        };
        tokio.spawn(async move {
            downloader.run().await;
        });

        // Spawn the sync manager.
        let manager = SyncManager {
            client,
            db,
            events,
            shutdown_rx,
            thumbnail_tx: thumb_tx,
            thumbnails_dir: manager_thumbnails_dir,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        };

        tokio.spawn(async move {
            if let Err(e) = manager.run().await {
                error!("sync manager error: {e}");
                let _ = manager.events.send(LibraryEvent::Error(e));
            }
        });

        Self { shutdown_tx, interval_tx }
    }

    /// Signal the sync manager to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Update the polling interval (seconds). Takes effect on the next cycle.
    /// Set to 0 to disable polling (sync on open only).
    pub fn set_interval(&self, secs: u64) {
        let _ = self.interval_tx.send(secs);
    }
}
