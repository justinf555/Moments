-- Audit log for sync record processing.
-- Captures timing of each record: when processing started and when it completed
-- (just before being added to the ack batch).
CREATE TABLE IF NOT EXISTS sync_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type TEXT    NOT NULL,
    entity_id   TEXT    NOT NULL,
    action      TEXT    NOT NULL,  -- 'upsert', 'delete', 'skip', 'error'
    started_at  TEXT    NOT NULL,  -- ISO 8601 timestamp
    completed_at TEXT,             -- ISO 8601 timestamp (NULL if failed before completion)
    error_msg   TEXT,              -- error message if action = 'error'
    sync_cycle  TEXT    NOT NULL   -- groups records from the same run_sync call
);

CREATE INDEX IF NOT EXISTS idx_sync_audit_cycle ON sync_audit (sync_cycle);
CREATE INDEX IF NOT EXISTS idx_sync_audit_entity ON sync_audit (entity_type, entity_id);
