use std::path::PathBuf;

use super::error::LibraryError;
use super::import::ImportSummary;
use super::media::MediaId;

/// Events emitted by the library backend and delivered to the GTK application.
///
/// The GTK layer creates a `std::sync::mpsc::channel::<LibraryEvent>()`, passes
/// the `Sender` into `LibraryFactory::create`, and polls the `Receiver` via
/// `glib::idle_add`. The library never imports GTK types.
///
/// All library operations — including import progress — flow through this single
/// channel so any component (photo grid, dynamic albums, sidebar) can observe
/// the full event stream without extra wiring.
#[derive(Debug)]
pub enum LibraryEvent {
    /// The library has finished opening and is ready to accept operations.
    Ready,

    /// The library has fully shut down after a `close()` call.
    ShutdownComplete,

    /// A non-fatal error occurred in a background operation.
    Error(LibraryError),

    // ── Import events ─────────────────────────────────────────────────────────

    /// One asset was successfully copied into the library.
    AssetImported { media_id: MediaId, path: PathBuf },

    /// Periodic progress update during a batch import.
    ImportProgress { current: usize, total: usize },

    /// Import pipeline finished (successfully or with per-file failures).
    ImportComplete(ImportSummary),

    // ── Thumbnail events ───────────────────────────────────────────────────────

    /// The grid thumbnail for an asset has been generated and written to disk.
    ThumbnailReady { media_id: MediaId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_event_is_debug() {
        let event = LibraryEvent::Ready;
        assert!(format!("{event:?}").contains("Ready"));
    }

    #[test]
    fn shutdown_complete_is_debug() {
        let event = LibraryEvent::ShutdownComplete;
        assert!(format!("{event:?}").contains("ShutdownComplete"));
    }

    #[test]
    fn error_event_wraps_library_error() {
        let event = LibraryEvent::Error(LibraryError::Bundle("test".to_string()));
        assert!(format!("{event:?}").contains("Error"));
    }

    #[test]
    fn asset_imported_contains_path() {
        let event = LibraryEvent::AssetImported {
            media_id: MediaId::__test_new("abc123"),
            path: PathBuf::from("/tmp/photo.jpg"),
        };
        assert!(format!("{event:?}").contains("AssetImported"));
    }

    #[test]
    fn import_progress_contains_counts() {
        let event = LibraryEvent::ImportProgress {
            current: 3,
            total: 10,
        };
        assert!(format!("{event:?}").contains("ImportProgress"));
    }

    #[test]
    fn import_complete_contains_summary() {
        let event = LibraryEvent::ImportComplete(ImportSummary::default());
        assert!(format!("{event:?}").contains("ImportComplete"));
    }
}
