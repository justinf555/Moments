//! Bidirectional sync engine.
//!
//! Backend-agnostic orchestration lives here (`SyncEngine`, `outbox/`).
//! Provider-specific protocol code lives under `providers/`.
//!
//! Start with [`SyncEngine::build`], which spawns two background tasks:
//! pull manager and push manager. Thumbnails are downloaded inline by
//! the pull manager.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tracing::{error, info, instrument};

use crate::library::db::Database;
use crate::library::Library;

pub mod event;
pub mod outbox;
pub mod providers;
pub mod state;

/// Long-running Immich sync service.
///
/// Owns the pull / push background tasks and the channels used to
/// signal shutdown and live interval changes. Constructed via
/// [`SyncEngine::build`] and held as `Arc<SyncEngine>` on
/// `MomentsApplication`; [`crate::client::SyncClient`] also holds an
/// `Arc<SyncEngine>` reference for UI-driven control (interval
/// changes, future "Sync Now" actions, …).
///
/// Lifecycle:
/// - `build` spawns the pull and push managers and returns the
///   `Arc<SyncEngine>` immediately. Tasks run until `shutdown` is
///   called or the shutdown watch channel is dropped.
/// - [`SyncEngine::shutdown`] signals graceful stop by flipping the
///   shutdown watch channel. Idempotent — safe to call multiple times.
/// - [`SyncEngine::set_interval`] forwards a new polling interval to
///   the running tasks.
#[derive(Debug)]
pub struct SyncEngine {
    shutdown_tx: watch::Sender<bool>,
    interval_tx: watch::Sender<u64>,
}

impl SyncEngine {
    /// Build the bidirectional sync engine and spawn its background
    /// tasks on the supplied Tokio runtime.
    ///
    /// Spawns two Tokio tasks:
    /// - **PullManager**: streams changes from Immich, upserts locally,
    ///   downloads thumbnails inline.
    /// - **PushManager**: drains the outbox, pushes local mutations to
    ///   Immich.
    ///
    /// Returns an `Arc<SyncEngine>` so the application and the paired
    /// [`crate::client::SyncClient`] can share ownership.
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all)]
    pub fn build(
        client: providers::immich::client::ImmichClient,
        library: Arc<Library>,
        db: Database,
        sync_events: mpsc::UnboundedSender<event::SyncEvent>,
        thumbnails_dir: PathBuf,
        initial_interval_secs: u64,
        tokio: tokio::runtime::Handle,
    ) -> Arc<Self> {
        use providers::immich::{pull, push};

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (interval_tx, interval_rx) = watch::channel(initial_interval_secs);

        // Spawn pull manager (thumbnails downloaded inline per asset).
        let pull_mgr = pull::PullManager {
            client: client.clone(),
            library: Arc::clone(&library),
            state: state::SyncStateRepository::new(db.clone()),
            db: db.clone(),
            sync_events: sync_events.clone(),
            shutdown_rx: shutdown_rx.clone(),
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
            sync_events,
            shutdown_rx,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
            moments_edit_tag: tokio::sync::Mutex::new(None),
        };
        tokio.spawn(async move {
            if let Err(e) = push_mgr.run().await {
                error!("push manager exited with error: {e}");
            }
        });

        info!("sync engine started");
        Arc::new(Self {
            shutdown_tx,
            interval_tx,
        })
    }

    /// Signal all sync tasks to shut down gracefully.
    ///
    /// Idempotent: calling twice is a no-op. The spawned tasks observe
    /// the shutdown flag at their next polling boundary and exit their
    /// loops; this method returns immediately and does not wait for
    /// the tasks to drain.
    #[instrument(skip_all)]
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        info!("sync engine shutdown requested");
    }

    /// Update the sync polling interval (seconds). Takes effect on
    /// the next cycle of the pull / push managers.
    #[instrument(skip(self))]
    pub fn set_interval(&self, secs: u64) {
        let _ = self.interval_tx.send(secs);
        info!(secs, "sync interval updated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_engine() -> SyncEngine {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (interval_tx, _interval_rx) = watch::channel(60u64);
        SyncEngine {
            shutdown_tx,
            interval_tx,
        }
    }

    #[test]
    fn immich_constants_are_sensible() {
        const { assert!(providers::immich::ACK_FLUSH_THRESHOLD > 0) };
    }

    #[test]
    fn sync_engine_shutdown_does_not_panic() {
        let engine = make_engine();
        engine.shutdown();
        // Calling shutdown again is also safe.
        engine.shutdown();
    }

    #[test]
    fn sync_engine_set_interval() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (interval_tx, interval_rx) = watch::channel(60u64);
        let engine = SyncEngine {
            shutdown_tx,
            interval_tx,
        };

        engine.set_interval(120);
        assert_eq!(*interval_rx.borrow(), 120);

        engine.set_interval(0);
        assert_eq!(*interval_rx.borrow(), 0);
    }

    #[test]
    fn sync_engine_shutdown_sets_flag() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (interval_tx, _interval_rx) = watch::channel(60u64);
        let engine = SyncEngine {
            shutdown_tx,
            interval_tx,
        };

        assert!(!*shutdown_rx.borrow());
        engine.shutdown();
        assert!(*shutdown_rx.borrow());
    }
}
