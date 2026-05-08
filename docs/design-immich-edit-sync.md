# Immich Edit Sync — Build Specification

**Status**: Approved, ready to implement.
**Issues**: #224 (Immich render-and-upload), #641 (sync-conflict schema — supersedes), #225 (polish — depends on this).
**Depends on**: #628 heartbeat reconciliation (merged on `main`).

This document specifies how Moments synchronises non-destructive edits with an Immich server. It is the result of an extended design conversation that included two empirical probe runs against an Immich v2.7.5 test instance and a roadmap review of the upstream project.

The build splits into four phases (A–D below). Phases A and B are independent and can land in either order; C depends on A; D depends on C.

---

## 1. Background

The previous design (`design-photo-editing.md` section 5) called for `PUT /assets/{id}/original` to overwrite the user's original on the server. **That endpoint was removed in current Immich `main`.** Empirical probing of v2.7.5 confirmed the only mutation surfaces available are:

- `PUT /assets/{id}/edits` — accepts a list of crop/rotate/mirror actions, native server-side modelling, syncs via `SyncAssetEditV1`. Shipped in v2.5.0; web in v2.6.0; mobile in v2.7.x. **Crop/rotate/mirror only — no exposure, no colour, no filters, no roadmap commitment for those.**
- `POST /assets` (multipart upload) and the stacks API (`POST /stacks`, `DELETE /stacks/{id}/assets/{aid}`).
- `PUT /tags` (idempotent upsert by name) and `PUT /tags/{id}/assets`.

The original is never replaced. Immich's "originals are sacred" architectural rule is observed.

---

## 2. Design summary

> **Status of this document**: Phase A is implemented in `feat/224-immich-stacks-phase-a`. The "actual wire shape vs. earlier draft" callouts in §4.1 and §4.2 record corrections discovered while testing against a live Immich v2.7.5 instance and cross-checking against the upstream OpenAPI spec.

**Two mechanisms, chosen by edit type:**

| Edit family | Mechanism | Server-side representation |
|---|---|---|
| Crop, rotate, mirror | `PUT /assets/{id}/edits` | Action list on the original asset, `isEdited: true` |
| Exposure, white balance, vignette, colour, curves, filters | Render local JPEG → upload as new asset → stack `[renderedId, originalId]` → tag rendered asset with `moments-edit` → embed XMP in rendered JPEG | Original + rendered child, stack relationship, custom XMP block in rendered bytes |

**Original assets are never modified.** The local `edits` table remains the source of truth for what edit was applied. The server-side artifacts (action list, stacked rendered asset) are caches/projections of that local state.

**Recovery** (fresh install, second machine, lost local DB): tag query enumerates Moments-edited assets; for each, the rendered sibling is downloaded on demand and XMP parsed to rebuild the local `edits` row. Geometric-only edits recover automatically via `SyncAssetEditV1` in the sync stream.

---

## 3. Local schema changes

Two new migrations, applied in order.

### 3.1 Migration `023_add_stacks.sql`

```sql
-- Issue #224: model Immich's asset stacks locally so the timeline can
-- collapse stacked siblings to the primary, and so push-side stack
-- mutations can be tracked.

CREATE TABLE stacks (
    id                TEXT    PRIMARY KEY NOT NULL,
    primary_asset_id  TEXT    NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    last_seen_at      INTEGER NOT NULL DEFAULT 0  -- joins #628 reconciliation
);

CREATE INDEX idx_stacks_primary_asset_id ON stacks(primary_asset_id);

ALTER TABLE media ADD COLUMN stack_id TEXT REFERENCES stacks(id) ON DELETE SET NULL;

CREATE INDEX idx_media_stack_id ON media(stack_id) WHERE stack_id IS NOT NULL;
```

The two cascades work together: deleting a `media` row cascade-deletes any `stacks` row whose primary it was, which in turn cascade-clears `stack_id` on the surviving siblings. Both fire automatically because sqlx 0.8 enables `PRAGMA foreign_keys` per connection.

### 3.2 Migration `024_add_edits_render_pointer.sql`

