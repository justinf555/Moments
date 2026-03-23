# Design: Video Import

**Issue:** [#79](https://github.com/justinf555/Moments/issues/79)
**Status:** Proposed
**Date:** 2026-03-24

## Problem

The import pipeline only accepts image files. Video files (MP4, MOV, MKV, etc.) are silently skipped. Users with mixed photo/video libraries can't manage their videos in Moments.

## Current State — What Already Works

The codebase is well-prepared for video:

- **`MediaType::Video = 1`** — already defined in the enum and database schema (`media.media_type` column)
- **Streaming BLAKE3 hash** — uses `BufReader` + streaming hasher, safe for multi-GB video files
- **`FormatRegistry`** — extensible handler dispatch by extension
- **Blocking task isolation** — all decode/hash work on `tokio::task::spawn_blocking`
- **Graceful metadata** — all `ExifInfo` fields are `Option`, missing data doesn't fail import

## What Needs to Change

### Phase 1: Accept Videos Into the Library (minimal)

Accept video files during import, store them with `MediaType::Video`, skip thumbnail generation for now (show a placeholder icon in the grid).

**Changes:**

| File | Change |
|------|--------|
| `src/library/format/registry.rs` | Add `is_video(ext)` method + `VIDEO_EXTENSIONS` constant |
| `src/library/importer.rs` | Determine `MediaType` from extension; accept videos; skip EXIF for videos |
| `src/library/thumbnailer.rs` | Skip thumbnail generation for `MediaType::Video` (mark as Failed) |

**VIDEO_EXTENSIONS:**
```rust
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "3gp",
];
```

**importer.rs changes:**
```rust
// Currently hardcoded:
media_type: MediaType::Image,

// Changed to:
media_type: if formats.is_video(&ext) { MediaType::Video } else { MediaType::Image },
```

**No database changes needed** — `media_type` column already exists.

### Phase 2: Video Thumbnail Generation

Extract a poster frame from the video and generate a WebP thumbnail, same as images.

**Approach: GStreamer** (GNOME-aligned, available in Flatpak runtime)

```rust
pub struct VideoHandler;

impl FormatHandler for VideoHandler {
    fn extensions(&self) -> &[&str] {
        VIDEO_EXTENSIONS
    }

    fn decode(&self, path: &Path) -> Result<DynamicImage, LibraryError> {
        // Use GStreamer pipeline to extract a frame:
        // filesrc → decodebin → videoconvert → appsink (single frame)
        // Convert GStreamer frame buffer to image::DynamicImage
    }
}
```

**Alternative considered: FFmpeg**
- More comprehensive codec support
- Not available in standard GNOME Flatpak runtime
- Would need explicit Flatpak module or bundled binary
- GStreamer is preferred for GNOME integration

**Thumbnail pipeline reuse:**
The existing pipeline works once we have a `DynamicImage`:
```
VideoHandler::decode() → poster frame as DynamicImage
  → resize to 360px (existing GRID_SIZE)
  → encode as WebP (existing pipeline)
  → write atomically (existing pipeline)
```

No orientation correction needed for video (EXIF orientation doesn't apply).

### Phase 3: Video Metadata Extraction

Video metadata (duration, resolution, codec) lives in container headers, not EXIF.

**New file:** `src/library/video_meta.rs`

```rust
pub struct VideoMetaInfo {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub creation_time: Option<i64>,  // Unix timestamp from container
}

pub fn extract_video_metadata(path: &Path) -> VideoMetaInfo {
    // GStreamer Discoverer API:
    // gst_pbutils::Discoverer → discover_uri()
    // Returns duration, stream info, tags
}
```

**Database consideration:**
- `width`/`height` already on `media` table — reuse for video resolution
- `taken_at` — use container creation time or file mtime
- Duration, codec, frame rate — could add to `media_metadata` table or new `video_metadata` table

Recommendation: add a `video_metadata` table (separate from photo EXIF) since the fields are completely different:

```sql
CREATE TABLE video_metadata (
    media_id    TEXT PRIMARY KEY NOT NULL REFERENCES media(id),
    duration_ms INTEGER,
    frame_rate  REAL,
    video_codec TEXT,
    audio_codec TEXT
);
```

## FormatRegistry Changes

### Option A: Extension-Based Media Type (Recommended for Phase 1)

Add a method to determine media type without decoding:

```rust
impl FormatRegistry {
    pub fn is_video(&self, ext: &str) -> bool {
        VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    }

    pub fn media_type(&self, ext: &str) -> Option<MediaType> {
        if self.is_supported(ext) {
            Some(MediaType::Image)
        } else if self.is_video(ext) {
            Some(MediaType::Video)
        } else {
            None
        }
    }
}
```

### Option B: VideoHandler Registered in FormatRegistry (Phase 2)

Register `VideoHandler` alongside image handlers:

```rust
registry.register(Arc::new(StandardHandler));
registry.register(Arc::new(RawHandler));
registry.register(Arc::new(VideoHandler));  // New
```

`decode()` returns the poster frame as `DynamicImage`. The thumbnail pipeline works unchanged.

## Import Pipeline Flow (After All Phases)

```
ImportJob::run(sources)
├── Walk directories → candidate files
├── For each file:
│   ├── Extension check → FormatRegistry.media_type(ext)
│   │   ├── None → skip (unsupported)
│   │   ├── Some(Image) → image pipeline
│   │   └── Some(Video) → video pipeline
│   │
│   ├── Hash (BLAKE3, streaming) — same for both
│   ├── Duplicate check — same for both
│   ├── Copy to originals/ — same for both
│   │
│   ├── [Image] extract_exif() → ExifInfo
│   ├── [Video] extract_video_metadata() → VideoMetaInfo
│   │
│   ├── Insert media row (with correct MediaType)
│   ├── Insert metadata row (image: media_metadata, video: video_metadata)
│   │
│   ├── Spawn ThumbnailJob
│   │   ├── [Image] FormatHandler::decode() → DynamicImage → resize → WebP
│   │   └── [Video] VideoHandler::decode() → poster frame → resize → WebP
│   │
│   └── Emit ImportProgress
└── Emit ImportComplete
```

## Grid Cell Video Indicator

Tracked separately in issue #70. Once video import works:
- Grid cells show a play icon badge for `MediaType::Video`
- Duration overlay (from `video_metadata.duration_ms`)

## Dependencies

### Rust Crates (Phase 2+)

```toml
gstreamer = "0.23"
gstreamer-pbutils = "0.23"   # Discoverer API
gstreamer-app = "0.23"       # AppSink for frame extraction
gstreamer-video = "0.23"     # Video frame conversion
```

### Flatpak

GStreamer core is in `org.gnome.Platform`. May need to verify that required plugins are available:
- `gst-plugins-base` (videoconvert, appsink)
- `gst-plugins-good` (MP4/MOV demuxing, matroska)
- `gst-plugins-bad` or `gst-plugins-ugly` (some codecs)

## Implementation Order

| Phase | Scope | Effort | Depends on |
|-------|-------|--------|-----------|
| 1 | Accept videos, placeholder thumbnail | Small | Nothing |
| 2 | GStreamer poster-frame thumbnails | Medium | GStreamer crate + Flatpak setup |
| 3 | Video metadata extraction + DB table | Medium | GStreamer discoverer |
| 4 | Grid video indicator (issue #70) | Small | Phase 1 |
| 5 | Video playback in viewer (issue #70) | Large | GStreamer or GtkVideo |

Phase 1 can ship independently — users get their videos in the library even without thumbnails or playback.

## Risks

- **GStreamer in Flatpak sandbox**: Frame extraction may require specific plugins not in the base platform. Need to test early.
- **Large video files**: Hash + copy can take seconds for multi-GB files. The streaming hasher handles memory, but progress reporting per-file may be needed.
- **Codec licensing**: Some codecs (H.264, H.265) have patent considerations. GStreamer's plugin split (good/bad/ugly) handles this — we use whatever the platform provides.
