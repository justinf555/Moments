// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saving and reverting pixel edits for Immich: the rendered sibling
//! asset, its XMP recovery block, and the stack/tag mutation sequence.
//! See `docs/design-immich-edit-sync.md` (Phase C).
//!
//! Atomicity (#709): every database write of a save or a revert —
//! including the outbox rows — runs in one transaction, and events are
//! emitted only after it commits. The rendered file is written before
//! the transaction and removed again if it fails; files of a replaced
//! render are removed only after the commit. A leftover file with no row
//! is harmless; a row with no file is not.

use std::path::PathBuf;

use tracing::warn;

use super::db::Tx;
use super::editing;
use super::error::LibraryError;
use super::media::{MediaEvent, MediaId, MediaRecord};
use super::mutation::Mutation;
use super::Library;

/// Side effects of a committed save/revert transaction that must wait
/// for the commit: events to emit and replaced render files to remove.
#[derive(Default)]
struct Committed {
    events: Vec<MediaEvent>,
    deleted_ids: Vec<MediaId>,
    deleted_paths: Vec<(MediaId, PathBuf)>,
}

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
    ///
    /// Atomic (#709): on error the library is left exactly as it was —
    /// the previous render (if any) and its outbox state untouched, no
    /// new row, no new outbox entry, and the new render file removed.
    pub async fn save_pixel_edit(
        &self,
        original_id: &MediaId,
        state: &editing::EditState,
        rendered_bytes: Vec<u8>,
    ) -> Result<(), LibraryError> {
        use crate::library::media::MediaType;
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
        // shard directories as needed. The file goes first: if the
        // transaction below fails we remove it again.
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

        let mut tx = self.db.begin().await?;
        let result = self
            .save_pixel_edit_in_tx(&mut tx, &original, state, &rendered_record)
            .await;
        match finish_tx(tx, result, "save_pixel_edit").await {
            Ok(committed) => {
                self.finish_committed(committed).await;
                Ok(())
            }
            Err(e) => {
                // No row points at the new render — remove its file.
                if let Err(rm) = tokio::fs::remove_file(&abs_path).await {
                    warn!(
                        path = %abs_path.display(),
                        error = %rm,
                        "save_pixel_edit: could not remove render after failed save"
                    );
                }
                Err(e)
            }
        }
    }

    /// The database half of [`Self::save_pixel_edit`] (steps 2–6), on
    /// one transaction. Outbox order: prior-render cleanup → import →
    /// stack → tag.
    ///
    /// Every read and write goes through `tx` (see [`Database::begin`]).
    ///
    /// [`Database::begin`]: crate::library::db::Database::begin
    async fn save_pixel_edit_in_tx(
        &self,
        tx: &mut Tx,
        original: &MediaRecord,
        state: &editing::EditState,
        rendered_record: &MediaRecord,
    ) -> Result<Committed, LibraryError> {
        let original_id = &original.id;
        let rendered_id = rendered_record.id.clone();
        let mut committed = Committed::default();

        // The XMP block and the StackCreated row were built from the
        // original as read before the transaction. Re-check it under the
        // write lock: if it was deleted, or its server id or hash changed
        // in between, fail rather than queue rows for a stale original.
        let expected = (original.external_id.clone(), original.content_hash.clone());
        if self.media.sync_identity_in_tx(tx, original_id).await? != Some(expected) {
            return Err(LibraryError::Runtime(format!(
                "save_pixel_edit: original {original_id} was deleted or re-synced during the save"
            )));
        }

        // If we're replacing an existing rendered sibling, enqueue its
        // revert sequence BEFORE the new upload so the order in the
        // outbox is sensible: untag → remove from stack → delete →
        // (then the new) import → stack → tag.
        //
        // `delete_permanently_in_tx` records `AssetDeleted` (matching
        // design §7.2 step 6c). The `_no_record` sync path skips the
        // recorder, which would orphan the prior rendered asset on
        // Immich forever — see Phase C code review item #1.
        if let Some(prev_rendered) = self
            .editing
            .server_rendered_asset_id_in_tx(tx, original_id)
            .await?
        {
            self.delete_rendered_in_tx(tx, &prev_rendered, &mut committed)
                .await?;
        }

        // Insert the rendered media row and record AssetImported; the
        // push manager picks it up next drain cycle, uploads the file,
        // and stamps external_id.
        committed.events.extend(
            self.media
                .insert_media_in_tx(tx, rendered_record, None)
                .await?,
        );

        // Persist the edit state and the rendered pointer.
        self.editing
            .upsert_edit_state_no_record_in_tx(tx, original_id, state)
            .await?;
        self.editing
            .set_server_rendered_asset_id_in_tx(tx, original_id, Some(&rendered_id))
            .await?;

        // Stack + tag mutations — these depend on the rendered asset
        // having a server-side external_id. The push handler retries
        // via the outbox backoff until AssetImported drains.
        self.recorder
            .record(
                tx,
                &Mutation::StackCreated {
                    rendered_asset_id: rendered_id.clone(),
                    original_asset_id: original_id.clone(),
                },
            )
            .await?;
        self.recorder
            .record(tx, &Mutation::AssetTaggedMomentsEdit { id: rendered_id })
            .await?;

        Ok(committed)
    }

    /// Revert any edit on `original_id`: clear local edit state, and
    /// — if a rendered sibling was previously uploaded — enqueue the
    /// stack/tag/delete sequence to clean it up server-side. Phase C §7.3.
    ///
    /// Records `AssetEditsCleared` last so the geometric-only `/edits`
    /// state is also wiped (no-op if no such state was ever pushed).
    ///
    /// Atomic (#709): every write commits together; the rendered file
    /// is removed only after the commit.
    pub async fn revert_edit(&self, original_id: &MediaId) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        let result = self.revert_edit_in_tx(&mut tx, original_id).await;
        let committed = finish_tx(tx, result, "revert_edit").await?;
        self.finish_committed(committed).await;
        Ok(())
    }

    async fn revert_edit_in_tx(
        &self,
        tx: &mut Tx,
        original_id: &MediaId,
    ) -> Result<Committed, LibraryError> {
        let mut committed = Committed::default();
        let prev_rendered = self
            .editing
            .server_rendered_asset_id_in_tx(tx, original_id)
            .await?;

        if let Some(rendered) = prev_rendered.as_ref() {
            self.enqueue_rendered_cleanup(tx, rendered).await?;
        }
        // Delete the local edits row; the render pointer lives on that
        // row, so it goes with it.
        self.editing
            .delete_edit_state_no_record_in_tx(tx, original_id)
            .await?;

        if let Some(rendered) = prev_rendered {
            // The local rendered media row goes too, recording
            // AssetDeleted so the push handler removes it from Immich.
            // Its file is removed after the commit.
            self.delete_media_in_tx(tx, &rendered, &mut committed)
                .await?;
        }

        // Belt-and-braces: also clear any server-side /edits action
        // list (Phase B path). DELETE is idempotent on v2.7.5.
        self.recorder
            .record(
                tx,
                &Mutation::AssetEditsCleared {
                    id: original_id.clone(),
                },
            )
            .await?;
        Ok(committed)
    }

    /// Enqueue the cleanup tuple for a rendered sibling and delete its
    /// row, deferring file removal to [`Self::finish_committed`].
    async fn delete_rendered_in_tx(
        &self,
        tx: &mut Tx,
        rendered_id: &MediaId,
        committed: &mut Committed,
    ) -> Result<(), LibraryError> {
        self.enqueue_rendered_cleanup(tx, rendered_id).await?;
        self.delete_media_in_tx(tx, rendered_id, committed).await
    }

    /// Delete one media row (recording `AssetDeleted`) on `tx`, and note
    /// its files and events for after the commit.
    async fn delete_media_in_tx(
        &self,
        tx: &mut Tx,
        id: &MediaId,
        committed: &mut Committed,
    ) -> Result<(), LibraryError> {
        let ids = std::slice::from_ref(id);
        // Collected before the delete — afterwards the row is gone.
        committed
            .deleted_paths
            .extend(self.media.collect_original_paths_in_tx(tx, ids).await?);
        committed
            .events
            .extend(self.media.delete_permanently_in_tx(tx, ids).await?);
        committed.deleted_ids.push(id.clone());
        Ok(())
    }

    /// After a commit: emit the media events and remove deleted files.
    async fn finish_committed(&self, committed: Committed) {
        self.media.emit_committed(committed.events);
        self.cleanup_files(&committed.deleted_ids, &committed.deleted_paths)
            .await;
    }

    /// Helper: enqueue the StackMemberRemoved + AssetUntaggedMomentsEdit
    /// mutations for a rendered child. Stack id is looked up from the
    /// child's `media.stack_id`; if null (StackCreated hasn't drained
    /// yet) the stack-remove step is skipped — the rendered's deletion
    /// alone is enough since Immich auto-deletes the stack at < 2
    /// members.
    async fn enqueue_rendered_cleanup(
        &self,
        tx: &mut Tx,
        rendered_id: &MediaId,
    ) -> Result<(), LibraryError> {
        if let Some(stack_id) = self.media.stack_id_in_tx(tx, rendered_id).await? {
            self.recorder
                .record(
                    tx,
                    &Mutation::StackMemberRemoved {
                        stack_id,
                        asset_id: rendered_id.clone(),
                    },
                )
                .await?;
        }
        self.recorder
            .record(
                tx,
                &Mutation::AssetUntaggedMomentsEdit {
                    id: rendered_id.clone(),
                },
            )
            .await?;
        Ok(())
    }
}

