// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Wiring the `originals-cache-max-mb` setting to the Immich originals
//! cache (#711). GSettings stays on the GTK thread; the resolver only
//! sees a plain byte count through a shared `AtomicU64`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk::{gio, glib, prelude::*};
use tracing::{debug, info, warn};

use crate::library::db::Database;
use crate::library::Library;
use crate::sync::outbox::OutboxRepository;
use crate::sync::providers::immich::resolver::cache::mb_to_bytes;
use crate::sync::providers::immich::resolver::guard::LibraryEvictionGuard;
use crate::sync::providers::immich::resolver::CachedResolver;

const LIMIT_KEY: &str = "originals-cache-max-mb";

/// Wait this long after the last change before trimming, so stepping the
/// spin row down doesn't run a pass per step.
const LIVE_PASS_DELAY: Duration = Duration::from_secs(2);

/// The current limit, in bytes, as a value the resolver can share.
pub(super) fn limit_from_settings(settings: &gio::Settings) -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(mb_to_bytes(settings.uint(LIMIT_KEY))))
}

/// Once the library is open: enable eviction, run one pass on Tokio so a
/// lowered limit applies without a download, and follow the setting.
pub(super) fn start(
    settings: &gio::Settings,
    resolver: &Arc<CachedResolver>,
    library: &Arc<Library>,
    db: Database,
    limit: Arc<AtomicU64>,
    tokio: &tokio::runtime::Handle,
) {
    resolver.set_eviction_guard(Arc::new(LibraryEvictionGuard::new(
        Arc::downgrade(library),
        OutboxRepository::new(db),
    )));

    spawn_pass(tokio, Arc::downgrade(resolver), "startup");

    // The handler lives as long as the settings object (the app). It holds
    // the resolver weakly, so it goes inert if the library is closed.
    let weak = Arc::downgrade(resolver);
    let tokio = tokio.clone();
    let generation = Rc::new(Cell::new(0u64));
    settings.connect_changed(Some(LIMIT_KEY), move |settings, key| {
        let bytes = mb_to_bytes(settings.uint(key));
        limit.store(bytes, Ordering::Relaxed);
        debug!(bytes, "originals cache limit changed");

        let gen = generation.get() + 1;
        generation.set(gen);
        let generation = Rc::clone(&generation);
        let weak = weak.clone();
        let tokio = tokio.clone();
        glib::timeout_add_local_once(LIVE_PASS_DELAY, move || {
            if generation.get() == gen {
                spawn_pass(&tokio, weak, "limit changed");
            }
        });
    });
}

fn spawn_pass(
    tokio: &tokio::runtime::Handle,
    resolver: std::sync::Weak<CachedResolver>,
    reason: &'static str,
) {
    tokio.spawn(async move {
        let Some(resolver) = resolver.upgrade() else {
            return;
        };
        match resolver.enforce_limit().await {
            Ok(outcome) => info!(
                reason,
                evicted = outcome.evicted,
                freed_bytes = outcome.freed_bytes,
                remaining_bytes = outcome.remaining_bytes,
                "originals cache checked"
            ),
            Err(e) => warn!(reason, "originals cache eviction failed: {e}"),
        }
    });
}
