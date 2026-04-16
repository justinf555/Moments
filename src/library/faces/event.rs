use super::model::PersonId;

/// Events emitted by `FacesService` after state changes.
///
/// Consumed by `PeopleClientV2` to patch ListStore models in-place.
/// Sent via `tokio::sync::mpsc` — multiple producers possible via
/// `FacesService: Clone`, single consumer (client).
#[derive(Debug, Clone)]
pub enum FacesEvent {
    /// A new person was added (sync upsert, no prior row).
    PersonAdded(PersonId),
    /// An existing person was updated (sync upsert, rename, hide, face count).
    PersonUpdated(PersonId),
    /// A person was removed (sync delete).
    PersonRemoved(PersonId),
}
