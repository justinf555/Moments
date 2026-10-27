// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saving and reverting pixel edits for Immich: the rendered sibling
//! asset, its XMP recovery block, and the stack/tag mutation sequence.
//! See `docs/design-immich-edit-sync.md` (Phase C).

use super::editing;
use super::error::LibraryError;
use super::media::MediaId;
use super::Library;

impl Library {
    /// Save a pixel-adjustment edit: persist the edit state, write the
    /// rendered JPEG (with the Moments XMP block embedded) to the
    /// originals shard, record the upload+stack+tag mutation sequence,
    /// and clean up any previously-rendered sibling. Phase C §7.2.
    ///
    /// The caller produces `rendered_bytes` by running the original
    /// through [`crate::renderer::pipeline::RenderPipeline`] at
    /// `FullRes` with the user's `EditState`, then encoding via
    /// [`crate::renderer::output::to_jpeg`]. XMP injection is handled
    /// here so the original's `external_id` and `content_hash` (needed
    /// for Phase D recovery) don't have to leak into the UI.
    ///
    /// Errors if the original asset hasn't been synced to Immich yet
    /// (no `external_id`) — Phase D recovery depends on that id being
    /// stable, and saving an edit before the original is on the
    /// server would lose recoverability.
    pub async fn save_pixel_edit(
        &self,
        original_id: &MediaId,
        state: &editing::EditState,
        rendered_bytes: Vec<u8>,
    ) -> Result<(), LibraryError> {
        use crate::library::media::{MediaRecord, MediaType};
        use crate::library::mutation::Mutation;
        use crate::renderer::xmp;

        // Look up the original — we need its external_id and content
        // hash for the XMP, and its filename for the rendered output.
        let original = self
            .media
            .get_media_record(original_id)
            .await?
            .ok_or_else(|| {
                LibraryError::Runtime(format!("save_pixel_edit: original {original_id} not found"))
            })?;
        let Some(original_external_id) = original.external_id.as_deref() else {
            return Err(LibraryError::Runtime(format!(
                "save_pixel_edit: original {original_id} has not been uploaded yet"
            )));
        };
        // Phase D fallback recovery (§7.4) uses `originalContentHash`
        // to find the parent when `originalAssetId` no longer resolves.
        // Defaulting to an empty string would resolve to any zero-hash
        // row — fail fast instead.
        let Some(content_hash) = original.content_hash.clone() else {
            return Err(LibraryError::Runtime(format!(
                "save_pixel_edit: original {original_id} has no content_hash — can't embed Phase D recovery fallback"
            )));
        };

        // Build the embedded edit block. `editJson` is the same JSON
        // the local `edits.edit_json` column stores.
        let edit_json = serde_json::to_string(state)
            .map_err(|e| LibraryError::Runtime(format!("serialize EditState: {e}")))?;
        let embedded = xmp::EmbeddedEdit {
            original_asset_id: original_external_id.to_string(),
            original_content_hash: content_hash,
            edit_version: 1,
            rendered_at: chrono::Utc::now(),
            edit_json,
        };
        let xmp_xml = xmp::encode(&embedded);
        let rendered_with_xmp = xmp::inject_xmp(&rendered_bytes, &xmp_xml)
            .map_err(|e| LibraryError::Runtime(format!("inject XMP: {e}")))?;

        // Allocate the rendered MediaId and write the file under the
        // standard sharded originals layout, creating the two-level
        // shard directories as needed.
        let rendered_id = MediaId::generate();
        let relative_path = crate::library::media::sharded_original_relative(&rendered_id);
        let abs_path = self.media.originals_dir().join(&relative_path);
        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LibraryError::Io)?;
        }
        tokio::fs::write(&abs_path, &rendered_with_xmp)
            .await
            .map_err(LibraryError::Io)?;

        // If we're replacing an existing rendered sibling, enqueue its
        // revert sequence BEFORE the new upload so the order in the
        // outbox is sensible: untag → remove from stack → delete →
        // (then the new) import → stack → tag.
        //
        // `delete_permanently` records `AssetDeleted` (matching design
        // §7.2 step 6c). The `_from_sync` variant skips the recorder,
        // which would orphan the prior rendered asset on Immich
        // forever — see Phase C code review item #1.
        if let Some(prev_rendered) = self.editing.server_rendered_asset_id(original_id).await? {
            self.enqueue_rendered_cleanup(&prev_rendered).await?;
            self.delete_permanently(&[prev_rendered]).await?;
        }

        // Insert the rendered media row. `insert_media` records the
        // AssetImported mutation; the push manager picks it up next
        // drain cycle, uploads the file, and stamps external_id.
        let rendered_filename = format!(
            "{}-edit.jpg",
            std::path::Path::new(&original.original_filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("photo")
        );
        let now = chrono::Utc::now().timestamp();
        let rendered_record = MediaRecord {
            id: rendered_id.clone(),
            content_hash: None, // computed lazily on import; not load-bearing for rendered children
            external_id: None,
            relative_path,
            original_filename: rendered_filename,
            file_size: rendered_with_xmp.len() as i64,
            imported_at: now,
            media_type: MediaType::Image,
            taken_at: original.taken_at,
            width: original.width,
            height: original.height,
            orientation: 1, // EditState already accounts for orientation
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
            is_moments_render: true,
        };
        self.media.insert_media(&rendered_record).await?;

        // Persist the edit state and the rendered pointer.
        self.editing
            .upsert_edit_state_no_record(original_id, state)
            .await?;
        self.editing
            .set_server_rendered_asset_id(original_id, Some(&rendered_id))
            .await?;

        // Stack + tag mutations — these depend on the rendered asset
        // having a server-side external_id. The push handler retries
        // via the outbox backoff until AssetImported drains.
        self.recorder
            .record(&Mutation::StackCreated {
                rendered_asset_id: rendered_id.clone(),
                original_asset_id: original_id.clone(),
            })
            .await?;
        self.recorder
            .record(&Mutation::AssetTaggedMomentsEdit { id: rendered_id })
            .await?;

        Ok(())
    }

    /// Revert any edit on `original_id`: clear local edit state, and
    /// — if a rendered sibling was previously uploaded — enqueue the
    /// stack/tag/delete sequence to clean it up server-side. Phase C §7.3.
    ///
    /// Records `AssetEditsCleared` last so the geometric-only `/edits`
    /// state is also wiped (no-op if no such state was ever pushed).
    pub async fn revert_edit(&self, original_id: &MediaId) -> Result<(), LibraryError> {
        use crate::library::mutation::Mutation;
        let prev_rendered = self.editing.server_rendered_asset_id(original_id).await?;

        if let Some(rendered) = prev_rendered.as_ref() {
            self.enqueue_rendered_cleanup(rendered).await?;
        }

        // Delete the local edits row; the render pointer lives on that
        // row, so it goes with it.
        self.editing
            .delete_edit_state_no_record(original_id)
            .await?;

        if let Some(rendered) = prev_rendered {
            // The local rendered media row + file go away too. This
            // records AssetDeleted via `delete_permanently`'s normal
            // path — the push handler removes it from Immich.
            self.delete_permanently(&[rendered]).await?;
        }

        // Belt-and-braces: also clear any server-side /edits action
        // list (Phase B path). DELETE is idempotent on v2.7.5.
        self.recorder
            .record(&Mutation::AssetEditsCleared {
                id: original_id.clone(),
            })
            .await?;
        Ok(())
    }

    /// Helper: enqueue the StackMemberRemoved + AssetUntaggedMomentsEdit
    /// mutations for a rendered child. Stack id is looked up from the
    /// child's `media.stack_id`; if null (StackCreated hasn't drained
    /// yet) the stack-remove step is skipped — the rendered's deletion
    /// alone is enough since Immich auto-deletes the stack at < 2
    /// members.
    async fn enqueue_rendered_cleanup(&self, rendered_id: &MediaId) -> Result<(), LibraryError> {
        use crate::library::mutation::Mutation;
        let item = self.media.get_media_item(rendered_id).await?;
        if let Some(stack_id) = item.as_ref().and_then(|m| m.stack_id.clone()) {
            self.recorder
                .record(&Mutation::StackMemberRemoved {
                    stack_id,
                    asset_id: rendered_id.clone(),
                })
                .await?;
        }
        self.recorder
            .record(&Mutation::AssetUntaggedMomentsEdit {
                id: rendered_id.clone(),
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::mutation;
    use crate::library::tests::{fresh_bundle, open_test_library, open_test_library_with_recorder};

    fn rendered_jpeg() -> Vec<u8> {
        // Hand-roll a 1x1 JPEG via the `image` crate.
        use image::{ImageFormat, RgbImage};
        let img = RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    #[tokio::test]
    async fn save_pixel_edit_requires_original_to_be_synced() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = fresh_bundle(dir.path());
        let library = open_test_library(bundle).await;

        // Insert an original media row WITHOUT external_id.
        let original_id = MediaId::new("orig".into());
        let mut rec = crate::library::db::test_helpers::test_record(original_id.clone());
        rec.relative_path = "orig.jpg".into();
        library.media().insert_media(&rec).await.unwrap();

        let err = library
            .save_pixel_edit(
                &original_id,
                &editing::EditState::default(),
                rendered_jpeg(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("has not been uploaded yet"));
    }

    #[tokio::test]
    async fn save_pixel_edit_writes_render_inserts_row_and_records_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = fresh_bundle(dir.path());
        let originals_dir = bundle.originals.clone();
        let (library, recorder) = open_test_library_with_recorder(bundle).await;

        // Insert a synced original (external_id + content_hash set).
        let original_id = MediaId::new("orig".into());
        let mut rec = crate::library::db::test_helpers::test_record(original_id.clone());
        rec.relative_path = "orig.jpg".into();
        rec.external_id = Some("immich-orig".into());
        rec.content_hash = Some("hash-orig".into());
        rec.original_filename = "IMG_42.jpg".into();
        library.media().insert_media(&rec).await.unwrap();
        recorder.clear();

        let mut state = editing::EditState::default();
        state.exposure.brightness = 0.5;
        library
            .save_pixel_edit(&original_id, &state, rendered_jpeg())
            .await
            .unwrap();

        // Server-rendered pointer is stamped on the edit row.
        let rendered_id = library
            .editing()
            .server_rendered_asset_id(&original_id)
            .await
            .unwrap()
            .expect("rendered id must be stamped");

        // The rendered file exists under the sharded originals layout.
        let rendered_relative = crate::library::media::sharded_original_relative(&rendered_id);
        let rendered_abs = originals_dir.join(&rendered_relative);
        assert!(
            rendered_abs.exists(),
            "rendered file should be on disk at {rendered_abs:?}"
        );
        let bytes = std::fs::read(&rendered_abs).unwrap();
        // XMP marker must be embedded for Phase D recovery.
        assert!(
            crate::renderer::xmp::extract_xmp(&bytes).unwrap().is_some(),
            "rendered JPEG must carry the Moments XMP block"
        );

        // Mutation sequence: AssetImported (from insert_media),
        // StackCreated, AssetTaggedMomentsEdit — recorded in this order.
        let recorded = recorder.snapshot();
        let mut iter = recorded.iter();
        assert!(matches!(
            iter.next(),
            Some(mutation::Mutation::AssetImported { id, .. }) if id == &rendered_id
        ));
        assert!(matches!(
            iter.next(),
            Some(mutation::Mutation::StackCreated { rendered_asset_id, original_asset_id })
                if rendered_asset_id == &rendered_id && original_asset_id == &original_id
        ));
        assert!(matches!(
            iter.next(),
            Some(mutation::Mutation::AssetTaggedMomentsEdit { id }) if id == &rendered_id
        ));
    }

    #[tokio::test]
    async fn revert_edit_clears_state_and_enqueues_rendered_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = fresh_bundle(dir.path());
        let (library, recorder) = open_test_library_with_recorder(bundle).await;

        // Set up: synced original + first pixel edit save → rendered exists.
        let original_id = MediaId::new("orig".into());
        let mut rec = crate::library::db::test_helpers::test_record(original_id.clone());
        rec.relative_path = "orig.jpg".into();
        rec.external_id = Some("immich-orig".into());
        rec.content_hash = Some("hash".into());
        library.media().insert_media(&rec).await.unwrap();
        let mut state = editing::EditState::default();
        state.exposure.brightness = 0.5;
        library
            .save_pixel_edit(&original_id, &state, rendered_jpeg())
            .await
            .unwrap();
        let rendered_id = library
            .editing()
            .server_rendered_asset_id(&original_id)
            .await
            .unwrap()
            .unwrap();
        recorder.clear();

        library.revert_edit(&original_id).await.unwrap();

        // Edit row is gone.
        assert!(library
            .editing()
            .get_edit_state(&original_id)
            .await
            .unwrap()
            .is_none());

        // Rendered media row is gone.
        assert!(library
            .media()
            .get_media_item(&rendered_id)
            .await
            .unwrap()
            .is_none());

        // Mutation sequence: AssetUntaggedMomentsEdit (stack-remove
        // is skipped — no stack_id yet because StackCreated hasn't
        // drained in this unit test), AssetDeleted for the rendered,
        // then AssetEditsCleared for the original. The Phase B
        // `AssetEditsCleared` shows up *after* the cleanup, matching
        // the §7.3 order documented in the design doc.
        let recorded = recorder.snapshot();
        assert!(recorded
            .iter()
            .any(|m| matches!(m, mutation::Mutation::AssetUntaggedMomentsEdit { id } if id == &rendered_id)));
        assert!(recorded
            .iter()
            .any(|m| matches!(m, mutation::Mutation::AssetDeleted { items } if items.iter().any(|(i, _)| i == &rendered_id))));
        assert!(matches!(
            recorded.last(),
            Some(mutation::Mutation::AssetEditsCleared { id }) if id == &original_id
        ));
    }
}
