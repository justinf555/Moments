-- Index albums.external_id so the sync upsert path doesn't full-scan
-- the table on every album. Mirrors migration 020 for media.
--
-- Since #585, the AlbumV1 sync handler resolves the local AlbumId via
-- `SELECT id FROM albums WHERE external_id = ?` once per pulled album;
-- AlbumDeleteV1 and AlbumToAssetV1 use the same lookup. Without the
-- index every call full-scans the albums table.
--
-- UNIQUE over non-NULL values: an Immich UUID identifies one
-- server-side album, so it must map to at most one local row.
-- Locally-created albums that haven't been pushed yet have NULL
-- external_id and are unaffected (SQLite treats NULLs as distinct in
-- unique indexes). The uniqueness also guarantees the new
-- "look up local id by external_id, else mint fresh" handler logic
-- can't silently produce two rows sharing one Immich UUID.
CREATE UNIQUE INDEX IF NOT EXISTS idx_albums_external_id
    ON albums(external_id) WHERE external_id IS NOT NULL;