```sql
-- Issue #224: link a local edits row to the Immich asset that holds
-- its rendered output. Set when a pixel-adjustment edit is uploaded;
-- null for geometric-only edits (those use the server's edits API).

ALTER TABLE edits ADD COLUMN server_rendered_asset_id TEXT REFERENCES media(id) ON DELETE SET NULL;
ALTER TABLE edits ADD COLUMN xmp_edit_version INTEGER NOT NULL DEFAULT 1;
```

`xmp_edit_version` tracks the schema version of the embedded XMP, allowing forward-compatible parsing of older renders.

### 3.3 Stacks join the heartbeat reconciliation

Per #628, four tables already participate in the reset-cycle orphan sweep. `stacks` becomes the fifth:

- `MediaRepository::bump_stack_last_seen_at(id, now)` — mirror of the existing pattern.
- `MediaRepository::ids_with_stale_stack_heartbeat(checkpoint) -> Vec<String>` — orphan finder.
- New `delete_stacks_with_stale_heartbeat` in finish_sync, runs after `delete_with_stale_heartbeat` on albums and before the people/faces sweeps. The migration's `ON DELETE SET NULL` cascade clears member `stack_id` automatically (sqlx 0.8 enables `PRAGMA foreign_keys` per connection); the explicit transactional `UPDATE` in the repo is a defensive belt-and-braces.

Bumped from `StackHandler` whenever a `StackV1` is processed.

A second one-shot deletion path, `MediaRepository::delete_stack(id)`, mirrors the same transactional cleanup for the `SyncStackDeleteV1` ingress.

---

## 4. Sync wiring

### 4.1 Pull side: `AssetV1` carries flat `stackId`; stacks stream separately

Empirically verified against v2.7.5 and confirmed against the OpenAPI spec on `main` — the `/sync/stream` endpoint emits stacks across **two coordinated streams**: `AssetV1` carries a flat `stackId` pointer, and `StackV1` / `StackDeleteV1` carry the stack metadata (including the primary asset id).

> **Earlier draft of this section assumed a nested `stack` object on `AssetV1`** (mirroring the `/api/assets/{id}` detail endpoint). That was wrong — confirmed by inspecting the live wire payload during Phase A verification. The actual sync wire shape is what this section now describes.

`SyncAssetV1` payload (relevant fields):

```json
{
  "stackId": "<stack uuid>" | null,
  "isEdited": false
}
```

`SyncStackV1` payload:

```json
{
  "id": "<stack uuid>",
  "primaryAssetId": "<asset uuid>",
  "ownerId": "<owner uuid>",
  "createdAt": "...",
  "updatedAt": "..."
}
```

`SyncStackDeleteV1` payload:

```json
{ "stackId": "<stack uuid>" }
```

Update `SyncAssetV1` in `src/sync/providers/immich/types.rs` to deserialise `stackId: Option<String>` (flat) and `isEdited: Option<bool>`. Add `SyncStackV1` and `SyncStackDeleteV1` types. Subscribe to `"StacksV1"` in the request `types` array.

Handlers:

1. **`AssetHandler`** (existing) — when handling `AssetV1`: if `stackId` is set, `set_media_stack_id`; if null, `clear_media_stack_id`. Does NOT touch the `stacks` table — that's `StackHandler`'s job.
2. **`StackHandler`** (new, `handlers/stack.rs`) — for `StackV1`: translate `primaryAssetId` (Immich UUID) to local `MediaId` via `id_by_external_id`; upsert the `stacks` row; bump heartbeat. Warn-and-skip if the primary's local row hasn't streamed yet — the next pull cycle re-emits.
3. **`StackDeleteHandler`** (new) — for `StackDeleteV1`: call `delete_stack(stack_id)` which clears member pointers and deletes the row in one transaction.

**Order independence**: `AssetV1` and `StackV1` arrive in arbitrary order. The FK on `media.stack_id REFERENCES stacks(id)` is enforced (sqlx 0.8 enables `PRAGMA foreign_keys` by default), so an asset can't bind to a non-existent stack.

Resolution: when `AssetHandler` sees an `AssetV1.stackId` for a stack that hasn't yet been upserted locally, it first creates a **stub** `stacks` row via `ensure_stack_stub(id, media_id)` — `INSERT … ON CONFLICT DO NOTHING` with the current asset itself as the placeholder `primary_asset_id`. The asset can then bind. When `StackV1` for that id arrives later, `upsert_stack` overwrites the placeholder primary with the authoritative one (and the `ON CONFLICT(id) DO UPDATE SET primary_asset_id = excluded.primary_asset_id` clause leaves `last_seen_at` untouched).

