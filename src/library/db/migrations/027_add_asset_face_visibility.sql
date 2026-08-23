-- Issue #680: persist the `isVisible` / `deletedAt` fields that
-- `AssetFaceV2` brings down the sync stream.
--
-- Immich distinguishes three face states that we previously flattened
-- into one: live and visible, live but hidden (`isVisible: false`), and
-- soft-deleted (`deletedAt` set, hard delete still arrives later as a
-- separate `AssetFaceDeleteV1`). Faces in the latter two states must
-- stop contributing to a person's grid and `face_count`.
--
-- Soft-deleted rows are filtered, not deleted: that matches the server's
-- own model and survives an un-delete without waiting for a resync.
--
-- Both defaults keep existing rows in the current "live and visible"
-- behaviour, so no backfill and no forced resync.
ALTER TABLE asset_faces ADD COLUMN is_visible INTEGER NOT NULL DEFAULT 1;
ALTER TABLE asset_faces ADD COLUMN deleted_at INTEGER;
