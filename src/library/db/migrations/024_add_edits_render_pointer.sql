-- Issue #224: link a local edits row to the Immich asset that holds
-- its rendered output. Set when a pixel-adjustment edit is uploaded;
-- null for geometric-only edits (those use the server's edits API).
--
-- xmp_edit_version tracks the schema version of the embedded XMP,
-- allowing forward-compatible parsing of older renders.
--
-- ON DELETE SET NULL on server_rendered_asset_id: if the rendered
-- child asset is removed (revert, orphan sweep), the local edits row
-- survives — it still describes the user's edit; only the server-side
-- render pointer is gone.

ALTER TABLE edits ADD COLUMN server_rendered_asset_id TEXT REFERENCES media(id) ON DELETE SET NULL;
ALTER TABLE edits ADD COLUMN xmp_edit_version INTEGER NOT NULL DEFAULT 1;
