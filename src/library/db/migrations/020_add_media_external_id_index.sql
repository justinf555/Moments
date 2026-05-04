-- Index media.external_id so the sync upsert path doesn't full-scan
-- the table on every asset. Two queries hit external_id per upsert:
--   - SELECT imported_at WHERE id = ? OR external_id = ? (added in #614)
--   - DELETE FROM media WHERE external_id = ? AND id != ? (#610)
-- Both are run once per asset during pull-sync, so a 10k-photo library
-- doing a full sync was previously paying 20k full table scans.
CREATE INDEX IF NOT EXISTS idx_media_external_id ON media(external_id);
