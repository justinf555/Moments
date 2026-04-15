//! Periodic auto-purge of expired trash items.
//!
//! Runs on the Tokio runtime: queries for items past the retention period,
//! permanently deletes them, and emits `Deleted` so clients update the UI.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

/// How often to check for expired trash items.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60); // 1 hour

/// Start the periodic trash purge task.
///
/// Runs an immediate check on startup (cleans up items that expired while
/// the app was closed), then repeats every hour. The `retention_days` value
/// is read from GSettings on the GTK thread before calling this function.
pub fn start(
    library: Arc<Library>,
    bus: EventSender,
    retention_days: u32,
) -> JoinHandle<()> {
    let max_age_secs = i64::from(retention_days) * 24 * 60 * 60;

    tokio::spawn(async move {
        loop {
            purge_expired(&library, &bus, max_age_secs, retention_days).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    })
}

/// Find and permanently delete all items past the retention period.
#[instrument(skip(library, bus))]
async fn purge_expired(
    library: &Library,
    bus: &EventSender,
    max_age_secs: i64,
    retention_days: u32,
) {
    let expired = match library.media().expired_trash(max_age_secs).await {
        Ok(ids) => ids,
        Err(e) => {
            error!("failed to query expired trash: {e}");
            return;
        }
    };

    if expired.is_empty() {
        debug!(retention_days, "no expired trash items");
        return;
    }

    info!(count = expired.len(), retention_days, "purging expired trash");

    if let Err(e) = library.delete_permanently(&expired).await {
        error!("failed to purge expired trash: {e}");
        return;
    }

    bus.send(AppEvent::Deleted { ids: expired });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_interval_is_one_hour() {
        assert_eq!(CHECK_INTERVAL, Duration::from_secs(3600));
    }
}
