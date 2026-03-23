CREATE TABLE thumbnails (
    media_id        TEXT    PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    status          INTEGER NOT NULL DEFAULT 0,  -- 0=pending  1=ready  2=failed
    file_path       TEXT,                        -- relative path inside bundle thumbnails/
    generated_at    INTEGER                      -- Unix timestamp
);
