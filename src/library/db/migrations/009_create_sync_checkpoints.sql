CREATE TABLE sync_checkpoints (
    entity_type TEXT PRIMARY KEY NOT NULL,
    ack         TEXT NOT NULL
);
