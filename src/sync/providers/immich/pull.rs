// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pull sync manager — streams changes from Immich and upserts locally.
//!
//! Connects to `POST /sync/stream`, processes NDJSON entity records,
//! and flushes acks incrementally. See `docs/design-immich-backend.md`.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::TryStreamExt;
use tokio::io::AsyncBufReadExt;
use tracing::{debug, error, info, instrument, warn};

use crate::library::error::LibraryError;
use crate::library::Library;
use crate::sync::event::SyncEvent;
use crate::sync::state::SyncStateRepository;

use super::client::ImmichClient;
use super::handlers::{self, CounterKind, SyncContext};
use super::types::*;
use super::ACK_FLUSH_THRESHOLD;

/// Entity streams requested from `POST /sync/stream`, in the order we
/// declare them (the server applies its own `SYNC_TYPES_ORDER`).
///
/// Issue #679: the `AssetsV1` and `AssetFacesV1` request types were
/// retired server-side. Immich's handlers for them now throw a 400
/// outright rather than degrading, which fails the entire stream before
/// a single entity is emitted — so a deprecated entry here takes down
/// all of sync, not just its own entity type.
///
/// Each entry's entity types must have a registered handler in
/// [`handlers::all_handlers`]; `requested_types_have_handlers` pins that
/// pairing, because an unhandled entity type is silently ignored by the
/// dispatch loop rather than raised as an error.
const SYNC_REQUEST_TYPES: &[&str] = &[
    "AssetsV2",
    "AssetExifsV1",
    "AlbumsV1",
    "AlbumToAssetsV1",
    "PeopleV1",
    "AssetFacesV2",
    // Issue #224: stacks arrive on their own stream as SyncStackV1 /
    // SyncStackDeleteV1. AssetV2 carries only `stackId`; the primary
    // lives on StackV1.
    "StacksV1",
    // Issue #224 Phase B: per-action geometric edit records. Each
    // SyncAssetEditV1 carries one (action, parameters, sequence) tuple;
    // the handler caches them and recomposes a local EditState from the
    // asset's full action list.
    "AssetEditsV1",
];

/// Counters for a single sync cycle.
#[derive(Default)]
struct SyncCounters {
    assets: usize,
    exifs: usize,
    deletes: usize,
    albums: usize,
    people: usize,
    faces: usize,
    errors: usize,
}

impl SyncCounters {
    fn increment(&mut self, kind: CounterKind) {
        match kind {
            CounterKind::Assets => self.assets += 1,
            CounterKind::Exifs => self.exifs += 1,
            CounterKind::Deletes => self.deletes += 1,
            CounterKind::Albums => self.albums += 1,
            CounterKind::People => self.people += 1,
            CounterKind::Faces => self.faces += 1,
            CounterKind::None => {}
        }
    }
}

/// Background pull sync engine for the Immich backend.
pub(crate) struct PullManager {
    pub client: ImmichClient,
    pub library: Arc<Library>,
    /// Sync-engine state (ack checkpoints + per-line audit log).
    pub state: SyncStateRepository,
    /// Direct DB handle for handlers that maintain provider-specific
    /// bookkeeping tables (e.g. `immich_asset_edits`).
    pub db: crate::library::db::Database,
    /// Channel for UI state updates (sync progress, errors).
    pub sync_events: tokio::sync::mpsc::UnboundedSender<SyncEvent>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    pub thumbnails_dir: PathBuf,
    pub interval_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl PullManager {
    /// Main sync loop. Runs an initial sync, then polls at the configured
    /// interval. The interval can be updated live via the watch channel.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<(), LibraryError> {
        info!("pull manager starting");

        // Cumulative asset count across continuous batches, reset when
        // the stream is exhausted and we go to sleep.
        let mut cumulative_assets: usize = 0;
        let mut cumulative_errors: usize = 0;

        loop {
            if *self.shutdown_rx.borrow() {
                info!("pull manager shutting down");
                break;
            }

            let had_items = match self.run_sync().await {
                Ok((total, assets)) => {
                    cumulative_assets += assets;
                    total > 0
                }
                Err(e) => {
                    error!("sync cycle failed: {e}");
                    cumulative_errors += 1;
                    let sync_event = if crate::sync::event::is_connectivity_error(&e) {
                        SyncEvent::Offline
                    } else {
                        SyncEvent::Error {
                            message: e.to_string(),
                        }
                    };
                    let _ = self.sync_events.send(sync_event);
                    false
                }
            };

            // If items were processed, immediately loop back for the
            // next batch — don't wait for the polling interval. This
            // makes initial sync of large libraries continuous.
            if had_items {
                debug!("items processed, continuing immediately");
                continue;
            }

            // Stream exhausted — emit Complete with cumulative counts.
            let _ = self.sync_events.send(SyncEvent::Complete {
                items: cumulative_assets,
                errors: cumulative_errors,
            });
            cumulative_assets = 0;
            cumulative_errors = 0;

            let interval_secs: u64 = {
                let mut rx = self.interval_rx.lock().await;
                let val = *rx.borrow_and_update();
                val
            };
            if interval_secs == 0 {
                info!("sync polling disabled (interval=0), stopping after initial sync");
                break;
            }

            let interval = std::time::Duration::from_secs(interval_secs);
            debug!(interval_secs, "waiting for next sync cycle");

            let mut shutdown = self.shutdown_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = shutdown.changed() => {
                    info!("pull manager shutting down during sleep");
                    break;
                }
            }
        }

