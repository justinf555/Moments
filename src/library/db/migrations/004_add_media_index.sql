-- Supports the reverse-chronological grid query without a full table scan.
-- Items without taken_at (no EXIF) sort to the end via COALESCE(..., 0).
CREATE INDEX idx_media_timeline ON media (taken_at, id);