Until the real `StackV1` lands, the grid may show the wrong asset as primary for that brief window — accepted as a transient. The stub's `last_seen_at = 0` means it's eligible for the heartbeat sweep, but the real `StackV1` will bump it before any sweep runs.

### 4.2 Pull side: `SyncAssetEditV1` (one event per action)

New entity type. The OpenAPI spec on `main` shows that edits stream **one record per action**, not as a single batched payload — each `SyncAssetEditV1` carries a single action with a `sequence` number that imposes an order across the asset's actions.

`SyncAssetEditV1` payload:

```json
{
  "id":        "<edit uuid>",
  "assetId":   "<asset uuid>",
  "action":    "crop" | "rotate" | "mirror",
  "parameters": { "x": 0, "y": 0, "width": 100, "height": 100 },
  "sequence":  0
}
```

A complementary `SyncAssetEditDeleteV1` (with the edit `id`) cancels a single action.

> **Earlier draft of this section assumed a single batched payload** with an `edits[]` array. That was wrong — Immich actually emits per-action records with sequence numbers. Each handler invocation is one row in a per-asset action list, ordered by `sequence`.

New handler `AssetEditHandler` in `src/sync/providers/immich/handlers/asset_edit.rs`:

1. Translate `assetId` (Immich UUID) → local `MediaId` via `media().id_by_external_id()`. Warn-and-skip if parent asset isn't local yet.
2. Upsert a row keyed by `(media_id, edit_id)` carrying `(action, parameters_json, sequence)` — local schema TBD as part of Phase B.
3. Recompose the asset's edit state by pulling all rows for that asset ordered by `sequence` (translation to `EditState` per §5.3).

Register in `handlers/mod.rs::all_handlers()` along with `AssetEditDeleteHandler`.

Subscribe to the new entity types in `pull.rs::run_sync` request body:

```rust
types: vec![
    "AssetsV1".to_string(),
    "AssetExifsV1".to_string(),
    "AssetEditsV1".to_string(),  // new (Phase B)
    "AlbumsV1".to_string(),
    "AlbumToAssetsV1".to_string(),
    "PeopleV1".to_string(),
    "AssetFacesV1".to_string(),
    "StacksV1".to_string(),       // added in Phase A
],
```

### 4.3 Push side: new outbox mutation types

Add to `src/library/mutation.rs::Mutation`:

```rust
pub enum Mutation {
    // ... existing ...

    /// Geometric edits via PUT /assets/{id}/edits. Replaces any
    /// existing server-side edit list for the asset.
    AssetEditsApplied {
        id: MediaId,
        actions: Vec<ImmichEditAction>,  // crop/rotate/mirror with params
    },

    /// Revert geometric edits via DELETE /assets/{id}/edits.
    AssetEditsCleared { id: MediaId },

    /// A new rendered asset has been uploaded and should be stacked
    /// with its source. The rendered asset's external_id is captured
    /// from the upload response and stamped before this mutation
    /// records.
    StackCreated {
        primary_asset_id: MediaId,    // the rendered asset
        original_asset_id: MediaId,   // the source
    },

    /// A stack member should be removed from its stack on the server.
    /// Used during revert (we remove the rendered child; Immich auto-
    /// deletes the stack when it falls below 2 members).
    StackMemberRemoved {
        stack_id: String,
        asset_id: MediaId,
    },

    /// Apply the moments-edit tag to a rendered asset. Idempotent.
    /// The tag itself is created lazily by the push handler the first
    /// time this mutation is processed for an account.
    AssetTaggedMomentsEdit { id: MediaId },

    /// Detach the moments-edit tag from a rendered asset. Used during
    /// revert.
    AssetUntaggedMomentsEdit { id: MediaId },
}
```

Corresponding `OutboxMutation::from_row` decoders in `src/sync/outbox/mutation.rs`.

### 4.4 Push side: `PushManager::push_one` arms

Add match arms in `src/sync/providers/immich/push.rs`:

