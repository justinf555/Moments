// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

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
    ///
    /// Each entry pairs the local ID with its server-side external_id
    /// (captured before the DB delete so the push manager can reference
    /// the server even though the local row is gone).
    AssetDeleted {
        items: Vec<(MediaId, Option<String>)>,
    },

    // ── Album ────────────────────────────────────────────────────────
    /// A new album was created.
    AlbumCreated { id: AlbumId, name: String },

    /// An album was renamed.
    AlbumRenamed { id: AlbumId, name: String },

    /// An album was deleted.
    ///
    /// `external_id` is captured before the DB delete so the push
    /// manager can reference the server even though the local row is gone.
    AlbumDeleted {
        id: AlbumId,
        external_id: Option<String>,
    },

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

    // ── Edits ────────────────────────────────────────────────────────
    /// The local edit state for an asset was updated. Payload-free —
    /// the push handler reads the current `EditState` at drain time
    /// and projects to whatever wire shape the provider needs. This
    /// is **latest-state read**, not coalescing: N rapid saves still
    /// produce N outbox rows and N wire calls, but every call sends
    /// the same final state, so no intermediate edit can leak past a
    /// later save.
    AssetEditsApplied { id: MediaId },

    /// The local edit state for an asset was cleared (revert).
    AssetEditsCleared { id: MediaId },

    // ── Stacks (Phase C, #224) ───────────────────────────────────────
    /// A pixel-adjustment edit was saved: a new rendered asset was
    /// imported and should be stacked with the original on the server.
    /// The push handler waits for both members' `external_id`s to be
    /// stamped before calling `POST /stacks`.
    StackCreated {
        /// Local id of the rendered asset (becomes the stack primary
        /// on Immich; §8.2 swaps this back to the original in the
        /// timeline grid).
        rendered_asset_id: MediaId,
        /// Local id of the original asset.
        original_asset_id: MediaId,
    },

    /// A stack member should be removed on the server (revert flow).
    /// Immich auto-deletes the stack when fewer than 2 members remain;
    /// the surviving sibling's `stackId` clears on the next pull
    /// cycle, so no explicit local cleanup is recorded here.
    StackMemberRemoved { stack_id: String, asset_id: MediaId },

    // ── Tags ──────────────────────────────────────────────────────────
    /// Apply the well-known `moments-edit` tag to a rendered asset.
    /// The push handler creates the tag lazily via PUT /tags on first
    /// use and caches the resolved tag id in memory for the session.
    AssetTaggedMomentsEdit { id: MediaId },

    /// Detach the `moments-edit` tag from an asset (revert flow).
    AssetUntaggedMomentsEdit { id: MediaId },

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

            Mutation::AssetDeleted { items } => items
                .iter()
                .map(|(id, external_id)| {
                    let payload = external_id
                        .as_deref()
                        .map(|eid| serde_json::json!({ "external_id": eid }).to_string());
                    OutboxRow {
                        entity_type: "asset".into(),
                        entity_id: id.as_str().into(),
                        action: "delete".into(),
                        payload,
                    }
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

            Mutation::AlbumDeleted { id, external_id } => {
                let payload = external_id
                    .as_deref()
                    .map(|eid| serde_json::json!({ "external_id": eid }).to_string());
                vec![OutboxRow {
                    entity_type: "album".into(),
                    entity_id: id.as_str().into(),
                    action: "delete".into(),
                    payload,
                }]
            }

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

            Mutation::AssetEditsApplied { id } => vec![OutboxRow {
                entity_type: "asset".into(),
                entity_id: id.as_str().into(),
                action: "apply_edits".into(),
                payload: None,
            }],

            Mutation::AssetEditsCleared { id } => vec![OutboxRow {
                entity_type: "asset".into(),
                entity_id: id.as_str().into(),
                action: "clear_edits".into(),
                payload: None,
            }],

            Mutation::StackCreated {
                rendered_asset_id,
                original_asset_id,
            } => {
                let payload =
                    serde_json::json!({ "original_asset_id": original_asset_id.as_str() })
                        .to_string();
                vec![OutboxRow {
                    entity_type: "stack".into(),
                    entity_id: rendered_asset_id.as_str().into(),
                    action: "create".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::StackMemberRemoved { stack_id, asset_id } => {
                let payload = serde_json::json!({ "asset_id": asset_id.as_str() }).to_string();
                vec![OutboxRow {
                    entity_type: "stack".into(),
                    entity_id: stack_id.clone(),
                    action: "remove_member".into(),
                    payload: Some(payload),
                }]
            }

            Mutation::AssetTaggedMomentsEdit { id } => vec![OutboxRow {
                entity_type: "asset".into(),
                entity_id: id.as_str().into(),
                action: "tag_moments_edit".into(),
                payload: None,
            }],

            Mutation::AssetUntaggedMomentsEdit { id } => vec![OutboxRow {
                entity_type: "asset".into(),
                entity_id: id.as_str().into(),
                action: "untag_moments_edit".into(),
                payload: None,
            }],

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

    // Deserialisation lives in `crate::sync::outbox::OutboxMutation`.
    // The push manager consumes single-id rows directly, so multi-id
    // `Mutation` variants are write-only as far as the outbox is concerned.
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
                items: vec![(MediaId::new("id5".to_string()), Some("ext-5".to_string()))],
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
                external_id: Some("ext-a3".to_string()),
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
            Mutation::AssetEditsApplied {
                id: MediaId::new("id6".to_string()),
            },
            Mutation::AssetEditsCleared {
                id: MediaId::new("id7".to_string()),
            },
            Mutation::StackCreated {
                rendered_asset_id: MediaId::new("rendered-1".to_string()),
                original_asset_id: MediaId::new("orig-1".to_string()),
            },
            Mutation::StackMemberRemoved {
                stack_id: "stk-1".to_string(),
                asset_id: MediaId::new("rendered-1".to_string()),
            },
            Mutation::AssetTaggedMomentsEdit {
                id: MediaId::new("rendered-2".to_string()),
            },
            Mutation::AssetUntaggedMomentsEdit {
                id: MediaId::new("rendered-3".to_string()),
            },
        ];

        assert_eq!(mutations.len(), 18);
        for m in &mutations {
            // Ensure Debug doesn't panic.
            let _ = format!("{m:?}");
        }
    }

    #[test]
    fn asset_edits_applied_serialises_payload_free() {
        let m = Mutation::AssetEditsApplied {
            id: MediaId::new("photo-1".into()),
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_type, "asset");
        assert_eq!(rows[0].entity_id, "photo-1");
        assert_eq!(rows[0].action, "apply_edits");
        assert!(rows[0].payload.is_none());
    }

    #[test]
    fn asset_edits_cleared_serialises_payload_free() {
        let m = Mutation::AssetEditsCleared {
            id: MediaId::new("photo-2".into()),
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "clear_edits");
        assert!(rows[0].payload.is_none());
    }

    #[test]
    fn stack_created_carries_original_id_in_payload() {
        let m = Mutation::StackCreated {
            rendered_asset_id: MediaId::new("rendered".into()),
            original_asset_id: MediaId::new("orig".into()),
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_type, "stack");
        assert_eq!(rows[0].entity_id, "rendered");
        assert_eq!(rows[0].action, "create");
        assert!(rows[0].payload.as_deref().unwrap().contains("orig"));
    }

    #[test]
    fn stack_member_removed_keys_by_stack_id() {
        let m = Mutation::StackMemberRemoved {
            stack_id: "stk-uuid".into(),
            asset_id: MediaId::new("rendered".into()),
        };
        let rows = m.to_outbox_rows();
        assert_eq!(rows[0].entity_type, "stack");
        assert_eq!(rows[0].entity_id, "stk-uuid");
        assert_eq!(rows[0].action, "remove_member");
        assert!(rows[0].payload.as_deref().unwrap().contains("\"rendered\""));
    }

    #[test]
    fn tag_actions_are_payload_free() {
        let tag = Mutation::AssetTaggedMomentsEdit {
            id: MediaId::new("rendered".into()),
        };
        let untag = Mutation::AssetUntaggedMomentsEdit {
            id: MediaId::new("rendered".into()),
        };
        let tag_rows = tag.to_outbox_rows();
        let untag_rows = untag.to_outbox_rows();
        assert_eq!(tag_rows[0].action, "tag_moments_edit");
        assert!(tag_rows[0].payload.is_none());
        assert_eq!(untag_rows[0].action, "untag_moments_edit");
        assert!(untag_rows[0].payload.is_none());
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
            ids: vec![MediaId::new("a".to_string()), MediaId::new("b".to_string())],
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
