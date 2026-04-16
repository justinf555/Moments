use super::model::AlbumId;

/// Events emitted by `AlbumService` after state changes.
///
/// Consumed by `AlbumClientV2` to patch ListStore models in-place.
/// Sent via `tokio::sync::mpsc` — single producer (service), single
/// consumer (client).
#[derive(Debug, Clone)]
pub enum AlbumEvent {
    /// A new album was added (sync upsert, no prior row).
    AlbumAdded(AlbumId),
    /// An existing album was updated (sync upsert, media added/removed).
    AlbumUpdated(AlbumId),
    /// An album was removed (sync delete).
    AlbumRemoved(AlbumId),
}
