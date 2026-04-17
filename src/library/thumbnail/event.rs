use crate::library::media::MediaId;

/// Events emitted by `ThumbnailService` after thumbnail state changes.
///
/// Consumed by clients (`MediaClientV2`, in a future PR) to load the
/// thumbnail texture into grid cells as soon as a thumbnail is written
/// to disk. Delivered via an [`EventEmitter`] — single producer (the
/// service), multiple consumers (fan-out).
///
/// Single-id variant is intentional: thumbnails are generated one per
/// asset, both by the local import pipeline and by the Immich sync
/// download. There is no batch producer.
///
/// [`EventEmitter`]: crate::event_emitter::EventEmitter
#[derive(Debug, Clone)]
pub enum ThumbnailEvent {
    /// A thumbnail is now on disk and can be loaded as a texture.
    Ready(MediaId),
}
