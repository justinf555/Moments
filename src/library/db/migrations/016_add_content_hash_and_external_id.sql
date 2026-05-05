-- Content hash for dedup, base64-encoded SHA-1 (28 chars including
-- padding) — same wire format Immich emits on its sync stream so that
-- locally-imported and server-pulled rows can dedup symmetrically.
-- MediaId is now UUID.
ALTER TABLE media ADD COLUMN content_hash TEXT;

-- External ID for Immich server mapping.
ALTER TABLE media ADD COLUMN external_id TEXT;
ALTER TABLE albums ADD COLUMN external_id TEXT;
ALTER TABLE people ADD COLUMN external_id TEXT;
