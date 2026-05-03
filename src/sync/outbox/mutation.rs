//! Single-entity mutation type used by the push manager.
//!
//! [`Mutation`](crate::library::mutation::Mutation) is the in-process
//! broadcast type — it can carry many ids per variant because a single
//! "trash these N items" command should fan out to subscribers as one
//! event. The outbox stores one row per affected entity, so by the time
//! a row is read back for push, "single id" is a fact.
//!
//! `OutboxMutation` makes that fact structural. The push handlers
//! pattern-match on it directly, which removes the `ids[0]` indexing in
//! `push.rs` that previously relied on a serialisation-layer invariant.
//!
//! Round-trip: `Mutation::to_outbox_rows()` → DB → `OutboxMutation::from_row()`.

use std::path::PathBuf;

use crate::library::album::AlbumId;
use crate::library::faces::PersonId;
use crate::library::media::MediaId;
use crate::library::mutation::OutboxRow;

/// A mutation as reconstructed from a single outbox row.
///
/// One variant per (entity_type, action) pair recognised by the push
/// manager. Multi-id [`Mutation`](crate::library::mutation::Mutation)
/// variants serialise into N outbox rows; each row deserialises into
/// one of these.
#[derive(Debug, Clone)]
pub enum OutboxMutation {
    AssetImported {
        id: MediaId,
        file_path: PathBuf,
    },
    AssetFavorited {
        id: MediaId,
        favorite: bool,
    },
    AssetTrashed {
        id: MediaId,
    },
    AssetRestored {
        id: MediaId,
    },
    AssetDeleted {
        id: MediaId,
        external_id: Option<String>,
    },

    AlbumCreated {
        id: AlbumId,
        name: String,
    },
    AlbumRenamed {
        id: AlbumId,
        name: String,
    },
    AlbumDeleted {
        id: AlbumId,
        external_id: Option<String>,
    },
    AlbumMediaAdded {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },
    AlbumMediaRemoved {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },

    PersonRenamed {
        id: PersonId,
        name: String,
    },
    PersonHidden {
        id: PersonId,
        hidden: bool,
    },
}

