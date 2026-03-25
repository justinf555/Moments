use std::path::PathBuf;

use super::album::AlbumId;
use super::error::LibraryError;
use super::import::ImportSummary;
use super::media::{MediaId, MediaItem};

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
    ImportProgress {
        current: usize,
        total: usize,
        imported: usize,
        skipped: usize,
        failed: usize,
    },

    /// Import pipeline finished (successfully or with per-file failures).
    ImportComplete(ImportSummary),

    // ── Thumbnail events ───────────────────────────────────────────────────────

    /// The grid thumbnail for an asset has been generated and written to disk.
    ThumbnailReady { media_id: MediaId },

    /// Thumbnail download progress (Immich sync).
    ThumbnailDownloadProgress { completed: usize, total: usize },

    /// All queued thumbnail downloads have finished.
    ThumbnailDownloadsComplete { total: usize },

    // ── Album events ────────────────────────────────────────────────────────

    /// A new album was created.
    AlbumCreated { id: AlbumId, name: String },

    /// An album was renamed.
    AlbumRenamed { id: AlbumId, name: String },

    /// An album was deleted.
    AlbumDeleted { id: AlbumId },

    /// Media items were added to or removed from an album.
    AlbumMediaChanged { album_id: AlbumId },

    // ── Sync events ─────────────────────────────────────────────────────────

    // ── Sync lifecycle events ────────────────────────────────────────────

    /// The sync stream has connected and is processing records.
    SyncStarted,

    /// Periodic sync progress (emitted every ack flush).
    SyncProgress { assets: usize, people: usize, faces: usize },

    /// The sync stream has finished processing.
    SyncComplete { assets: usize, people: usize, faces: usize, errors: usize },

    /// A single asset was synced from the server. Used for incremental
    /// grid updates without full reload.
    AssetSynced { item: MediaItem },

    /// People data changed during sync (new/updated/deleted people or faces).
    /// The People collection grid should reload.
    PeopleSyncComplete,

    /// An asset was permanently deleted on the server.
    AssetDeletedRemote { media_id: MediaId },
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
            imported: 2,
            skipped: 1,
            failed: 0,
        };
        assert!(format!("{event:?}").contains("ImportProgress"));
    }

    #[test]
    fn import_complete_contains_summary() {
        let event = LibraryEvent::ImportComplete(ImportSummary::default());
        assert!(format!("{event:?}").contains("ImportComplete"));
    }
}
