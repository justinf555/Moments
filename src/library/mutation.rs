//! Library mutation types.
//!
//! Every state change the library can produce is represented as a
//! [`Mutation`] variant. Consumers (sync outbox, UI clients) decide
//! what to do with each mutation.

use std::path::PathBuf;

use crate::library::album::AlbumId;
use crate::library::faces::PersonId;
use crate::library::media::MediaId;

/// A state change produced by a library service.
#[derive(Debug, Clone)]
pub enum Mutation {
    // ── Asset ────────────────────────────────────────────────────────

    /// A new asset was imported locally and may need uploading.
    AssetImported { id: MediaId, file_path: PathBuf },

    /// One or more assets had their favourite state changed.
    AssetFavorited { ids: Vec<MediaId>, favorite: bool },

    /// One or more assets were moved to the trash.
    AssetTrashed { ids: Vec<MediaId> },

    /// One or more assets were restored from the trash.
    AssetRestored { ids: Vec<MediaId> },

    /// One or more assets were permanently deleted.
    AssetDeleted { ids: Vec<MediaId> },

    // ── Album ────────────────────────────────────────────────────────

    /// A new album was created.
    AlbumCreated { id: AlbumId, name: String },

    /// An album was renamed.
    AlbumRenamed { id: AlbumId, name: String },

    /// An album was deleted.
    AlbumDeleted { id: AlbumId },

    /// Media items were added to an album.
    AlbumMediaAdded {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },

    /// Media items were removed from an album.
    AlbumMediaRemoved {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },

    // ── People ───────────────────────────────────────────────────────

    /// A person was renamed.
    PersonRenamed { id: PersonId, name: String },

    /// A person's hidden state was changed.
    PersonHidden { id: PersonId, hidden: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_debug_format() {
        let m = Mutation::AssetTrashed {
            ids: vec![MediaId::new("abc".to_string())],
        };
        let dbg = format!("{m:?}");
        assert!(dbg.contains("AssetTrashed"));
        assert!(dbg.contains("abc"));
    }

    #[test]
    fn mutation_clone_is_independent() {
        let original = Mutation::AlbumCreated {
            id: AlbumId::from_raw("album-1".to_string()),
            name: "Photos".to_string(),
        };
        let cloned = original.clone();

        // Both exist independently.
        let orig_dbg = format!("{original:?}");
        let clone_dbg = format!("{cloned:?}");
        assert_eq!(orig_dbg, clone_dbg);
    }

    #[test]
    fn all_mutation_variants_can_be_constructed() {
        // Verify every variant compiles and can be debug-printed.
        let mutations: Vec<Mutation> = vec![
            Mutation::AssetImported {
                id: MediaId::new("id1".to_string()),
                file_path: PathBuf::from("/photos/test.jpg"),
            },
            Mutation::AssetFavorited {
                ids: vec![MediaId::new("id2".to_string())],
                favorite: true,
            },
            Mutation::AssetTrashed {
                ids: vec![MediaId::new("id3".to_string())],
            },
            Mutation::AssetRestored {
                ids: vec![MediaId::new("id4".to_string())],
            },
            Mutation::AssetDeleted {
                ids: vec![MediaId::new("id5".to_string())],
            },
            Mutation::AlbumCreated {
                id: AlbumId::from_raw("a1".to_string()),
                name: "Album".to_string(),
            },
            Mutation::AlbumRenamed {
                id: AlbumId::from_raw("a2".to_string()),
                name: "Renamed".to_string(),
            },
            Mutation::AlbumDeleted {
                id: AlbumId::from_raw("a3".to_string()),
            },
            Mutation::AlbumMediaAdded {
                album_id: AlbumId::from_raw("a4".to_string()),
                media_ids: vec![MediaId::new("m1".to_string())],
            },
            Mutation::AlbumMediaRemoved {
                album_id: AlbumId::from_raw("a5".to_string()),
                media_ids: vec![MediaId::new("m2".to_string())],
            },
            Mutation::PersonRenamed {
                id: PersonId::from_raw("p1".to_string()),
                name: "Alice".to_string(),
            },
            Mutation::PersonHidden {
                id: PersonId::from_raw("p2".to_string()),
                hidden: true,
            },
        ];

        assert_eq!(mutations.len(), 12);
        for m in &mutations {
            // Ensure Debug doesn't panic.
            let _ = format!("{m:?}");
        }
    }

    #[test]
    fn mutation_clone_deep_copies_vecs() {
        let original = Mutation::AssetTrashed {
            ids: vec![
                MediaId::new("a".to_string()),
                MediaId::new("b".to_string()),
            ],
        };
        let cloned = original.clone();

        // Verify the clone is structurally equal.
        if let (
            Mutation::AssetTrashed { ids: orig_ids },
            Mutation::AssetTrashed { ids: clone_ids },
        ) = (&original, &cloned)
        {
            assert_eq!(orig_ids.len(), clone_ids.len());
            assert_eq!(orig_ids[0].as_str(), clone_ids[0].as_str());
            assert_eq!(orig_ids[1].as_str(), clone_ids[1].as_str());
        } else {
            panic!("expected AssetTrashed variants");
        }
    }
}
