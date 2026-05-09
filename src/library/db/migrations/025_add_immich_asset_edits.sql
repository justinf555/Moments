-- Issue #224 Phase B: cache the per-action records that arrive on
-- Immich's `/sync/stream` as `SyncAssetEditV1`. The pull handler
-- recomposes an `EditState` from these rows and writes it back to
-- the user-facing `edits` table.
--
-- Provider-specific bookkeeping — only the Immich pull side reads or
-- writes here. The library never touches this table.
--
-- Cascade: when a media row is removed (heartbeat orphan sweep, manual
-- delete) the cached actions go with it. sqlx 0.8 enables
-- PRAGMA foreign_keys per connection, so the cascade fires.

CREATE TABLE immich_asset_edits (
    id              TEXT    PRIMARY KEY NOT NULL,            -- Immich edit UUID
    media_id        TEXT    NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    action          TEXT    NOT NULL,                        -- "crop" | "rotate" | "mirror"
    parameters_json TEXT    NOT NULL,
    sequence        INTEGER NOT NULL,
    last_seen_at    INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_immich_asset_edits_media_id ON immich_asset_edits(media_id);