```rust
OutboxMutation::AssetEditsApplied { id, actions } => {
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    self.client
        .put_no_content(
            &format!("/assets/{external_id}/edits"),
            &serde_json::json!({ "edits": actions }),
        )
        .await?;
    self.bump_media_heartbeat(id.as_str()).await
}

OutboxMutation::AssetEditsCleared { id } => {
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    self.client
        .delete_no_content(&format!("/assets/{external_id}/edits"))
        .await?;
    self.bump_media_heartbeat(id.as_str()).await
}

OutboxMutation::StackCreated { primary_asset_id, original_asset_id } => {
    let primary_ext  = self.lookup_media_external_id(primary_asset_id.as_str()).await?;
    let original_ext = self.lookup_media_external_id(original_asset_id.as_str()).await?;
    let resp: serde_json::Value = self.client.post(
        "/stacks",
        &serde_json::json!({ "assetIds": [primary_ext, original_ext] }),
    ).await?;
    if let Some(stack_id) = resp["id"].as_str() {
        // Upsert local stacks row, set media.stack_id on both members.
        self.persist_stack_locally(stack_id, primary_asset_id.as_str(),
            &[primary_asset_id.as_str(), original_asset_id.as_str()]).await?;
    }
    Ok(())
}

OutboxMutation::StackMemberRemoved { stack_id, asset_id } => {
    let asset_ext = self.lookup_media_external_id(asset_id.as_str()).await?;
    self.client
        .delete_no_content(&format!("/stacks/{stack_id}/assets/{asset_ext}"))
        .await?;
    // Server auto-deletes stack at <2 members; local cleanup happens
    // when the next AssetV1 arrives with stack: null.
    Ok(())
}

OutboxMutation::AssetTaggedMomentsEdit { id } => {
    let tag_id = self.ensure_moments_edit_tag().await?;
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    self.client.put_no_content(
        &format!("/tags/{tag_id}/assets"),
        &serde_json::json!({ "ids": [external_id] }),
    ).await
}

OutboxMutation::AssetUntaggedMomentsEdit { id } => {
    let tag_id = self.ensure_moments_edit_tag().await?;
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    self.client.delete_with_body(
        &format!("/tags/{tag_id}/assets"),
        &serde_json::json!({ "ids": [external_id] }),
    ).await
}
```

`ensure_moments_edit_tag` calls `PUT /tags { tags: ["moments-edit"] }` (idempotent), caches the tag id in memory for the push session, and re-derives on cache miss.

---

## 5. XMP specification

### 5.1 Namespace

```
prefix: moments
URI:    urn:moments:edits:1.0
```

`1.0` is the schema version. Bumped only when the embedded JSON shape changes incompatibly. Consumers parse forward-compatibly: ignore unknown fields, accept versions ≥ minimum supported.

### 5.2 Fields

Embedded as RDF properties under a single `rdf:Description` block:

```xml
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:moments="urn:moments:edits:1.0">
      <moments:originalAssetId>e4f66d10-b88f-44e7-89ce-79259805a3b3</moments:originalAssetId>
      <moments:originalContentHash>3ed6e7c8...base64</moments:originalContentHash>
      <moments:editVersion>1</moments:editVersion>
      <moments:renderedAt>2026-05-08T12:34:56Z</moments:renderedAt>
      <moments:editJson>{"exposure":0.5,"vignette":...}</moments:editJson>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `originalAssetId` | string (UUID) | yes | Immich asset id of the source. Primary recovery key. |
| `originalContentHash` | string (SHA-1, base64) | yes | Fallback if `originalAssetId` doesn't resolve (rare: source was re-uploaded). Must match Immich's `checksum` field format. |
| `editVersion` | integer | yes | Schema version of `editJson`. Starts at 1; bump on incompatible changes. |
| `renderedAt` | string (RFC 3339) | yes | When the render was produced locally. Tiebreaker if multiple Moments installs raced. |
| `editJson` | string (JSON) | yes | The serialised `EditState`. Format defined in `library/editing/model.rs`. |

### 5.3 Action mapping (geometric)

When the editor's `EditState` contains only crop/rotate/mirror, the save flow does **not** produce a render or XMP. Instead it serialises to the Immich edits-API actions:

```
EditState.crop(x, y, w, h)              -> { action: "crop",   parameters: { x, y, width, height } }
EditState.rotate(degrees: 90 | 180 | 270) -> { action: "rotate", parameters: { angle } }
EditState.mirror(Horizontal | Vertical)   -> { action: "mirror", parameters: { axis: "horizontal"|"vertical" } }
```

Reverse map applies in `AssetEditHandler` for the pull side.

### 5.4 Encoder/decoder module

New module `src/renderer/xmp.rs`:

```rust
pub struct EmbeddedEdit {
    pub original_asset_id: String,
    pub original_content_hash: String,
    pub edit_version: u32,
    pub rendered_at: chrono::DateTime<chrono::Utc>,
    pub edit_json: String,
}

