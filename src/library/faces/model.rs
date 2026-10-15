// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

/// Unique identifier for a person (Immich UUID or future local ID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PersonId(String);

impl PersonId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wrap an existing ID string (from database or sync stream). People
    /// only come from Immich, so there is no `generate()`.
    pub fn new(id: String) -> Self {
        Self(id)
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
        let id = PersonId::new("abc-123".to_string());
        assert_eq!(format!("{id}"), "abc-123");
    }

    #[test]
    fn person_id_as_str() {
        let id = PersonId::new("abc-123".to_string());
        assert_eq!(id.as_str(), "abc-123");
    }

    #[test]
    fn person_id_equality() {
        let a = PersonId::new("same".to_string());
        let b = PersonId::new("same".to_string());
        let c = PersonId::new("different".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn person_id_clone() {
        let id = PersonId::new("test".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn person_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(PersonId::new("a".to_string()));
        set.insert(PersonId::new("a".to_string()));
        set.insert(PersonId::new("b".to_string()));
        assert_eq!(set.len(), 2);
    }
}
