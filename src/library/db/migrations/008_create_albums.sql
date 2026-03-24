CREATE TABLE albums (
    id         TEXT    PRIMARY KEY NOT NULL,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE album_media (
    album_id TEXT    NOT NULL REFERENCES albums(id),
    media_id TEXT    NOT NULL REFERENCES media(id),
    added_at INTEGER NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (album_id, media_id)
);

CREATE INDEX idx_album_media_album ON album_media(album_id, position);
