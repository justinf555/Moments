-- Unified outbox for pushing local mutations to the Immich server.
-- Replaces the old upload_queue table — file uploads are now just
-- another outbox entry (action = 'import', payload = file path).
DROP TABLE IF EXISTS upload_queue;

CREATE TABLE sync_outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type TEXT    NOT NULL,
    entity_id   TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    payload     TEXT,
    created_at  INTEGER NOT NULL,
    status      INTEGER NOT NULL DEFAULT 0  -- 0=pending, 1=done, 2=failed
);
