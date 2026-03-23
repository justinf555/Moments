CREATE TABLE media (
    id                TEXT    PRIMARY KEY NOT NULL,  -- BLAKE3 hex (64 chars)
    relative_path     TEXT    NOT NULL UNIQUE,       -- e.g. "2025/01/15/photo.jpg"
    original_filename TEXT    NOT NULL,
    file_size         INTEGER NOT NULL,
    imported_at       INTEGER NOT NULL,              -- Unix timestamp (seconds)
    taken_at          INTEGER,                       -- EXIF datetime (issue #7)
    width             INTEGER,                       -- image dimensions (issue #7)
    height            INTEGER                        -- image dimensions (issue #7)
);