pub fn encode(edit: &EmbeddedEdit) -> String { ... }
pub fn decode(xml: &str) -> Result<EmbeddedEdit, XmpError> { ... }
```

Implementation: hand-rolled with `quick-xml`. The XMP block is wrapped in JPEG APP1 segment with the standard Adobe XMP marker `http://ns.adobe.com/xap/1.0/\0`. See [XMP Specification Part 3 §1.1.3](https://www.adobe.com/content/dam/acom/en/devnet/xmp/pdfs/XMP%20SDK%20Release%20cc-2016-08/XMPSpecificationPart3.pdf) for the segment layout.

JPEG APP1 segment injection: render the JPEG via the `image` crate, then post-process the byte stream to insert the APP1 marker after the SOI. ~50 LoC. Reuse: when reading XMP back, walk the JPEG segments to find APP1 with the XMP marker.

---

## 6. Tag specification

### 6.1 Lifecycle

- **Creation**: lazy on first push of `AssetTaggedMomentsEdit`. `PUT /tags { tags: ["moments-edit"] }` is idempotent — returns existing or creates.
- **Caching**: cache the tag id on `PushManager` for the session. Re-fetch on next session start (or cache miss).
- **Application**: only on rendered assets, never on originals.
- **Revert**: `AssetUntaggedMomentsEdit` detaches before the rendered asset is deleted from the stack.

### 6.2 Visibility

In Immich v2.7.5 web UI:

- Photo info panel: **does not display tags** (verified empirically).
- Sidebar Explore → Tags page: lists `moments-edit` as a category; clicking shows tagged assets.
- Search bar: tag autocompletes as a filter.
- API `AssetResponseDto.tags[]`: present, syncs through `AssetV1`.

The tag should be treated as a permanent server-side marker that *may* become user-visible if Immich's UI changes. Tag name `moments-edit` is acceptable in either case.

### 6.3 Discovery query

For new-install enumeration (Phase D):

```
POST /api/search/metadata
{ "tagIds": ["<moments-edit-tag-id>"], "size": 1000 }
```

Returns paged list of all assets carrying the tag — i.e., every Moments-rendered asset on this Immich account. Iterate with `page` parameter for libraries with > 1000 edits.

---

## 7. UI / save flows

### 7.1 Save (geometric-only edit)

Triggered when `EditState::is_geometric_only()` returns true.

```
1. Persist EditState to local edits table.
2. Convert to ImmichEditAction list (§5.3).
3. Record outbox mutation AssetEditsApplied { id, actions }.
4. Done. PushManager flushes; server applies; SyncAssetEditV1 echoes back on next pull.
```

No render, no upload, no stack, no tag. Single API call on push.

### 7.2 Save (pixel adjustments present)

Triggered when `EditState::has_pixel_adjustments()` returns true.

```
1. Persist EditState to local edits table (edit_json updated).
2. Render full-resolution JPEG via RenderPipeline.
3. Compute SHA-1 of original (or read from media.content_hash if cached).
4. Encode XMP block (§5.4) with EmbeddedEdit fields populated.
5. Inject XMP into JPEG APP1 segment.
6. If edits.server_rendered_asset_id is non-null:
   a. Outbox: StackMemberRemoved { stack_id, asset_id: server_rendered_asset_id }.
   b. Outbox: AssetUntaggedMomentsEdit { id: server_rendered_asset_id }.
   c. Outbox: AssetDeleted { id: server_rendered_asset_id }.
   d. Wait for these to drain (or interleave; idempotent).
7. Outbox: AssetImported { rendered_bytes, filename: "<original>-edit.jpg", ... }
8. After push completes and external_id is stamped on the new media row:
   a. Outbox: StackCreated { primary_asset_id: rendered, original_asset_id: original }.
   b. Outbox: AssetTaggedMomentsEdit { id: rendered }.
9. Update edits.server_rendered_asset_id on the local row to the new render's MediaId.
```

