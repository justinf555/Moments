ALTER TABLE media ADD COLUMN orientation INTEGER NOT NULL DEFAULT 1;
ALTER TABLE media ADD COLUMN media_type  INTEGER NOT NULL DEFAULT 0;  -- 0=image  1=video

CREATE TABLE media_metadata (
    media_id     TEXT NOT NULL PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
    camera_make  TEXT,
    camera_model TEXT,
    lens_model   TEXT,
    aperture     REAL,
    shutter_str  TEXT,
    iso          INTEGER,
    focal_length REAL,
    gps_lat      REAL,
    gps_lon      REAL,
    gps_alt      REAL,
    color_space  TEXT
);
