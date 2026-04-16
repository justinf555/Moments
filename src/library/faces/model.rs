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
