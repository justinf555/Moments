CREATE TABLE edits (
    media_id    TEXT    PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    edit_json   TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL,
    rendered_at INTEGER
);
