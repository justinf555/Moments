CREATE TABLE upload_queue (
    file_path  TEXT    PRIMARY KEY NOT NULL,
    sha1_hash  TEXT,
    status     INTEGER NOT NULL DEFAULT 0,  -- 0=pending, 1=completed, 2=failed, 3=duplicate
    error_msg  TEXT,
    created_at INTEGER NOT NULL
);
