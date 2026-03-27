# Design: Non-Destructive Photo Editing (#17)

## Context

Users want to edit photos in Moments without modifying originals. Immich already supports non-destructive editing (crop, rotate, mirror) by creating a new version of the asset. We extend this with exposure, color, and filter adjustments. Design targets Immich backend first, then backports to local.

## Approach

Edit operations stored as a flat JSON parameter model. Edits are applied in real-time via a downscaled preview in the viewer sidebar. On save, the full-res result is rendered and uploaded to Immich as the edited version. Originals are never modified.

---

## 1. Edit State Data Model

New file: `src/library/editing.rs`

Flat parameter model — all values default to identity (0.0 / false / None):

```rust
pub struct EditState {
    pub version: u32,           // Schema version for future-proofing
    pub transforms: TransformState,
    pub exposure: ExposureState,
    pub color: ColorState,
    pub filter: Option<String>, // Preset name, or None
}

pub struct CropRect {
    pub x: f64, pub y: f64,     // Normalized 0.0-1.0
    pub width: f64, pub height: f64,
}

pub struct TransformState {
    pub crop: Option<CropRect>,
    pub rotate_degrees: i32,       // 0, 90, 180, 270
    pub straighten_degrees: f64,   // -45.0 to 45.0
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

pub struct ExposureState {
    // All -1.0 to 1.0, neutral at 0.0
    pub brightness: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub white_balance: f64,
}

pub struct ColorState {
    // All -1.0 to 1.0, neutral at 0.0
    pub saturation: f64,
    pub vibrance: f64,
    pub hue_shift: f64,
    pub temperature: f64,
    pub tint: f64,
}
```

Filters are preset combinations of exposure/color values. Selecting a filter overwrites those sections; the user can tweak sliders on top.

## 2. Database Schema

Migration `014_create_edits.sql`:

```sql
CREATE TABLE edits (
    media_id    TEXT    PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
    edit_json   TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL,
    rendered_at INTEGER
);
```

- One row per asset. Revert = delete row.
- `rendered_at` tracks when last uploaded to Immich. `updated_at > rendered_at` = dirty.

## 3. Library Trait: `LibraryEditing`

```rust
pub trait LibraryEditing: Send + Sync {
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError>;
    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError>;
    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError>;
    async fn render_and_save(&self, id: &MediaId) -> Result<(), LibraryError>;
    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError>;
}
```

Added to the `Library` supertrait in `src/library.rs`. DB methods in new `src/library/db/edits.rs`.

## 4. Rendering Pipeline

New file: `src/library/edit_renderer.rs`

Pure function, no I/O:

```rust
pub fn apply_edits(img: DynamicImage, state: &EditState) -> DynamicImage;
```

Fixed application order:
1. Rotate (90° steps) → Flip → Straighten (freeform)
2. Crop (denormalize coordinates)
3. Brightness → Contrast → Highlights → Shadows → White balance
4. Saturation → Vibrance → Hue shift → Temperature → Tint

**Real-time preview strategy**: downscaled preview (~1200px), full-res only on save.

```
Slider change → debounce (50ms) → apply_edits on spawn_blocking
  → MemoryTexture → set_paintable on gtk::Picture
```

## 5. Immich Integration

**Save**: Load original → apply edits → encode JPEG → `PUT /assets/{id}/original` → update `rendered_at` → regenerate thumbnail

**Revert**: `DELETE /assets/{id}/original` → delete `edits` row → regenerate thumbnail from original

New `ImmichClient` methods:
- `upload_edited_asset(asset_id, rendered_bytes, filename)`
- `revert_asset(asset_id)`

**Note**: Exact Immich API endpoints need verification against the server version. Fallback: upload as new asset linked to original.

## 6. UI: Edit Panel in Viewer Sidebar

Reuses existing `OverlaySplitView` — edit panel replaces info panel when active.

```
EditPanel (gtk::Box vertical, scrollable)
├── Filter row (horizontal scroll, 80px thumbnails)
│   ├── "Original" | "B&W" | "Vintage" | "Warm" | "Cool" | "Vivid"
├── PreferencesGroup "Transform"
│   ├── Rotate 90° CW / CCW buttons
│   ├── Flip H / Flip V toggles
│   ├── Straighten slider
│   └── Crop button (opens overlay)
├── PreferencesGroup "Exposure"
│   ├── Brightness slider
│   ├── Contrast slider
│   ├── Highlights slider
│   ├── Shadows slider
│   └── White Balance slider
├── PreferencesGroup "Color"
│   ├── Saturation slider
│   ├── Vibrance slider
│   ├── Hue slider
│   ├── Temperature slider
│   └── Tint slider
└── Action bar
    ├── "Revert" (destructive)
    └── "Save" (suggested-action)
```

Header bar: add "Edit" toggle button (`document-edit-symbolic`) next to info toggle. Mutually exclusive — one panel visible at a time.

Edit session state held in viewer as `Option<EditSession>`:
```rust
struct EditSession {
    state: EditState,
    preview_image: Arc<DynamicImage>,  // ~1200px for fast preview
    full_res_image: Arc<DynamicImage>, // For final render on save
    render_gen: u64,                   // Debounce generation counter
}
```

## 7. Phased Implementation

| Phase | Scope | Key files |
|-------|-------|-----------|
| 1 | Data model + DB persistence | `editing.rs`, `db/edits.rs`, migration 014, trait impls |
| 2 | Edit renderer (no UI) | `edit_renderer.rs`, unit tests with reference images |
| 3 | Edit panel UI + exposure/color sliders | `viewer/edit_panel.rs`, `viewer.rs` changes |
| 4 | Geometric transforms (rotate, flip, straighten, crop) | Edit panel extensions, crop overlay widget |
| 5 | Filters/presets | Filter thumbnail row, preset definitions |
| 6 | Immich render-and-upload | `immich_client.rs` endpoints, `immich.rs` render_and_save |
| 7 | Polish | Grid edit indicator, thumbnail regen, keyboard shortcuts, error handling |

## Risks

- **Immich API**: `PUT /assets/{id}/original` needs verification. Fallback: upload as new asset.
- **Straighten**: `image` crate has no freeform rotation. Need `imageproc` dependency or manual bilinear interpolation. Can defer to Phase 4.
- **Crop overlay**: Draggable handles in GTK4 are complex. Start with dialog-based crop, upgrade to overlay later.
- **Preview performance**: If 1200px is too slow, drop to 800px. Profile in Phase 3.

## Files to Create

- `src/library/editing.rs` — EditState types + LibraryEditing trait
- `src/library/edit_renderer.rs` — apply_edits() pure function
- `src/library/db/edits.rs` — Database CRUD
- `src/library/db/migrations/014_create_edits.sql` — Schema
- `src/ui/viewer/edit_panel.rs` — Edit panel widget

## Files to Modify

- `src/library.rs` — Add LibraryEditing to Library supertrait
- `src/library/db.rs` — Add mod edits
- `src/library/providers/local.rs` — Implement LibraryEditing
- `src/library/providers/immich.rs` — Implement LibraryEditing + render_and_save
- `src/library/immich_client.rs` — Add edit upload/revert endpoints
- `src/ui/viewer.rs` — Edit button, session state, preview rendering
- `src/library/thumbnailer.rs` — Support edited thumbnail generation
