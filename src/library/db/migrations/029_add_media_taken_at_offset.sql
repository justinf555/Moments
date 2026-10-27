-- Issue #677: persist the capture-time UTC offset and repair rows that
-- were imported before #675.
--
-- `taken_at_offset_minutes` is the EXIF `OffsetTimeOriginal` /
-- `OffsetTime` tag in minutes east of UTC, as read from the original at
-- import time. It is recorded alongside `taken_at` (the capture-local
-- wall clock) and never applied to it. NULL means "no offset known":
-- the file has no offset tag, the row came from Immich, or the row was
-- imported before this column existed and has not been examined yet.
--
-- Before #675 the EXIF extractor subtracted the offset, so local rows
-- from cameras that write the tag were filed `offset` minutes early. The
-- one-shot backfill in `library/maintenance/` re-reads the tag, corrects
-- those rows and fills in this column, which also marks the row as done.
ALTER TABLE media ADD COLUMN taken_at_offset_minutes INTEGER;

-- One row per completed one-shot library maintenance task, so a task
-- that must not run twice (e.g. the #677 backfill) can short-circuit.
CREATE TABLE library_maintenance (
    task         TEXT    PRIMARY KEY NOT NULL,
    completed_at INTEGER NOT NULL  -- Unix timestamp (seconds)
);