The "delete old render, upload new render" sequence ensures at most one rendered sibling per photo. Step 6 is skipped on the first edit.

### 7.3 Revert

```
1. If geometric-only edits exist: outbox AssetEditsCleared { id: original }.
2. If edits.server_rendered_asset_id is non-null:
   a. Outbox StackMemberRemoved.
   b. Outbox AssetUntaggedMomentsEdit.
   c. Outbox AssetDeleted (the rendered asset).
3. Delete local edits row.
4. UI: navigate back to original (it surfaces in the timeline as the stack auto-collapses).
```

### 7.4 Recovery (Phase D)

Triggered when user opens edit panel on a photo whose local `edits` row is absent but whose Immich asset has the `moments-edit` tag (or is the primary of a stack containing a `moments-edit`-tagged child).

```
1. Find the rendered sibling: SELECT id FROM media WHERE stack_id = ? AND id != ?
   (the non-primary; for an edited photo the primary IS the rendered)
   Actually: SELECT m.id FROM media m
     JOIN stacks s ON m.stack_id = s.id
     WHERE s.id = <stack of this photo> AND m.id = s.primary_asset_id
   That's the rendered. (Wait: the rendered IS primary in our model.)

   Equivalent: find the Moments-tagged member of the stack the user opened.

2. Download the rendered JPEG (cache locally — they were going to view it anyway).
3. Parse XMP block; extract EmbeddedEdit.
4. Validate: originalAssetId resolves to a local media row whose external_id matches.
   Fallback: lookup by originalContentHash if asset id mismatches.
5. Insert local edits row from editJson, set server_rendered_asset_id.
6. Open editor; user continues from where the previous Moments install left off.
```

Edge case: corrupt or missing XMP. Surface "edit history not recoverable; revert and re-edit" in the UI.

---

## 8. Grid filtering

The grid query (across All, Favorites, Recent Imports, Album views, Person views) must filter to primary-only when stacks are present:

```sql
SELECT m.*
FROM media m
LEFT JOIN stacks s ON m.stack_id = s.id
WHERE m.is_trashed = 0
  AND (s.id IS NULL OR s.primary_asset_id = m.id)
  AND <existing filter clauses>
```

Touch points:

- `MediaRepository::list_filtered` (and any pagination cursors)
- `AlbumRepository::list_media`
- `FacesRepository::list_media_for_person`
- Any direct SQL in clients (audit grep).

The Recent Imports view counts edits as imports — when the user saves a pixel-adjustment edit, the rendered asset has a fresh `imported_at` and would appear in Recent Imports. That's correct behaviour.

---

## 9. Phased plan

### Phase A — schema + sync wiring (no editor UX changes)

Independent of the editor. Lands the stack model and recovers any user who already uses Immich's stack feature in the wild (panoramas, bursts).

- [x] Migration `023_add_stacks.sql`
- [x] Migration `024_add_edits_render_pointer.sql`
- [x] `SyncAssetV1` deserialises **flat** `stackId` and `isEdited`; new `SyncStackV1` / `SyncStackDeleteV1` types
- [x] `AssetHandler` sets/clears `media.stack_id` from `AssetV1.stackId`
- [x] `StackHandler` upserts `stacks` rows from `StackV1`; `StackDeleteHandler` removes them on `StackDeleteV1`
- [x] Subscribe to `"StacksV1"` in sync request types
- [x] `MediaRepository`: `upsert_stack`, `set/clear_media_stack_id`, `bump_stack_last_seen_at`, `ids_with_stale_stack_heartbeat`, `delete_stacks_with_stale_heartbeat`, `delete_stack`
- [x] Service-layer accessors
- [x] Extend `finish_sync` in `pull.rs` to sweep stale stacks
- [x] Grid query filter on `MediaRepository::list`, `AlbumRepository::list_media`, `FacesRepository::list_media_for_person`
- [x] Stack-badge overlay on photo grid cell (`edit-copy-symbolic`, top-right)
- [x] Tests: stack upsert idempotency, stale-sweep with member rejoin, primary-only filter, clear-membership

