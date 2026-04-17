use super::model::MediaId;

/// Events emitted by `MediaService` after state changes.
///
/// Consumed by clients (`MediaClientV2`, in a future PR) to patch ListStore
/// models in place. Delivered via an [`EventEmitter`] — single producer
/// (the service), multiple consumers (fan-out).
///
/// All variants carry a `Vec<MediaId>` so batch operations (trash, restore,
/// bulk favourite) can emit one aggregate event instead of one per id.
/// Single-item callers emit `vec![id]`. Consumers should loop over the vec
/// when patching models.
///
/// [`EventEmitter`]: crate::event_emitter::EventEmitter
#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// Media rows were inserted — local imports (`insert_media`) or new
    /// rows from the sync stream (`upsert_media` with no prior row).
    Added(Vec<MediaId>),
    /// Media rows changed — `set_favorite`, `trash`, `restore`, or a sync
    /// upsert that matched an existing row. Consumers should re-query the
    /// affected rows and either patch fields in their tracked models or
    /// remove rows that no longer match the model's filter (e.g. a trashed
    /// item drops out of a "all non-trashed" model).
    Updated(Vec<MediaId>),
    /// Media rows were permanently deleted (`delete_permanently`, either
    /// from local user action or from the sync stream).
    Removed(Vec<MediaId>),
}
