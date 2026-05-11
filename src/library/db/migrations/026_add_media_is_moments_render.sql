-- Issue #224 Phase C: mark assets that are Moments-rendered children
-- of a stack. Used by:
--   * the §8.2 grid filter — when a stack contains a Moments-render
--     member, surface the *other* sibling (the original) instead of
--     the server's stack primary, so the user sees the artifact they
--     can still edit (the rendered child is a flat JPEG).
--   * Phase D recovery — eager tag-based discovery on first sync can
--     enumerate Moments-rendered assets and rebuild local edits rows
--     from XMP.
--
-- Defaults to 0 for all existing rows. The save flow sets the column
-- to 1 when inserting the new media row for a rendered output; the
-- pull side toggles it based on the asset's `tags[]` containing the
-- well-known `moments-edit` tag (see Phase C tag handling).

ALTER TABLE media ADD COLUMN is_moments_render INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_media_is_moments_render ON media(is_moments_render) WHERE is_moments_render = 1;
