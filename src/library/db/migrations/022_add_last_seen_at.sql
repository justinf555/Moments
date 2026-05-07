-- Issue #628: heartbeat-based reset reconciliation.
--
-- Each reconciled row gains a `last_seen_at` (unix seconds) that's
-- bumped on every successful server interaction:
--   - Pull side: AssetV1 / AssetDeleteV1 / AlbumV1 / AlbumDeleteV1 /
--     PersonV1 / AssetFaceV1 handlers update the corresponding row.
--   - Push side: external_id stamping after upload, and any
--     server-confirmed write-through (favorite, restore, album
--     mutation) bumps the affected row.
--
-- On a reset cycle (`SyncResetV1` → ... → `SyncCompleteV1`), rows
-- whose `last_seen_at` is older than the reset's checkpoint are
-- treated as deleted server-side and removed locally. For media and
-- albums the sweep is gated on `external_id IS NOT NULL`, which
-- excludes locally-imported rows that have never been pushed (they
-- don't belong to the server's namespace and aren't candidates for
-- server-side orphaning). People and asset_faces don't have a
-- locally-only counterpart, so the gate doesn't apply there.
--
-- Existing rows are backfilled to `0`. On the first post-migration
-- reset cycle the stream re-emits every live row and bumps it past
-- the checkpoint before `SyncCompleteV1` runs the sweep, so the
-- literal backfill value is immaterial as long as it's ≤ that
-- checkpoint. Local-only rows survive via the external_id filter.

ALTER TABLE media       ADD COLUMN last_seen_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE albums      ADD COLUMN last_seen_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE people      ADD COLUMN last_seen_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE asset_faces ADD COLUMN last_seen_at INTEGER NOT NULL DEFAULT 0;
