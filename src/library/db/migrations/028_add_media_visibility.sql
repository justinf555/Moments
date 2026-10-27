-- Issue #695: persist Immich's asset `visibility` so archived, hidden and
-- locked assets stop showing up in All Photos.
--
-- Values mirror Immich's `AssetVisibility` wire values:
--   'timeline' normal asset
--   'archive'  out of the timeline, still shown in albums and people
--   'hidden'   Immich-internal (e.g. the motion half of a Live Photo)
--   'locked'   locked folder; never listed until #464 adds a locked view
--
-- The default keeps local imports and every existing row in the timeline,
-- so the local import path needs no change.
ALTER TABLE media ADD COLUMN visibility TEXT NOT NULL DEFAULT 'timeline';

-- One-shot actions the Immich pull engine runs before its next stream,
-- then deletes. Keyed by action name so a row can't be queued twice.
CREATE TABLE sync_pending_actions (
    action     TEXT    PRIMARY KEY NOT NULL,
    created_at INTEGER NOT NULL
);

-- Rows synced before this migration all read 'timeline', whatever the
-- server says: a locked photo would stay in All Photos. Sync acks live
-- server-side, so the server won't re-send those assets on its own. Queue
-- a reset of the `AssetV2` ack so the next pull re-delivers every asset
-- once and the upsert writes the real visibility.
--
-- Only libraries that already hold server rows need it; a fresh or local
-- library has nothing stale.
INSERT INTO sync_pending_actions (action, created_at)
SELECT 'restream_assets_visibility', CAST(strftime('%s', 'now') AS INTEGER)
WHERE EXISTS (SELECT 1 FROM media WHERE external_id IS NOT NULL);