Acceptance: pulling from a fresh Immich account that contains stacks (panoramas, bursts) results in a timeline that shows only primaries; clicking a stacked thumbnail expands to siblings; no duplicate-asset bug.

### Phase B — geometric edits via `/assets/{id}/edits`

Independent of Phase A; can land in parallel.

- [ ] `ImmichClient` methods: `get_asset_edits`, `put_asset_edits`, `delete_asset_edits`
- [ ] `Mutation::AssetEditsApplied` and `AssetEditsCleared` enum variants
- [ ] `OutboxMutation::from_row` decoders
- [ ] `PushManager::push_one` arms (§4.4)
- [ ] `AssetEditHandler` for `SyncAssetEditV1`
- [ ] `EditState::is_geometric_only()` predicate
- [ ] `EditState ↔ ImmichEditAction` mapping (§5.3)
- [ ] Editor save flow §7.1
- [ ] Tests: probe with corrected nested-`parameters` payload (open question §11.1 to resolve first)

Acceptance: cropping a photo in the Moments editor + save → photo appears cropped in Immich web within a sync cycle; cropping the same photo on Immich web → Moments shows the crop after next pull; revert clears it from both sides.

### Phase C — pixel-adjustment edits via render+stack+tag+XMP

Depends on Phase A (stacks model).

- [ ] `src/renderer/xmp.rs` encoder/decoder
- [ ] JPEG APP1 segment injection in render pipeline output
- [ ] `Mutation::StackCreated`, `StackMemberRemoved`, `AssetTaggedMomentsEdit`, `AssetUntaggedMomentsEdit` variants
- [ ] `OutboxMutation::from_row` decoders
- [ ] `PushManager::push_one` arms (§4.4) and `ensure_moments_edit_tag` helper
- [ ] `EditState::has_pixel_adjustments()` predicate
- [ ] Editor save flow §7.2 (two-step delete-then-upload)
- [ ] Editor revert flow §7.3
- [ ] Tests: XMP roundtrip, save → render → upload → stack → tag end-to-end (mocked HTTP), revert

Acceptance: applying a vignette in the Moments editor + save → original photo gets stacked with a new rendered asset on Immich; the rendered asset is primary, has `moments-edit` tag, and contains the XMP block; revert removes the rendered asset and the stack auto-collapses.

### Phase D — recovery

Depends on Phase C.

- [ ] On editor open: detect "stack with `moments-edit`-tagged child but no local `edits` row"
- [ ] Lazy XMP fetch + decode + edits-row reconstruction (§7.4)
- [ ] Optional: eager tag-based discovery on first sync (`GET /search/metadata?tagIds=…`)
- [ ] Tests: simulated fresh install scenario

Acceptance: deleting the local `edits` row for a Moments-edited photo and reopening the editor recovers the edit state from XMP without user-visible failure.

---

## 10. Test strategy

### 10.1 Unit / integration

- Repository layer for new tables (existing pattern).
- XMP encoder/decoder roundtrip.
- JPEG APP1 segment injection roundtrip.
- `OutboxMutation::from_row` for new variants.
- Push handler dispatch (mock ImmichClient).
- Pull handler `AssetEditHandler`.

### 10.2 End-to-end against the local Immich test instance

`http://localhost:2283`, API key at `/home/justin/.config/immich-test/api_key`.

Tests should be opt-in (env-gated) since CI doesn't have an Immich server. A `make test-immich` target.

- Crop a known asset → assert `isEdited: true` and edit list reflects the crop.
- Apply pixel edit → assert new asset created + stack + tag.
- Revert → assert rendered asset gone, stack auto-collapsed.
- Recovery: wipe local `edits` row, reopen editor, assert reconstruction.

### 10.3 Empirical probe before Phase B starts

Verify (open question §11.1) that the corrected `PUT /assets/{id}/edits` payload works on v2.7.5:

```bash
curl -X PUT -H "x-api-key: $KEY" -H "Content-Type: application/json" \
  -d '{"edits":[{"action":"rotate","parameters":{"angle":90}}]}' \
  http://localhost:2283/api/assets/<uuid>/edits
```

Expected: 200 with empty body, asset's `isEdited: true` and `edits[]` populated. Cleanup with DELETE.

