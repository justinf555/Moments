use super::model::AlbumId;

/// Events emitted by `AlbumService` after state changes.
///
/// Consumed by clients (`AlbumClientV2`, and in future `MediaClientV2`
/// for album-filtered media models) to patch ListStore models in place.
/// Delivered via an [`EventEmitter`] — single producer (the service),
/// multiple consumers (fan-out).
///
/// [`EventEmitter`]: crate::event_emitter::EventEmitter
#[derive(Debug, Clone)]
pub enum AlbumEvent {
    /// A new album row was added (sync upsert, no prior row).
    AlbumAdded(AlbumId),
    /// An existing album row was updated (sync upsert with an existing row,
    /// or — transitionally — a membership change; see [`AlbumMediaChanged`]).
    ///
    /// [`AlbumMediaChanged`]: AlbumEvent::AlbumMediaChanged
    AlbumUpdated(AlbumId),
    /// An album was removed (local delete or sync delete).
    AlbumRemoved(AlbumId),
    /// An album's media membership changed: photos were added to or removed
    /// from it. Emitted alongside `AlbumUpdated` today for backward
    /// compatibility; consumers that care only about membership (e.g. a
    /// media grid filtered by album) should subscribe to this variant
    /// specifically to avoid refreshing on unrelated metadata updates.
    AlbumMediaChanged(AlbumId),
}