impl OutboxMutation {
    /// Reconstruct from an outbox row. Returns `None` for unknown
    /// (entity_type, action) pairs — the push manager logs and skips.
    pub fn from_row(row: &OutboxRow) -> Option<Self> {
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
                Some(Self::AssetImported {
                    id: MediaId::new(row.entity_id.clone()),
                    file_path: PathBuf::from(path),
                })
            }
            ("asset", "favorite") => Some(Self::AssetFavorited {
                id: MediaId::new(row.entity_id.clone()),
                favorite: true,
            }),
            ("asset", "unfavorite") => Some(Self::AssetFavorited {
                id: MediaId::new(row.entity_id.clone()),
                favorite: false,
            }),
            ("asset", "trash") => Some(Self::AssetTrashed {
                id: MediaId::new(row.entity_id.clone()),
            }),
            ("asset", "restore") => Some(Self::AssetRestored {
                id: MediaId::new(row.entity_id.clone()),
            }),
            ("asset", "delete") => {
                let p = json();
                let external_id = p["external_id"].as_str().map(String::from);
                Some(Self::AssetDeleted {
                    id: MediaId::new(row.entity_id.clone()),
                    external_id,
                })
            }
            ("album", "create") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Self::AlbumCreated {
                    id: AlbumId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("album", "rename") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Self::AlbumRenamed {
                    id: AlbumId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("album", "delete") => {
                let p = json();
                let external_id = p["external_id"].as_str().map(String::from);
                Some(Self::AlbumDeleted {
                    id: AlbumId::from_raw(row.entity_id.clone()),
                    external_id,
                })
            }
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
                Some(Self::AlbumMediaAdded {
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
                Some(Self::AlbumMediaRemoved {
                    album_id: AlbumId::from_raw(row.entity_id.clone()),
                    media_ids,
                })
            }
            ("person", "rename") => {
                let p = json();
                let name = p["name"].as_str().unwrap_or("").to_string();
                Some(Self::PersonRenamed {
                    id: PersonId::from_raw(row.entity_id.clone()),
                    name,
                })
            }
            ("person", "hide") => {
                let p = json();
                let hidden = p["hidden"].as_bool().unwrap_or(false);
                Some(Self::PersonHidden {
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
    use crate::library::mutation::Mutation;

    /// Every multi-id `Mutation` produces N rows that each deserialise
    /// into one `OutboxMutation` — the M7 invariant made structural.
    #[test]
    fn multi_id_mutation_round_trips_to_single_id_outbox_mutations() {
        let m = Mutation::AssetTrashed {
            ids: vec![
                MediaId::new("a".into()),
                MediaId::new("b".into()),
                MediaId::new("c".into()),
            ],
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows.len(), 3);

        let mutations: Vec<OutboxMutation> = rows
            .iter()
            .map(|r| OutboxMutation::from_row(r).expect("row deserialises"))
            .collect();

        assert!(matches!(
            &mutations[0],
            OutboxMutation::AssetTrashed { id } if id.as_str() == "a"
        ));
        assert!(matches!(
            &mutations[1],
            OutboxMutation::AssetTrashed { id } if id.as_str() == "b"
        ));
        assert!(matches!(
            &mutations[2],
            OutboxMutation::AssetTrashed { id } if id.as_str() == "c"
        ));
    }

    #[test]
    fn unknown_entity_action_returns_none() {
        let row = OutboxRow {
            entity_type: "unknown".into(),
            entity_id: "x".into(),
            action: "nope".into(),
            payload: None,
        };
        assert!(OutboxMutation::from_row(&row).is_none());
    }

    #[test]
    fn favorite_unfavorite_actions_map_to_bool() {
        let fav = OutboxRow {
            entity_type: "asset".into(),
            entity_id: "a".into(),
            action: "favorite".into(),
            payload: None,
        };
        let unfav = OutboxRow {
            entity_type: "asset".into(),
            entity_id: "a".into(),
            action: "unfavorite".into(),
            payload: None,
        };
        assert!(matches!(
            OutboxMutation::from_row(&fav),
            Some(OutboxMutation::AssetFavorited { favorite: true, .. })
        ));
        assert!(matches!(
            OutboxMutation::from_row(&unfav),
            Some(OutboxMutation::AssetFavorited {
                favorite: false,
                ..
            })
        ));
    }

    #[test]
    fn asset_deleted_carries_external_id() {
        let row = OutboxRow {
            entity_type: "asset".into(),
            entity_id: "local-1".into(),
            action: "delete".into(),
            payload: Some(r#"{"external_id":"ext-1"}"#.into()),
        };
        let m = OutboxMutation::from_row(&row).unwrap();
        match m {
            OutboxMutation::AssetDeleted { id, external_id } => {
                assert_eq!(id.as_str(), "local-1");
                assert_eq!(external_id.as_deref(), Some("ext-1"));
            }
            _ => panic!("expected AssetDeleted"),
        }
    }

    /// Every `Mutation` variant produces rows that all deserialise back
    /// into a non-None `OutboxMutation`. Equivalent of the old
    /// `Mutation::from_outbox_row` round-trip test, but routed through
    /// the typed single-id deserialiser.
    #[test]
    fn every_mutation_variant_round_trips_through_outbox() {
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
                items: vec![(MediaId::new("id5".into()), Some("ext-5".into()))],
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
                external_id: Some("ext-a3".into()),
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
            for row in &rows {
                assert!(
                    OutboxMutation::from_row(row).is_some(),
                    "round-trip failed for {mutation:?} -> {row:?}"
                );
            }
        }
    }

    #[test]
    fn album_add_media_preserves_full_id_list() {
        let row = OutboxRow {
            entity_type: "album".into(),
            entity_id: "alb1".into(),
            action: "add_media".into(),
            payload: Some(r#"{"media_ids":["m1","m2","m3"]}"#.into()),
        };
        let m = OutboxMutation::from_row(&row).unwrap();
        match m {
            OutboxMutation::AlbumMediaAdded {
                album_id,
                media_ids,
            } => {
                assert_eq!(album_id.as_str(), "alb1");
                assert_eq!(media_ids.len(), 3);
                assert_eq!(media_ids[0].as_str(), "m1");
            }
            _ => panic!("expected AlbumMediaAdded"),
        }
    }
}