/// Commit `tx` if `result` is `Ok`, otherwise roll it back explicitly
/// (logging a rollback failure) and return the original error.
///
/// Both orchestrators end through here so they handle rollback the same
/// way. Dropping an uncommitted `Tx` would also roll back; doing it
/// explicitly makes the failure visible in the log.
async fn finish_tx<T>(
    tx: Tx,
    result: Result<T, LibraryError>,
    op: &'static str,
) -> Result<T, LibraryError> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(e) => {
            if let Err(rb) = tx.rollback().await {
                warn!(op, error = %rb, "rollback failed");
            }
            Err(e)
        }
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

    // ── Atomicity (#709) ────────────────────────────────────────────

    use crate::library::db::Database;
    use crate::library::media::MediaEvent;
    use crate::library::recorder::tests::FailingRecorder;
    use crate::library::recorder::MutationRecorder;
    use crate::library::tests::open_test_library_with;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Where a failure is injected.
    #[derive(Debug, Clone, Copy)]
    enum Fault {
        /// The recorder fails after writing the rows for a mutation
        /// whose outbox action is this.
        Recorder(&'static str),
        /// A trigger aborts this statement (a repository failure).
        Sql(&'static str),
    }

    /// Everything a save/revert can change, for old-vs-new comparisons.
    #[derive(Debug, PartialEq)]
    struct Snapshot {
        media: Vec<(String, Option<String>)>,
        edit: Option<(String, Option<String>)>,
        outbox: Vec<(String, String)>,
        files: Vec<std::path::PathBuf>,
    }

    async fn snapshot(db: &Database, original: &MediaId, originals: &std::path::Path) -> Snapshot {
        let media = sqlx::query_as("SELECT id, stack_id FROM media ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
        let edit = sqlx::query_as(
            "SELECT edit_json, server_rendered_asset_id FROM edits WHERE media_id = ?",
        )
        .bind(original.as_str())
        .fetch_optional(db.pool())
        .await
        .unwrap();
        let outbox = sqlx::query_as("SELECT entity_id, action FROM sync_outbox ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
        let mut files = Vec::new();
        let mut stack = vec![originals.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        Snapshot {
            media,
            edit,
            outbox,
            files,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        library: Library,
        db: Database,
        originals: std::path::PathBuf,
        original_id: MediaId,
        /// The render from the first (successful) save.
        rendered_id: MediaId,
        /// While set, the recorder fails on `fail_action`.
        armed: Arc<AtomicBool>,
    }

    /// A synced original with one committed pixel edit whose render is
    /// stacked under it (so a cleanup records `StackMemberRemoved` too),
    /// all recorded through a real outbox writer. The recorder fails
    /// only once `armed` is set, on mutations whose outbox action is
    /// `fail_action`.
    async fn fixture(fail_action: Option<&'static str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let bundle = fresh_bundle(dir.path());
        let originals = bundle.originals.clone();
        let armed = Arc::new(AtomicBool::new(false));
        let armed_in = Arc::clone(&armed);
        let (library, db) = open_test_library_with(bundle, move |db| {
            Arc::new(FailingRecorder::new(db, move |m| {
                armed_in.load(Ordering::SeqCst)
                    && fail_action
                        .is_some_and(|action| m.to_outbox_rows().iter().any(|r| r.action == action))
            })) as Arc<dyn MutationRecorder>
        })
        .await;

        let original_id = MediaId::new("orig".into());
        let mut rec = crate::library::db::test_helpers::test_record(original_id.clone());
        rec.relative_path = "orig.jpg".into();
        rec.external_id = Some("immich-orig".into());
        rec.content_hash = Some("hash-orig".into());
        library.media().insert_media(&rec).await.unwrap();
        std::fs::create_dir_all(&originals).unwrap();
        std::fs::write(originals.join("orig.jpg"), b"original").unwrap();

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

        // As if StackCreated had drained: the render is the stack
        // primary (Immich convention) and both rows point at it.
        let media = library.media();
        media
            .ensure_stack_stub("stack-1", &rendered_id, 0)
            .await
            .unwrap();
        media
            .set_media_stack_id(&rendered_id, "stack-1")
            .await
            .unwrap();
        media
            .set_media_stack_id(&original_id, "stack-1")
            .await
            .unwrap();

        Fixture {
            _dir: dir,
            library,
            db,
            originals,
            original_id,
            rendered_id,
            armed,
        }
    }

    impl Fixture {
        async fn snapshot(&self) -> Snapshot {
            snapshot(&self.db, &self.original_id, &self.originals).await
        }

        async fn inject(&self, fault: Fault) {
            self.armed.store(true, Ordering::SeqCst);
            if let Fault::Sql(trigger) = fault {
                sqlx::query(trigger).execute(self.db.pool()).await.unwrap();
            }
        }
    }

    async fn drain_media(rx: &mut tokio::sync::mpsc::UnboundedReceiver<MediaEvent>) -> usize {
        let mut n = 0;
        while let Ok(Some(_)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            n += 1;
        }
        n
    }

    fn trigger(name: &str, when: &str) -> &'static str {
        Box::leak(
            format!("CREATE TRIGGER {name} {when} BEGIN SELECT RAISE(ABORT, 'injected'); END")
                .into_boxed_str(),
        )
    }

    /// Every step of a second save (which also replaces the first
    /// render) can fail; each failure leaves the old state untouched,
    /// removes the new render file and emits nothing.
    #[tokio::test]
    async fn save_pixel_edit_failure_at_any_step_leaves_old_state() {
        let faults = [
            Fault::Recorder("remove_member"),
            Fault::Recorder("untag_moments_edit"),
            Fault::Recorder("delete"),
            Fault::Recorder("import"),
            Fault::Recorder("create"),
            Fault::Recorder("tag_moments_edit"),
            Fault::Sql(trigger("fail_media_delete", "BEFORE DELETE ON media")),
            Fault::Sql(trigger("fail_media_insert", "BEFORE INSERT ON media")),
            Fault::Sql(trigger(
                "fail_edit_upsert",
                "BEFORE UPDATE OF edit_json ON edits",
            )),
            Fault::Sql(trigger(
                "fail_render_pointer",
                "BEFORE UPDATE OF server_rendered_asset_id ON edits \
                 WHEN NEW.server_rendered_asset_id IS NOT NULL",
            )),
        ];
        for fault in faults {
            let action = match fault {
                Fault::Recorder(a) => Some(a),
                Fault::Sql(_) => None,
            };
            let fx = fixture(action).await;
            let before = fx.snapshot().await;
            let mut rx = fx.library.media().subscribe();
            fx.inject(fault).await;

            let mut state = editing::EditState::default();
            state.exposure.brightness = -0.5;
            let result = fx
                .library
                .save_pixel_edit(&fx.original_id, &state, rendered_jpeg())
                .await;

            assert!(result.is_err(), "{fault:?}: save must fail");
            assert_eq!(fx.snapshot().await, before, "{fault:?}: old state kept");
            assert_eq!(drain_media(&mut rx).await, 0, "{fault:?}: no events");
        }
    }

    /// Every step of a revert can fail; each failure leaves the edit,
    /// the render row and file, and the outbox as they were.
    #[tokio::test]
    async fn revert_edit_failure_at_any_step_leaves_old_state() {
        let faults = [
            Fault::Recorder("remove_member"),
            Fault::Recorder("untag_moments_edit"),
            Fault::Recorder("delete"),
            Fault::Recorder("clear_edits"),
            Fault::Sql(trigger("fail_edit_delete", "BEFORE DELETE ON edits")),
            Fault::Sql(trigger("fail_media_delete", "BEFORE DELETE ON media")),
        ];
        for fault in faults {
            let action = match fault {
                Fault::Recorder(a) => Some(a),
                Fault::Sql(_) => None,
            };
            let fx = fixture(action).await;
            let before = fx.snapshot().await;
            let mut rx = fx.library.media().subscribe();
            fx.inject(fault).await;

            let result = fx.library.revert_edit(&fx.original_id).await;

            assert!(result.is_err(), "{fault:?}: revert must fail");
            assert_eq!(fx.snapshot().await, before, "{fault:?}: old state kept");
            assert_eq!(drain_media(&mut rx).await, 0, "{fault:?}: no events");
        }
    }

    /// The happy path of a replacing save: the outbox order is cleanup →
    /// import → stack → tag, the old render's row and file are gone and
    /// the new ones are in place, and events fire after the commit.
    #[tokio::test]
    async fn save_pixel_edit_replacing_render_commits_in_outbox_order() {
        let fx = fixture(None).await;
        let before_outbox = fx.snapshot().await.outbox.len();
        let old_file = fx
            .originals
            .join(crate::library::media::sharded_original_relative(
                &fx.rendered_id,
            ));
        assert!(old_file.exists());
        let mut rx = fx.library.media().subscribe();

        let mut state = editing::EditState::default();
        state.exposure.brightness = -0.5;
        fx.library
            .save_pixel_edit(&fx.original_id, &state, rendered_jpeg())
            .await
            .unwrap();

        let new_id = fx
            .library
            .editing()
            .server_rendered_asset_id(&fx.original_id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(new_id, fx.rendered_id);
        assert!(!old_file.exists(), "replaced render file removed");
        assert!(fx
            .originals
            .join(crate::library::media::sharded_original_relative(&new_id))
            .exists());
        assert!(fx
            .library
            .media()
            .get_media_item(&fx.rendered_id)
            .await
            .unwrap()
            .is_none());

        let after = fx.snapshot().await;
        let new_rows: Vec<(&str, &str)> = after.outbox[before_outbox..]
            .iter()
            .map(|(id, action)| (id.as_str(), action.as_str()))
            .collect();
        let old = fx.rendered_id.as_str();
        let new = new_id.as_str();
        assert_eq!(
            new_rows,
            vec![
                ("stack-1", "remove_member"),
                (old, "untag_moments_edit"),
                (old, "delete"),
                (new, "import"),
                (new, "create"),
                (new, "tag_moments_edit"),
            ]
        );
        // Removed(old), Updated(original freed from the stack), Added(new).
        assert_eq!(drain_media(&mut rx).await, 3);
    }

    /// The original is re-checked inside the transaction: if it was
    /// re-synced (or deleted) after `save_pixel_edit` read it, the save
    /// fails and nothing is written.
    #[tokio::test]
    async fn save_pixel_edit_in_tx_rejects_stale_original() {
        let fx = fixture(None).await;
        let before = fx.snapshot().await;
        let mut stale = fx
            .library
            .media()
            .get_media_record(&fx.original_id)
            .await
            .unwrap()
            .unwrap();
        stale.external_id = Some("immich-old".into());
        let mut rendered = crate::library::db::test_helpers::test_record(MediaId::generate());
        rendered.is_moments_render = true;

        let mut tx = fx.db.begin().await.unwrap();
        let result = fx
            .library
            .save_pixel_edit_in_tx(&mut tx, &stale, &editing::EditState::default(), &rendered)
            .await;
        assert!(result.is_err());
        tx.rollback().await.unwrap();

        let mut gone = stale.clone();
        gone.id = MediaId::new("deleted".into());
        let mut tx = fx.db.begin().await.unwrap();
        assert!(fx
            .library
            .save_pixel_edit_in_tx(&mut tx, &gone, &editing::EditState::default(), &rendered)
            .await
            .is_err());
        tx.rollback().await.unwrap();

        assert_eq!(fx.snapshot().await, before);
    }
}
