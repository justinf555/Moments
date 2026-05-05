-- Index media.external_id so the sync upsert path doesn't full-scan
-- the table on every asset. Several queries hit external_id per upsert:
--   - SELECT imported_at WHERE id = ? OR external_id = ? (added in #614)
--   - DELETE FROM media WHERE external_id = ? AND id != ? (#610)
--   - SELECT id WHERE external_id = ?                     (#626)
-- Each runs once per asset during pull-sync, so a 10k-photo library
-- doing a full sync was previously paying tens of thousands of full
-- table scans.
--
-- The index is UNIQUE over non-NULL values: an Immich UUID identifies
-- one server-side asset, so it must map to at most one local row.
-- Locally-imported assets that haven't been pushed yet have NULL
-- external_id and are unaffected (SQLite treats NULLs as distinct in
-- unique indexes, so any number of NULL rows are allowed). Issue #626
-- needed this guarantee: the new "look up local MediaId by external_id,
-- else generate fresh" handler logic is a check, not a constraint, so
-- without DB-level uniqueness any concurrent or out-of-order path
-- could silently produce two rows sharing one Immich UUID.
CREATE UNIQUE INDEX IF NOT EXISTS idx_media_external_id
    ON media(external_id) WHERE external_id IS NOT NULL;
