use super::model::PersonId;

/// Events emitted by `FacesService` after state changes.
///
/// Consumed by clients (`PeopleClientV2`, and in future `MediaClientV2`
/// for person-filtered media models) to patch ListStore models in place.
/// Delivered via an [`EventEmitter`] — single producer (the service),
/// multiple consumers (fan-out).
///
/// [`EventEmitter`]: crate::event_emitter::EventEmitter
#[derive(Debug, Clone)]
pub enum FacesEvent {
    /// A new person was added (sync upsert, no prior row).
    PersonAdded(PersonId),
    /// An existing person row was updated (sync upsert, rename, hide).
    ///
    /// Deliberately not emitted from `update_face_count` — `face_count`
    /// is not a GObject property, and bulk sync would otherwise produce
    /// O(faces) no-op roundtrips.
    PersonUpdated(PersonId),
    /// A person was removed (sync delete).
    PersonRemoved(PersonId),
    /// The set of media assigned to this person changed — an asset face
    /// was created, reassigned to or from this person, or deleted.
    ///
    /// Consumed by media grids filtered by person. A reassignment emits
    /// two `PersonMediaChanged` events (one for the previous person, one
    /// for the new). On a local backend this never fires — local
    /// libraries have no face detection.
    PersonMediaChanged(PersonId),
}
