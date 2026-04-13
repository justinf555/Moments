use async_trait::async_trait;

use super::error::LibraryError;
use super::media::MediaId;

/// Unique identifier for a person (Immich UUID or future local ID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PersonId(String);

impl PersonId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_raw(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for PersonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A recognised person with face detection data.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: PersonId,
    pub name: String,
    pub face_count: u32,
    pub is_hidden: bool,
}

/// Feature trait for face/people management.
///
/// Implemented by every backend. The Immich backend reads from locally
/// synced `people` / `asset_faces` tables. The local backend returns
/// empty results until a local face detection pipeline is added.
#[async_trait]
pub trait LibraryFaces: Send + Sync {
    /// List all people, ordered by face count descending.
    ///
    /// If `include_hidden` is false, hidden people are excluded.
    /// If `include_unnamed` is false, people with empty names are excluded.
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError>;

    /// Get media IDs for all assets containing a specific person.
    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError>;

    /// Rename a person.
    async fn rename_person(&self, person_id: &PersonId, name: &str) -> Result<(), LibraryError>;

    /// Hide or unhide a person.
    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError>;

    /// Merge source people into the target person.
    /// All faces from the source people are reassigned to the target,
    /// and the source people are deleted.
    async fn merge_people(
        &self,
        target: &PersonId,
        sources: &[PersonId],
    ) -> Result<(), LibraryError>;

    /// Return the filesystem path to a person's face thumbnail, if available.
    ///
    /// The Immich backend stores these at `{thumbnails_dir}/people/{id}.jpg`.
    /// The local backend returns `None`.
    fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn person_id_display() {
        let id = PersonId::from_raw("abc-123".to_string());
        assert_eq!(format!("{id}"), "abc-123");
    }

    #[test]
    fn person_id_as_str() {
        let id = PersonId::from_raw("abc-123".to_string());
        assert_eq!(id.as_str(), "abc-123");
    }

    #[test]
    fn person_id_equality() {
        let a = PersonId::from_raw("same".to_string());
        let b = PersonId::from_raw("same".to_string());
        let c = PersonId::from_raw("different".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn person_id_clone() {
        let id = PersonId::from_raw("test".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn person_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(PersonId::from_raw("a".to_string()));
        set.insert(PersonId::from_raw("a".to_string()));
        set.insert(PersonId::from_raw("b".to_string()));
        assert_eq!(set.len(), 2);
    }
}
