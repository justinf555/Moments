/// Events emitted by the sync engine for UI state tracking.
///
/// Consumed by `SyncClient` to update GObject properties.
/// Sent via `tokio::sync::mpsc` — produced by both the pull and push
/// managers, consumed by the client singleton.
///
/// Note: there is no `Started` variant. The sync engine connects to
/// the stream silently; `Processing` fires only when items actually
/// need work. An empty stream emits `Complete { items: 0 }` directly.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    /// Items are being synced (pull or push). The count is a running
    /// total across both directions — "N changes pending".
    Processing { items: usize },
    /// A sync cycle (pull or push) finished.
    Complete { items: usize, errors: usize },
    /// Sync failed (authentication, server error, etc.).
    Error { message: String },
    /// Server is unreachable.
    Offline,
}

/// Check whether a sync error indicates a connectivity/network problem
/// (offline) vs a server-side or protocol error.
pub fn is_connectivity_error(err: &crate::library::error::LibraryError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("connection refused")
        || msg.contains("dns error")
        || msg.contains("timed out")
        || msg.contains("network is unreachable")
        || msg.contains("no route to host")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_event_debug_format() {
        let event = SyncEvent::Processing { items: 42 };
        let debug = format!("{event:?}");
        assert!(debug.contains("Processing"));
        assert!(debug.contains("42"));
    }

    #[test]
    fn sync_event_clone() {
        let event = SyncEvent::Error {
            message: "auth failed".to_string(),
        };
        let cloned = event.clone();
        assert!(matches!(cloned, SyncEvent::Error { message } if message == "auth failed"));
    }

    #[test]
    fn sync_event_complete_fields() {
        let event = SyncEvent::Complete {
            items: 10,
            errors: 2,
        };
        assert!(matches!(
            event,
            SyncEvent::Complete {
                items: 10,
                errors: 2
            }
        ));
    }

    #[test]
    fn sync_event_offline_variant() {
        let event = SyncEvent::Offline;
        assert!(matches!(event, SyncEvent::Offline));
    }
}
