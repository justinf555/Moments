//! Bidirectional Immich sync engine.
//!
//! - **Pull**: Immich → Moments via `/sync/stream`
//! - **Push**: Moments → Immich via outbox pattern
//!
//! Start with [`SyncHandle::start`], which spawns three background tasks:
//! pull manager, push manager, and thumbnail downloader.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, Semaphore};
use tracing::{error, info};

use crate::event_bus::EventSender;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::Library;

pub(crate) mod client;
pub(crate) mod downloader;
pub mod outbox;
pub(crate) mod pull;
pub(crate) mod push;
pub(crate) mod types;

/// Maximum concurrent thumbnail downloads.
const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Bounded channel capacity for thumbnail download requests.
const THUMBNAIL_QUEUE_SIZE: usize = 1000;
/// Delay between dispatching thumbnail downloads to avoid server overload.
const THUMBNAIL_THROTTLE: Duration = Duration::from_millis(5);
/// Flush acks to the database after this many processed entities.
const ACK_FLUSH_THRESHOLD: usize = 500;

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
        server_url: &str,
        access_token: &str,
        library: Arc<Library>,
        db: Database,
        events: EventSender,
        thumbnails_dir: PathBuf,
        initial_interval_secs: u64,
    ) -> Result<Self, LibraryError> {
        let client = client::ImmichClient::new(server_url, access_token)?;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (interval_tx, interval_rx) = watch::channel(initial_interval_secs);
        let (thumbnail_tx, thumbnail_rx) = mpsc::channel(THUMBNAIL_QUEUE_SIZE);

        // Spawn thumbnail downloader.
        let downloader = downloader::ThumbnailDownloader {
            client: client.clone(),
            library: Arc::clone(&library),
            events: events.clone(),
            thumbnails_dir: thumbnails_dir.clone(),
            rx: thumbnail_rx,
            semaphore: Arc::new(Semaphore::new(MAX_THUMBNAIL_WORKERS)),
        };
        tokio::spawn(async move {
            downloader.run().await;
        });

        // Spawn pull manager.
        let pull = pull::PullManager {
            client: client.clone(),
            library: Arc::clone(&library),
            db: db.clone(),
            events: events.clone(),
            shutdown_rx: shutdown_rx.clone(),
            thumbnail_tx,
            thumbnails_dir,
            interval_rx: tokio::sync::Mutex::new(interval_rx.clone()),
        };
        tokio::spawn(async move {
            if let Err(e) = pull.run().await {
                error!("pull manager exited with error: {e}");
            }
        });

        // Spawn push manager.
        let push = push::PushManager {
            client,
            library,
            db,
            shutdown_rx,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        };
        tokio::spawn(async move {
            if let Err(e) = push.run().await {
                error!("push manager exited with error: {e}");
            }
        });

        info!("sync engine started");
        Ok(Self {
            shutdown_tx,
            interval_tx,
        })
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
