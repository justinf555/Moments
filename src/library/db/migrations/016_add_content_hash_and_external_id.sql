-- Content hash for dedup (BLAKE3 hex, 64 chars). MediaId is now UUID.
ALTER TABLE media ADD COLUMN content_hash TEXT;

-- External ID for Immich server mapping.
ALTER TABLE media ADD COLUMN external_id TEXT;
ALTER TABLE albums ADD COLUMN external_id TEXT;
ALTER TABLE people ADD COLUMN external_id TEXT;