        info!("pull manager stopped");
        Ok(())
    }

    /// Execute a single sync cycle. Returns `(total_entities, assets)`.
    #[instrument(skip(self))]
    async fn run_sync(&self) -> Result<(usize, usize), LibraryError> {
        let request = SyncStreamRequest {
            types: SYNC_REQUEST_TYPES
                .iter()
                .map(|t| (*t).to_string())
                .collect(),
        };

        debug!("starting sync stream");
        let response = self.client.post_stream("/sync/stream", &request).await?;

        let byte_stream = response.bytes_stream().map_err(std::io::Error::other);
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(byte_stream));

        let mut lines = reader.lines();
        let mut acks: Vec<String> = Vec::new();
        let mut counters = SyncCounters::default();
        let mut notified_processing = false;
        let mut line_number: usize = 0;
        let sync_cycle = chrono::Utc::now().to_rfc3339();

        // Issue #628: heartbeat-based reset reconciliation. The
        // checkpoint is `Some(unix_seconds)` while a reset cycle is
        // open; rows whose `last_seen_at` lags it (and have a non-null
        // external_id, for media/albums) are deleted at end-of-stream.
        //
        // Seed from the audit log so a stream that resumes mid-reset
        // — server picked up from last ack rather than re-sending
        // SyncResetV1 — still finishes the reconciliation correctly.
        let mut reset_checkpoint_at: Option<i64> = self.state.current_reset_checkpoint().await?;
        if reset_checkpoint_at.is_some() {
            info!(
                checkpoint = ?reset_checkpoint_at,
                "resuming reset reconciliation from prior cycle"
            );
        }

        let entity_handlers = handlers::all_handlers();
        let ctx = SyncContext {
            client: self.client.clone(),
            library: Arc::clone(&self.library),
            state: self.state.clone(),
            thumbnails_dir: self.thumbnails_dir.clone(),
            db: self.db.clone(),
        };

        info!("reading sync stream");

        while let Some(line) = lines.next_line().await.map_err(|e| {
            LibraryError::Immich(format!(
                "failed to read sync stream line {line_number}: {e}"
            ))
        })? {
            line_number += 1;
            if line.is_empty() {
                continue;
            }

            let sync_line: SyncLine = serde_json::from_str(&line).map_err(|e| {
                error!(
                    line_number,
                    line = %line.chars().take(200).collect::<String>(),
                    "failed to parse sync line"
                );
                LibraryError::Immich(format!("failed to parse sync line {line_number}: {e}"))
            })?;

            let entity_type = sync_line.entity_type.as_str();

            // ── Reset tracking ──────────────────────────────────────
            // Issue #628: SyncResetV1 enters heartbeat-reconcile mode.
            // The checkpoint is wall time at receipt — assets the
            // stream re-emits will bump their `last_seen_at` past it,
            // and anything left below the line at SyncCompleteV1 is
            // an orphan. No HashSet snapshot needed.
            if entity_type == "SyncResetV1" {
                warn!("server requested sync reset — performing full resync");
                reset_checkpoint_at = Some(chrono::Utc::now().timestamp());
            }

            // ── Dispatch to handler ─────────────────────────────────
            if let Some(handler) = entity_handlers
                .iter()
                .find(|h| h.entity_type() == entity_type)
            {
                let audit_id = self
                    .state
                    .start_audit(entity_type, "", &sync_cycle)
                    .await
                    .ok();

                match handler.handle(&sync_line.data, line_number, &ctx).await {
                    Ok(result) => {
                        if let Some(aid) = audit_id {
                            let _ = self.state.complete_audit(aid, result.audit_action).await;
                        }
                        acks.push(sync_line.ack);
                        counters.increment(result.counter);

                        // Notify UI when the first asset is processed so the
                        // spinner shows immediately, even for small syncs.
                        // Non-asset entities (exif, albums, people) don't
                        // trigger the spinner.
                        if !notified_processing && counters.assets > 0 {
                            let _ = self.sync_events.send(SyncEvent::Processing {
                                items: counters.assets,
                            });
                            notified_processing = true;
                        }

                        // Issue #628: orphan tracking is handled by
                        // per-row `last_seen_at` heartbeats in the
                        // handlers themselves. Nothing per-line here.
                    }
                    Err(e) => {
                        warn!(entity_type, error = %e, "skipping sync entity");
                        if let Some(aid) = audit_id {
                            let _ = self.state.fail_audit(aid, &e.to_string()).await;
                        }
                        counters.errors += 1;
                    }
                }

                if counters.assets % 500 == 0 && counters.assets > 0 {
                    info!(assets = counters.assets, "sync progress");
                }
            } else {
                // Includes `SyncCompleteV1` only if the handler list
                // didn't pick it up — `SyncCompleteHandler` is
                // registered in `all_handlers()` so the normal
                // dispatch path handles it. The stream then ends
                // naturally when the server closes the connection;
                // `lines.next_line()` returns `None` and we exit the
                // loop. This branch covers genuinely-unknown types.
                debug!(
                    entity_type,
                    line_number, "ignoring unknown sync entity type"
                );
                acks.push(sync_line.ack);
            }

            if acks.len() >= ACK_FLUSH_THRESHOLD {
                self.flush_acks(&mut acks).await?;
                if counters.assets > 0 {
                    let _ = self.sync_events.send(SyncEvent::Processing {
                        items: counters.assets,
                    });
                }
            }
        }

        self.finish_sync(reset_checkpoint_at, &mut acks, &counters)
            .await
    }

    // ── Sync infrastructure ─────────────────────────────────────────────

    async fn finish_sync(
        &self,
        reset_checkpoint_at: Option<i64>,
        acks: &mut Vec<String>,
        counters: &SyncCounters,
    ) -> Result<(usize, usize), LibraryError> {
        // Issue #628: if a reset cycle was open, the stream just
        // closed it. Sweep rows whose heartbeat didn't catch up to
        // the cycle's checkpoint — these are entities the server has
        // dropped since the checkpoint was taken.
        //
        // Media and albums gate on `external_id IS NOT NULL` so
        // locally-imported / not-yet-pushed rows are immune. People
        // and asset_faces have no local-only counterpart and sweep
        // unconditionally below the checkpoint.
        if let Some(checkpoint) = reset_checkpoint_at {
            let media_orphans = self
                .library
                .media()
                .ids_with_stale_heartbeat(checkpoint)
                .await?;
            if !media_orphans.is_empty() {
                info!(
                    count = media_orphans.len(),
                    "removing orphaned assets after reset sync"
                );
                self.library
                    .delete_permanently_from_sync(&media_orphans)
                    .await?;
            }

            let album_orphans = self
                .library
                .albums()
                .delete_with_stale_heartbeat(checkpoint)
                .await?;
            if !album_orphans.is_empty() {
                info!(
                    count = album_orphans.len(),
                    "removing orphaned albums after reset sync"
                );
            }

            // Issue #224: stacks join the heartbeat sweep as the fifth
            // participating table. Members rejoin the un-stacked
            // timeline automatically via `media.stack_id`'s
            // `ON DELETE SET NULL` FK (migration 023).
            let stack_orphans = self
                .library
                .media()
                .delete_stacks_with_stale_heartbeat(checkpoint)
                .await?;
            if !stack_orphans.is_empty() {
                info!(
                    count = stack_orphans.len(),
                    "removing orphaned stacks after reset sync"
                );
            }

            let people_removed = self
                .library
                .faces()
                .delete_people_with_stale_heartbeat(checkpoint)
                .await?;
            if !people_removed.is_empty() {
                info!(
                    count = people_removed.len(),
                    "removing orphaned people after reset sync"
                );
            }

            let faces_removed = self
                .library
                .faces()
                .delete_asset_faces_with_stale_heartbeat(checkpoint)
                .await?;
            if faces_removed > 0 {
                info!(
                    count = faces_removed,
                    "removing orphaned asset_faces after reset sync"
                );
            }
        }

        if !acks.is_empty() {
            self.flush_acks(acks).await?;
        }

        let total = counters.assets + counters.albums + counters.people + counters.faces;
        if total > 0 || counters.errors > 0 {
            info!(
                synced = counters.assets,
                errors = counters.errors,
                total,
                "sync batch complete"
            );
        } else {
            debug!("sync complete — no new assets");
        }

        Ok((total, counters.assets))
    }

    async fn flush_acks(&self, acks: &mut Vec<String>) -> Result<(), LibraryError> {
        if acks.is_empty() {
            return Ok(());
        }

        info!(count = acks.len(), "flushing acks to server");
        for chunk in acks.chunks(1000) {
            let ack_request = SyncAckRequest {
                acks: chunk.to_vec(),
            };
            self.client
                .post_no_content("/sync/ack", &ack_request)
                .await?;

            // Save checkpoints after each successful chunk so that a
            // failure in a later chunk doesn't lose already-acked progress.
            //
            // Ack format is `entity_type|cursor`. `split('|').next()` is
            // infallible (returns the whole string when the separator is
            // absent), so we don't gate the insert on `if let Some`. A
            // malformed ack would key the checkpoint by the whole string
            // and the next sync cycle would resume from there — degraded
            // but not incorrect.
            let mut checkpoints: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for ack in chunk {
                let entity_type = ack.split('|').next().unwrap_or(ack.as_str());
                checkpoints.insert(entity_type.to_string(), ack.clone());
            }
            let pairs: Vec<(String, String)> = checkpoints.into_iter().collect();
            self.state.save_checkpoints(&pairs).await?;
        }

        acks.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SyncCounters ───────────────────────────────────────────────

    #[test]
    fn sync_counters_default_is_all_zero() {
        let c = SyncCounters::default();
        assert_eq!(c.assets, 0);
        assert_eq!(c.exifs, 0);
        assert_eq!(c.deletes, 0);
        assert_eq!(c.albums, 0);
        assert_eq!(c.people, 0);
        assert_eq!(c.faces, 0);
        assert_eq!(c.errors, 0);
    }

    #[test]
    fn sync_counters_increment() {
        let mut c = SyncCounters::default();
        c.increment(CounterKind::Assets);
        c.increment(CounterKind::Assets);
        c.increment(CounterKind::Deletes);
        c.increment(CounterKind::None);
        assert_eq!(c.assets, 2);
        assert_eq!(c.deletes, 1);
        assert_eq!(c.errors, 0);
    }

    /// Issue #679: a request type and the entity types it produces move
    /// together. Renaming one side without the other is not a loud
    /// failure — the dispatch loop logs unknown entity types at `debug`
    /// and carries on, so sync reports success while importing nothing.
    /// This pins the pairing so that combination cannot ship.
    #[test]
    fn requested_types_have_handlers() {
        // (request type, entity types it emits on the stream)
        let contract: &[(&str, &[&str])] = &[
            ("AssetsV2", &["AssetV2", "AssetDeleteV1"]),
            ("AssetExifsV1", &["AssetExifV1"]),
            ("AlbumsV1", &["AlbumV1", "AlbumDeleteV1"]),
            (
                "AlbumToAssetsV1",
                &["AlbumToAssetV1", "AlbumToAssetDeleteV1"],
            ),
            ("PeopleV1", &["PersonV1", "PersonDeleteV1"]),
            ("AssetFacesV2", &["AssetFaceV2", "AssetFaceDeleteV1"]),
            ("StacksV1", &["StackV1", "StackDeleteV1"]),
            ("AssetEditsV1", &["AssetEditV1", "AssetEditDeleteV1"]),
        ];

        let requested: Vec<&str> = contract.iter().map(|(req, _)| *req).collect();
        assert_eq!(
            requested, SYNC_REQUEST_TYPES,
            "SYNC_REQUEST_TYPES changed without updating the entity-type contract below"
        );

        let handlers = handlers::all_handlers();
        let registered: Vec<&str> = handlers.iter().map(|h| h.entity_type()).collect();

        for (request_type, entity_types) in contract {
            for entity in *entity_types {
                assert!(
                    registered.contains(entity),
                    "requesting {request_type} but no handler is registered for its \
                     {entity} entities — sync would silently drop them"
                );
            }
        }
    }

    /// Immich throws a 400 for these rather than degrading, which takes
    /// down the whole stream. Issue #679.
    #[test]
    fn no_retired_request_types() {
        for retired in [
            "AssetsV1",
            "AssetFacesV1",
            "PartnerAssetsV1",
            "AlbumAssetsV1",
        ] {
            assert!(
                !SYNC_REQUEST_TYPES.contains(&retired),
                "{retired} is deprecated server-side and 400s the entire sync stream"
            );
        }
    }
}
