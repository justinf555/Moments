use crate::library::album::AlbumId;
use crate::library::import::ImportSummary;
use crate::library::media::{MediaId, MediaItem};

/// Application-layer event type.
///
/// Translated from [`LibraryEvent`](crate::library::event::LibraryEvent) at the
/// application boundary and delivered to all [`EventBus`](crate::event_bus::EventBus)
/// subscribers. UI components subscribe to the events they care about; the bus
/// handles fan-out.
///
/// Events are split into two categories:
/// - **Result events** — outcomes from library operations or sync. Consumed by
///   models, sidebar, and other UI components.
/// - **Command events** (`*Requested`) — UI intent emitted by buttons. Consumed
///   by the [`CommandDispatcher`] which executes the library call and emits the
///   corresponding result event.
///
/// See `docs/design-event-bus.md` for the full design.
#[derive(Debug, Clone)]
pub enum AppEvent {
    // ── Lifecycle ────────────────────────────────────────────────────────────
    Ready,
    ShutdownComplete,
    Error(String),

    // ── Import ───────────────────────────────────────────────────────────────
    ImportProgress {
        current: usize,
        total: usize,
        imported: usize,
        skipped: usize,
        failed: usize,
    },
    ImportComplete {
        summary: ImportSummary,
    },

    // ── Thumbnails ───────────────────────────────────────────────────────────
    ThumbnailReady {
        media_id: MediaId,
    },
    ThumbnailDownloadProgress {
        completed: usize,
        total: usize,
    },
    ThumbnailDownloadsComplete {
        total: usize,
    },

    // ── Commands (UI intent → CommandDispatcher) ─────────────────────────────
    TrashRequested {
        ids: Vec<MediaId>,
    },
    RestoreRequested {
        ids: Vec<MediaId>,
    },
    DeleteRequested {
        ids: Vec<MediaId>,
    },
    FavoriteRequested {
        ids: Vec<MediaId>,
        state: bool,
    },
    RemoveFromAlbumRequested {
        album_id: AlbumId,
        ids: Vec<MediaId>,
    },

    // ── Results (CommandDispatcher → subscribers) ────────────────────────────
    FavoriteChanged {
        ids: Vec<MediaId>,
        is_favorite: bool,
    },
    Trashed {
        ids: Vec<MediaId>,
    },
    Restored {
        ids: Vec<MediaId>,
    },
    Deleted {
        ids: Vec<MediaId>,
    },
    AssetSynced {
        item: MediaItem,
    },
    AssetDeletedRemote {
        media_id: MediaId,
    },

    // ── Albums ───────────────────────────────────────────────────────────────
    AlbumCreated {
        id: AlbumId,
        name: String,
    },
    AlbumRenamed {
        id: AlbumId,
        name: String,
    },
    AlbumDeleted {
        id: AlbumId,
    },
    AlbumMediaChanged {
        album_id: AlbumId,
    },

    // ── Sync ─────────────────────────────────────────────────────────────────
    SyncStarted,
    SyncProgress {
        assets: usize,
        people: usize,
        faces: usize,
    },
    SyncComplete {
        assets: usize,
        people: usize,
        faces: usize,
        errors: usize,
    },
    PeopleSyncComplete,
}
