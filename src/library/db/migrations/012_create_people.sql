CREATE TABLE IF NOT EXISTS people (
    id            TEXT    PRIMARY KEY NOT NULL,
    name          TEXT    NOT NULL DEFAULT '',
    birth_date    TEXT,
    is_hidden     INTEGER NOT NULL DEFAULT 0,
    is_favorite   INTEGER NOT NULL DEFAULT 0,
    color         TEXT,
    face_asset_id TEXT,
    face_count    INTEGER NOT NULL DEFAULT 0,
    synced_at     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_people_name ON people(name);
CREATE INDEX idx_people_face_count ON people(face_count DESC);
