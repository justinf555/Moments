-- Issue #224: model Immich's asset stacks locally so the timeline can
-- collapse stacked siblings to the primary, and so push-side stack
-- mutations can be tracked.
--
-- Cascade behaviour:
--   * stacks.primary_asset_id ON DELETE CASCADE — if the primary asset
--     is removed locally (e.g. via the heartbeat orphan sweep), the
--     stack row goes with it; remaining members fall back to NULL via
--     the second FK.
--   * media.stack_id ON DELETE SET NULL — when a stack is deleted,
--     surviving members rejoin the un-stacked timeline rather than
--     being orphaned.

CREATE TABLE stacks (
    id                TEXT    PRIMARY KEY NOT NULL,
    primary_asset_id  TEXT    NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    last_seen_at      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_stacks_primary_asset_id ON stacks(primary_asset_id);

ALTER TABLE media ADD COLUMN stack_id TEXT REFERENCES stacks(id) ON DELETE SET NULL;

CREATE INDEX idx_media_stack_id ON media(stack_id) WHERE stack_id IS NOT NULL;
