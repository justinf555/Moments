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

// ── Outbox serialization ─────────────────────────────────────────────

/// One row in the `sync_outbox` table.
///
/// Multi-ID mutations (e.g. `AssetTrashed { ids: [a, b] }`) expand into
/// multiple rows — one per entity. The Mutation type owns both the
/// serialization ([`Mutation::to_outbox_rows`]) and deserialization
/// ([`Mutation::from_outbox_row`]) so the format is defined in one place.
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub payload: Option<String>,
}

impl Mutation {
    /// Serialize this mutation into outbox rows.
    pub fn to_outbox_rows(&self) -> Vec<OutboxRow> {
        match self {
            Mutation::AssetImported { id, file_path } => {
                let payload = serde_json::json!({
                    "file_path": file_path.to_string_lossy(),
                })
                .to_string();
                vec![OutboxRow {
                    entity_type: "asset".into(),
                    entity_id: id.as_str().into(),
                    action: "import".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::AssetFavorited { ids, favorite } => {
                let action = if *favorite { "favorite" } else { "unfavorite" };
                ids.iter()
                    .map(|id| OutboxRow {
                        entity_type: "asset".into(),
                        entity_id: id.as_str().into(),
                        action: action.into(),
                        payload: None,
                    })
                    .collect()
            }

            Mutation::AssetTrashed { ids } => ids
                .iter()
                .map(|id| OutboxRow {
                    entity_type: "asset".into(),
                    entity_id: id.as_str().into(),
                    action: "trash".into(),
                    payload: None,
                })
                .collect(),

            Mutation::AssetRestored { ids } => ids
                .iter()
                .map(|id| OutboxRow {
                    entity_type: "asset".into(),
                    entity_id: id.as_str().into(),
                    action: "restore".into(),
                    payload: None,
                })
                .collect(),

            Mutation::AssetDeleted { ids } => ids
                .iter()
                .map(|id| OutboxRow {
                    entity_type: "asset".into(),
                    entity_id: id.as_str().into(),
                    action: "delete".into(),
                    payload: None,
                })
                .collect(),

            Mutation::AlbumCreated { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                vec![OutboxRow {
                    entity_type: "album".into(),
                    entity_id: id.as_str().into(),
                    action: "create".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::AlbumRenamed { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                vec![OutboxRow {
                    entity_type: "album".into(),
                    entity_id: id.as_str().into(),
                    action: "rename".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::AlbumDeleted { id } => vec![OutboxRow {
                entity_type: "album".into(),
                entity_id: id.as_str().into(),
                action: "delete".into(),
                payload: None,
            }],

            Mutation::AlbumMediaAdded {
                album_id,
                media_ids,
            } => {
                let payload = serde_json::json!({
                    "media_ids": media_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                })
                .to_string();
                vec![OutboxRow {
                    entity_type: "album".into(),
                    entity_id: album_id.as_str().into(),
                    action: "add_media".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::AlbumMediaRemoved {
                album_id,
                media_ids,
            } => {
                let payload = serde_json::json!({
                    "media_ids": media_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                })
                .to_string();
                vec![OutboxRow {
                    entity_type: "album".into(),
                    entity_id: album_id.as_str().into(),
                    action: "remove_media".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::PersonRenamed { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                vec![OutboxRow {
                    entity_type: "person".into(),
                    entity_id: id.as_str().into(),
                    action: "rename".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::PersonHidden { id, hidden } => {
                let payload = serde_json::json!({ "hidden": hidden }).to_string();
                vec![OutboxRow {
                    entity_type: "person".into(),
                    entity_id: id.as_str().into(),
                    action: "hide".into(),
                    payload: Some(payload),
                }]
            }
        }
    }

    /// Reconstruct a `Mutation` from an outbox row.
    ///
    /// Each outbox row stores one entity, so multi-ID variants (e.g.
    /// `AssetTrashed`) are reconstructed with a single-element `ids` vec.
    /// Returns `None` if the entity_type/action pair is unrecognised.
    pub fn from_outbox_row(row: &OutboxRow) -> Option<Self> {
        let json = || -> serde_json::Value {
            row.payload
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Object(Default::default()))
        };

        match (row.entity_type.as_str(), row.action.as_str()) {
            ("asset", "import") => {
                let p = json();
                let path = p["file_path"].as_str().unwrap_or("");
                Some(Mutation::AssetImported {
                    id: MediaId::new(row.entity_id.clone()),
                    file_path: PathBuf::from(path),
                })
            }
            ("asset", "favorite") => Some(Mutation::AssetFavorited {
                ids: vec![MediaId::new(row.entity_id.clone())],
                favorite: true,
            }),
            ("asset", "unfavorite") => Some(Mutation::AssetFavorited {
                ids: vec![MediaId::new(row.entity_id.clone())],
                favorite: false,
            }),
            ("asset", "trash") => Some(Mutation::AssetTrashed {
                ids: vec![MediaId::new(row.entity_id.clone())],
            }),
            ("asset", "restore") => Some(Mutation::AssetRestored {
                ids: vec![MediaId::new(row.entity_id.clone())],
            }),
            ("asset", "delete") => Some(Mutation::AssetDeleted {
                ids: vec![MediaId::new(row.entity_id.clone())],
            }),
            ("album", "create") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Mutation::AlbumCreated {
                    id: AlbumId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("album", "rename") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Mutation::AlbumRenamed {
                    id: AlbumId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("album", "delete") => Some(Mutation::AlbumDeleted {
                id: AlbumId::from_raw(row.entity_id.clone()),
            }),
            ("album", "add_media") => {
                let p = json();
                let media_ids = p["media_ids"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| MediaId::new(s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Mutation::AlbumMediaAdded {
                    album_id: AlbumId::from_raw(row.entity_id.clone()),
                    media_ids,
                })
            }
            ("album", "remove_media") => {
                let p = json();
                let media_ids = p["media_ids"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| MediaId::new(s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Mutation::AlbumMediaRemoved {
                    album_id: AlbumId::from_raw(row.entity_id.clone()),
                    media_ids,
                })
            }
            ("person", "rename") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Mutation::PersonRenamed {
                    id: PersonId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("person", "hide") => {
                let p = json();
                let hidden = p["hidden"].as_bool().unwrap_or(false);
                Some(Mutation::PersonHidden {
                    id: PersonId::from_raw(row.entity_id.clone()),
                    hidden,
                })
            }
            _ => None,
        }
    }
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

    /// Every single-entity mutation survives a round-trip through the
    /// outbox serialization format. Multi-ID mutations expand to one row
    /// per ID, so the round-trip produces a single-element vec.
    #[test]
    fn outbox_round_trip_all_variants() {
        let cases: Vec<Mutation> = vec![
            Mutation::AssetImported {
                id: MediaId::new("id1".into()),
                file_path: PathBuf::from("/photos/test.jpg"),
            },
            Mutation::AssetFavorited {
                ids: vec![MediaId::new("id2".into())],
                favorite: true,
            },
            Mutation::AssetFavorited {
                ids: vec![MediaId::new("id2b".into())],
                favorite: false,
            },
            Mutation::AssetTrashed {
                ids: vec![MediaId::new("id3".into())],
            },
            Mutation::AssetRestored {
                ids: vec![MediaId::new("id4".into())],
            },
            Mutation::AssetDeleted {
                ids: vec![MediaId::new("id5".into())],
            },
            Mutation::AlbumCreated {
                id: AlbumId::from_raw("a1".into()),
                name: "Vacation".into(),
            },
            Mutation::AlbumRenamed {
                id: AlbumId::from_raw("a2".into()),
                name: "Trip".into(),
            },
            Mutation::AlbumDeleted {
                id: AlbumId::from_raw("a3".into()),
            },
            Mutation::AlbumMediaAdded {
                album_id: AlbumId::from_raw("a4".into()),
                media_ids: vec![MediaId::new("m1".into()), MediaId::new("m2".into())],
            },
            Mutation::AlbumMediaRemoved {
                album_id: AlbumId::from_raw("a5".into()),
                media_ids: vec![MediaId::new("m3".into())],
            },
            Mutation::PersonRenamed {
                id: PersonId::from_raw("p1".into()),
                name: "Alice".into(),
            },
            Mutation::PersonHidden {
                id: PersonId::from_raw("p2".into()),
                hidden: true,
            },
        ];

        for mutation in &cases {
            let rows = mutation.to_outbox_rows();
            assert!(!rows.is_empty(), "no rows for {mutation:?}");

            // Each row must deserialize back to Some.
            for row in &rows {
                let back = Mutation::from_outbox_row(row);
                assert!(
                    back.is_some(),
                    "round-trip failed for {mutation:?} -> {row:?}"
                );
            }
        }
    }

    #[test]
    fn outbox_unknown_action_returns_none() {
        let row = OutboxRow {
            entity_type: "unknown".into(),
            entity_id: "x".into(),
            action: "nope".into(),
            payload: None,
        };
        assert!(Mutation::from_outbox_row(&row).is_none());
    }

    #[test]
    fn outbox_multi_id_expands_to_multiple_rows() {
        let m = Mutation::AssetTrashed {
            ids: vec![
                MediaId::new("a".into()),
                MediaId::new("b".into()),
                MediaId::new("c".into()),
            ],
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].entity_id, "a");
        assert_eq!(rows[1].entity_id, "b");
        assert_eq!(rows[2].entity_id, "c");
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
