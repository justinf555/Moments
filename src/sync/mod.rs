//! Bidirectional sync engine.
//!
//! Backend-agnostic orchestration lives here (`SyncHandle`, `outbox/`).
//! Provider-specific protocol code lives under `providers/`.
//!
//! Start with [`SyncHandle::start`], which spawns three background tasks:
//! pull manager, push manager, and thumbnail downloader.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch, Semaphore};
use tracing::{error, info};

use crate::event_bus::EventSender;
use crate::library::db::Database;
use crate::library::Library;

pub mod outbox;
pub mod providers;

/// Handle to the running sync engine.
///
/// Provides shutdown and live interval update. Drop to abandon tasks
/// (use [`shutdown`](Self::shutdown) for graceful stop).
pub struct SyncHandle {
    shutdown_tx: watch::Sender<bool>,
    interval_tx: watch::Sender<u64>,
}

impl SyncHandle {
    /// Start the bidirectional sync engine.
    ///
    /// Spawns three Tokio tasks:
    /// - **PullManager**: streams changes from Immich, upserts locally
    /// - **PushManager**: drains the outbox, pushes local mutations to Immich
    /// - **ThumbnailDownloader**: bounded worker pool for thumbnail fetching
    ///
    /// Returns a handle for shutdown and interval control.
    pub fn start(
        client: providers::immich::client::ImmichClient,
        library: Arc<Library>,
        db: Database,
        events: EventSender,
        thumbnails_dir: PathBuf,
        initial_interval_secs: u64,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        use providers::immich::{
            downloader, pull, push, MAX_THUMBNAIL_WORKERS, THUMBNAIL_QUEUE_SIZE,
        };

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (interval_tx, interval_rx) = watch::channel(initial_interval_secs);
        let (thumbnail_tx, thumbnail_rx) = mpsc::channel(THUMBNAIL_QUEUE_SIZE);

        // Spawn thumbnail downloader.
        let dl = downloader::ThumbnailDownloader {
            client: client.clone(),
            library: Arc::clone(&library),
            events: events.clone(),
            thumbnails_dir: thumbnails_dir.clone(),
            rx: thumbnail_rx,
            semaphore: Arc::new(Semaphore::new(MAX_THUMBNAIL_WORKERS)),
        };
        tokio.spawn(async move {
            dl.run().await;
        });

        // Spawn pull manager.
        let pull_mgr = pull::PullManager {
            client: client.clone(),
            library: Arc::clone(&library),
            db: db.clone(),
            events: events.clone(),
            shutdown_rx: shutdown_rx.clone(),
            thumbnail_tx,
            thumbnails_dir,
            interval_rx: tokio::sync::Mutex::new(interval_rx.clone()),
        };
        tokio.spawn(async move {
            if let Err(e) = pull_mgr.run().await {
                error!("pull manager exited with error: {e}");
            }
        });

        // Spawn push manager.
        let push_mgr = push::PushManager {
            client,
            db,
            shutdown_rx,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        };
        tokio.spawn(async move {
            if let Err(e) = push_mgr.run().await {
                error!("push manager exited with error: {e}");
            }
        });

        info!("sync engine started");
        Self {
            shutdown_tx,
            interval_tx,
        }
    }

    /// Signal all sync tasks to shut down gracefully.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        info!("sync engine shutdown requested");
    }

    /// Update the sync polling interval (seconds). Takes effect next cycle.
    pub fn set_interval(&self, secs: u64) {
        let _ = self.interval_tx.send(secs);
        info!(secs, "sync interval updated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_sensible() {
        use providers::immich::*;
        assert!(MAX_THUMBNAIL_WORKERS > 0);
        assert!(THUMBNAIL_QUEUE_SIZE > 0);
        assert!(ACK_FLUSH_THRESHOLD > 0);
        assert!(THUMBNAIL_THROTTLE.as_millis() > 0);
    }

    #[test]
    fn sync_handle_shutdown_does_not_panic() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (interval_tx, _interval_rx) = watch::channel(60u64);
        let handle = SyncHandle {
            shutdown_tx,
            interval_tx,
        };
        handle.shutdown();
        // Calling shutdown again is also safe.
        handle.shutdown();
    }

    #[test]
    fn sync_handle_set_interval() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (interval_tx, interval_rx) = watch::channel(60u64);
        let handle = SyncHandle {
            shutdown_tx,
            interval_tx,
        };

        handle.set_interval(120);
        assert_eq!(*interval_rx.borrow(), 120);

        handle.set_interval(0);
        assert_eq!(*interval_rx.borrow(), 0);
    }

    #[test]
    fn sync_handle_shutdown_sets_flag() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (interval_tx, _interval_rx) = watch::channel(60u64);
        let handle = SyncHandle {
            shutdown_tx,
            interval_tx,
        };

        assert!(!*shutdown_rx.borrow());
        handle.shutdown();
        assert!(*shutdown_rx.borrow());
    }
}
