CREATE TABLE IF NOT EXISTS asset_faces (
    id            TEXT    PRIMARY KEY NOT NULL,
    asset_id      TEXT    NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    person_id     TEXT             REFERENCES people(id) ON DELETE SET NULL,
    image_width   INTEGER NOT NULL DEFAULT 0,
    image_height  INTEGER NOT NULL DEFAULT 0,
    bbox_x1       INTEGER NOT NULL DEFAULT 0,
    bbox_y1       INTEGER NOT NULL DEFAULT 0,
    bbox_x2       INTEGER NOT NULL DEFAULT 0,
    bbox_y2       INTEGER NOT NULL DEFAULT 0,
    source_type   TEXT    NOT NULL DEFAULT 'MachineLearning'
);

CREATE INDEX idx_asset_faces_asset ON asset_faces(asset_id);
CREATE INDEX idx_asset_faces_person ON asset_faces(person_id);