If still 500: open an issue upstream and reduce Phase B scope to "best-effort, fall back to render+stack for geometric ops too".

---

## 11. Open questions

### 11.1 Does the corrected `/edits` payload work on v2.7.5?

The first probe sent a flat `{action, x, y, width, height}` shape. The OpenAPI spec on `main` confirms the correct shape is `{action, parameters: {…}}`. Re-test with that before Phase B implementation begins.

### 11.2 Does Immich strip XMP from uploaded JPEGs?

The probe established that originals aren't transcoded. The render is uploaded *as a new asset* via `POST /assets`. Verify: upload a JPEG with embedded custom XMP, fetch it back via `GET /assets/{id}/original`, confirm bytes are identical. If Immich rewrites EXIF/XMP at ingestion time, the embedded `editJson` would be lost.

### 11.3 SHA-1 dedup interaction

If the user "edits" with no-op adjustments and saves, the rendered JPEG could conceivably hash-match an existing asset (rare; JPEG re-encoding usually perturbs bytes). The probe found `bulk-upload-check` returns `"reject", "duplicate"` for matching SHA-1. Implementations options:

- (a) Pre-flight `bulk-upload-check`; if duplicate, skip the upload and stack with the existing matched asset.
- (b) Add a `EditState::is_identity()` short-circuit in the editor save path so no-op saves never produce a render.

(b) is cheaper and probably sufficient.

### 11.4 Stack merging semantics

What does `POST /stacks { assetIds: [a, b] }` do when `a` is already in another stack? Probe didn't test. Affects multi-device editing race: device A creates stack; device B tries to create overlapping stack. Possible outcomes: 4xx (we handle and refresh), merge (cleaner), or duplicate (bad).

### 11.5 Multi-device editing race

If devices A and B both render-and-stack the same original concurrently:

```
Server end state: stack { original, render_A, render_B }, primary = whichever POST won the race.
```

Our local `edits.server_rendered_asset_id` records "our" render. The other render is visible on every client but isn't authoritative locally. Resolution policy: TBD. Probably "newest `renderedAt` wins; older render is auto-deleted on next save". Defer to a follow-up issue if it doesn't surface in v0.4.0.

### 11.6 What about the existing `design-photo-editing.md`?

Section 5 of that doc describes the now-dead `PUT /assets/{id}/original` model. It must be updated — either by deleting section 5 and pointing to this document, or by replacing its content with a summary of this design.

---

## 12. Out of scope

- **Filter API for non-geometric edits.** Immich does not currently model exposure/colour/curves server-side. No timeline. We use the render+stack path until they ship one (which would let us migrate to native if/when it appears).
- **Three-way merge of conflicting edits.** Multi-device races resolve by last-write-wins (see §11.5). No UI for "your edit and someone else's edit conflict; merge?".
- **Editing originals (replace bytes).** Architecturally rejected. Originals are immutable on the server.
- **Editing other users' shared photos.** Out of scope — editing requires `AssetEditCreate` permission which is owner-only.
- **Stacking unrelated photos as an "edit".** Stacks created in the Immich UI by users (e.g. burst grouping) must work transparently — we never assume a stack means "Moments edit". The `moments-edit` tag is the disambiguator.
- **Sidecar XMP files.** Immich doesn't expose them via API for retrieval; even though it parses them at upload time, we can't read them back. We embed XMP in the rendered JPEG bytes directly.

---

## 13. References

- Issue #224: <https://github.com/justinf555/Moments/issues/224>
- Issue #641: <https://github.com/justinf555/Moments/issues/641> (supersede)
- Issue #225: <https://github.com/justinf555/Moments/issues/225> (depends on this)
- #628 heartbeat reconciliation: merged on `main` (PR #649)
- Immich v2.5.0 editor PR: <https://github.com/immich-app/immich/pull/24155>
- Immich v2.7.x mobile editor PR: <https://github.com/immich-app/immich/pull/25397>
- Immich OpenAPI spec: <https://raw.githubusercontent.com/immich-app/immich/main/open-api/immich-openapi-specs.json>
- Existing editing design: `docs/design-photo-editing.md` (section 5 to be updated to reference this doc)
- XMP Specification Part 3 (storage in files): see Adobe SDK
