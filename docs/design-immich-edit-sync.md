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

> **Status of this document**: Phase A merged via #651. Phase B merged via #652 — deviates from the §4 sketches in three deliberate ways (see Phase B as-built callouts in §4.2 / §4.3 / §4.4 / §7.1). Phase C is implemented in `feat/224-immich-edits-phase-c` and lands the full pixel-adjustment path: render → upload → stack → tag → embed XMP, with the §8.2 grid override flipped on so the timeline surfaces the original. See Phase C as-built callouts in §4.3 / §4.4 / §7.2 / §7.3 / §8.2 for as-built notes (orchestration lives on `Library::save_pixel_edit` / `Library::revert_edit`; the UI client branches on `EditState::has_pixel_adjustments()`). Phase D (XMP-based recovery) remains as drafted. The "actual wire shape vs. earlier draft" callouts in §4.1 / §4.2 record corrections discovered while testing against a live Immich v2.7.5 instance and cross-checking against the upstream OpenAPI spec.
>
> **Phase B layering summary**: `Mutation::AssetEdits{Applied,Cleared}` are payload-free. `ImmichEditAction` lives in `src/sync/providers/immich/edit_action.rs` and never leaks into the library. The push handler reads the latest `EditState` from `EditingRepository::get_edit_state()` at drain time, projects to actions, and decides PUT vs. DELETE vs. skip — this is **latest-state read** (every wire call sends the final state, so no intermediate edit can leak past a later save), not coalescing (N saves still drive N round-trips; insert-side dedup would be a future enhancement). There is no `EditState::is_geometric_only()` predicate; `project()` returns `Option<Vec<ImmichEditAction>>` and `None` is the "Phase C territory" signal.

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

A complementary `SyncAssetEditDeleteV1` (with the edit id under field `editId`) cancels a single action.

> **Wire-shape correction (Phase B run-time)** — initial draft assumed `id`; convention guess assumed `assetEditId`. Empirically (live v2.7.5 sync stream during dev testing), the actual payload is `{"editId": "<uuid>"}` — Immich uses the shorter form here even though every other delete entity follows `<entity>Id` (`SyncAssetDeleteV1.assetId`, `SyncStackDeleteV1.stackId`, etc.). Phase B's branch reflects the corrected shape.

> **Earlier draft of this section assumed a single batched payload** with an `edits[]` array. That was wrong — Immich actually emits per-action records with sequence numbers. Each handler invocation is one row in a per-asset action list, ordered by `sequence`.

New handler `AssetEditHandler` in `src/sync/providers/immich/handlers/asset_edit.rs`:

1. Translate `assetId` (Immich UUID) → local `MediaId` via `media().id_by_external_id()`. Warn-and-skip if parent asset isn't local yet.
2. Upsert a row keyed by Immich `edit_id` (PK) carrying `(media_id, action, parameters_json, sequence, last_seen_at)` into the `immich_asset_edits` table.
3. Recompose the asset's `EditState` by pulling all rows for that asset ordered by `sequence` and folding via `edit_action::recompose()`. Write the result back to the user-facing `edits` table via `EditingRepository` (no recorder — server-driven update).

> **As-built (#224 Phase B)** — schema is `migrations/025_add_immich_asset_edits.sql`. `immich_asset_edits` is provider-scoped Immich bookkeeping, not library data; the library never reads or writes it. `media_id` has `ON DELETE CASCADE` so the orphan sweep on `media` clears the cached actions automatically (sqlx 0.8 enables `PRAGMA foreign_keys` per connection). Recompose tolerates third-party-tool shapes: rotates sum mod 360, repeated mirrors of the same axis XOR, last crop wins. Crop coordinate translation needs `media.width × media.height`; if dims are missing, the cached actions are kept but the `EditState` write is skipped (next pull retries once dims arrive).

Register in `handlers/mod.rs::all_handlers()` along with `AssetEditDeleteHandler`. The handler also needs direct DB access for the bookkeeping table, so `SyncContext` gains a `db: Database` field alongside the existing `library`/`state`/`client`/`thumbnails_dir`.

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

> **Superseded (#679):** `AssetsV1` and `AssetFacesV1` were retired server-side and now return 400. The live list is `SYNC_REQUEST_TYPES` in `src/sync/providers/immich/pull.rs` — see `docs/design-immich-backend.md`.

### 4.3 Push side: new outbox mutation types

Add to `src/library/mutation.rs::Mutation`:

```rust
pub enum Mutation {
    // ... existing ...

    /// The local edit state for an asset was updated. Payload-free —
    /// the push handler reads the current `EditState` at drain time
    /// and projects to whatever wire shape the provider needs.
    AssetEditsApplied { id: MediaId },

    /// The local edit state for an asset was cleared (revert).
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

> **As-built (#224 Phase B)** — `AssetEditsApplied` and `AssetEditsCleared` are payload-free. The earlier draft had `actions: Vec<ImmichEditAction>` in the variant, which leaked the Immich wire shape into a library-level type and produced one outbox row per save. The payload-free shape gives **latest-state read** at drain (each push reads `EditState` so every wire call sends the final state — no risk of stale intermediates) and keeps `ImmichEditAction` in `sync/providers/immich/edit_action.rs` where it belongs. Note: this is *not* coalescing — N saves still produce N outbox rows and N PUT calls; an insert-side `ON CONFLICT DO UPDATE` could collapse them but isn't done here. Phase C variants (`StackCreated`, `AssetTaggedMomentsEdit`, etc.) are still drafted as below; their as-built form will be revisited when Phase C lands.

### 4.4 Push side: `PushManager::push_one` arms

Add match arms in `src/sync/providers/immich/push.rs`:

```rust
// As-built (#224 Phase B): the AssetEditsApplied row carries no
// payload, so the push handler reads the latest EditState here and
// decides PUT/DELETE/skip based on whether the state is identity,
// projectable, or pixel-only (Phase C territory).
OutboxMutation::AssetEditsApplied { id } => {
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    let state = EditingRepository::new(self.db.clone())
        .get_edit_state(&id)
        .await?;
    let actions = match state.as_ref() {
        Some(s) if !s.is_identity() => {
            let dims = self.lookup_media_dims(id.as_str()).await?;
            edit_action::project(s, dims)
        }
        _ => Some(Vec::new()),
    };
    match actions {
        Some(actions) if !actions.is_empty() => {
            self.client.put_asset_edits(&external_id, &actions).await?;
        }
        Some(_) => {
            self.client.delete_asset_edits(&external_id).await?;
        }
        None => {
            warn!(id = %id, "edit not projectable to /edits — Phase C; skipping");
            return Ok(());
        }
    }
    self.bump_media_heartbeat(id.as_str()).await
}

OutboxMutation::AssetEditsCleared { id } => {
    let external_id = self.lookup_media_external_id(id.as_str()).await?;
    self.client.delete_asset_edits(&external_id).await?;
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

> **As-built (#224 Phase C)** — implemented largely as drafted; `ensure_moments_edit_tag` lives on `PushManager` with a `Mutex<Option<String>>` cache. Two notable wire-shape findings from the §10.3 probe pass: `POST /tags { name }` returns **400** on existing tags (rejected), so we use `PUT /tags { tags: [...] }` exclusively. `delete_stack_member` returns **204** and leaves the stack record with a single member; the surviving sibling's `stackId` is cleared on the asset row, and local cleanup completes via the next pull cycle's heartbeat reconciliation. The stack-create response is `{ id, primaryAssetId, assets: [...] }`; we use `id` for `persist_stack_locally` and defensively check `primary_asset_id` matches what we expected (Immich convention: first id in `assetIds` becomes primary). `require_media_external_id` is a strict variant of `lookup_media_external_id` that errors when `external_id` is null — Phase C push arms use it so `StackCreated` / `AssetTaggedMomentsEdit` automatically retry via outbox backoff until `AssetImported` drains.

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

> **As-built (#224 Phase B)** — there is no `EditState::is_geometric_only()` predicate and no provider-specific logic at save time. `EditingService::save_edit_state` does:
>
> ```
> 1. Persist EditState to the local `edits` table.
> 2. Record outbox mutation:
>    - AssetEditsApplied { id }   if state is non-identity
>    - AssetEditsCleared { id }   if state is identity (revert-via-resave)
> 3. Done.
> ```
>
> The mutation is payload-free. The push handler decides what to do at drain time (§4.4): identity or `None` from `project()` → DELETE; geometric → PUT; pixel adjustments → log + skip until Phase C lands. No render, no upload, no stack, no tag for any path that flows through `/assets/{id}/edits`.

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

> **As-built (#224 Phase C)** — implemented as `Library::save_pixel_edit(original_id, state, rendered_bytes)`. The UI layer (`MediaClientV2::save_edit_state`) branches on `EditState::has_pixel_adjustments()` and runs the render + JPEG encode on a Tokio blocking thread before calling into Library. Library owns XMP encode + inject (so the original's `external_id` / `content_hash` never have to leak into the UI), generates the rendered `MediaId`, writes the bytes to the standard sharded originals layout, and inserts the rendered `media` row (which records `AssetImported` via the existing media-service path). All four mutations (`AssetImported`, `StackCreated`, `AssetTaggedMomentsEdit`, plus the prior-render cleanup tuple) are enqueued at save time — the push manager's existing outbox retry handles the `AssetImported → external_id → StackCreated` dependency (the strict `require_media_external_id` errors and the backoff cycles the entry until the upload completes; ordering by outbox row id keeps the steps in sequence on the happy path). **Erroring case**: if the original isn't yet on the server (no `external_id`), `save_pixel_edit` returns an error — Phase D recovery depends on a stable `originalAssetId` in the XMP, so silently saving without it would break recoverability.

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

> **As-built (#224 Phase C)** — implemented as `Library::revert_edit(original_id)`. The flow is: look up the rendered sibling via `editing.server_rendered_asset_id`; if non-null, enqueue `StackMemberRemoved` (skipped when `media.stack_id` is null — `StackCreated` hadn't drained yet) + `AssetUntaggedMomentsEdit`, then call `Library::delete_permanently` for the rendered asset (which records `AssetDeleted` and cleans up the local file). Finally records `AssetEditsCleared` for the original as belt-and-braces against any geometric-only state that was previously pushed via the Phase B path (idempotent: Immich `DELETE /assets/{id}/edits` returns 204 even when no edits exist). UI navigation (step 4) is handled by the existing client signal flow when the original surfaces in the grid as the stack collapses.

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

### 8.1 Generic primary-only filter (Phase A)

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

### 8.2 Phase C UX override: surface the original for Moments-edit stacks

The Phase A primary-only rule is correct for Immich-native stacks (panoramas, bursts). It is **wrong** for Moments-edit stacks, where Phase C uploads a rendered JPEG as a new asset and stacks it with the original. On the server side the rendered child becomes the stack primary (Immich convention); on the user side that's the wrong artifact to surface — the rendered JPEG carries no editable state, while the original *plus* the local `edits` row is what the user thinks of as "their photo with an edit applied".

The local `edits` table remains the source of truth for what edit was applied. The server-side render is a sync artifact, not a first-class user-visible asset.

**Resolution (Phase C, as-built #224):**

Migration `026_add_media_is_moments_render.sql` adds an `is_moments_render INTEGER NOT NULL DEFAULT 0` column on `media`. Phase C save sets it to `1` when inserting the rendered media row; a future pull-side change will toggle it based on the asset's `tags[]` (when tag sync lands as part of Phase D).

The grid filter in `MediaRepository::list`, `get_many`, and the per-album / per-person queries got the §8.2 extension:

```sql
LEFT JOIN stacks s ON m.stack_id = s.id
LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
WHERE (
  s.id IS NULL                                    -- not stacked → show
  OR (render.id IS NULL                            -- ordinary stack → use server primary
      AND s.primary_asset_id = m.id)
  OR (render.id IS NOT NULL                        -- Moments-edit stack → surface the
      AND m.is_moments_render = 0)                 --   non-render sibling
)
```

The final form is slightly tighter than the draft sketch: instead of `m.id != render.id AND m.stack_id = s.id`, we use `m.is_moments_render = 0`. Because the `render` join only matches rows with `is_moments_render = 1`, the WHERE clause's third branch implicitly excludes the rendered child via the column on `m` itself, with no second equality check needed.

Thumbnail and viewer paths render the original through `RenderPipeline` with the local `edits` row applied — the same on-the-fly edit pipeline already used for local-only edits today. The rendered JPEG asset is never user-visible inside Moments; it's strictly a sync artifact.

Re-opening the editor loads the local `edits` row, not the rendered bytes. (Phase D recovery is the only path that ever parses the rendered JPEG's XMP — to rebuild a missing local `edits` row.)

**Does Phase A need to change to make Phase C work?** No. Phase A's primary-only rule is preserved as the second WHERE branch; Phase C just adds the third branch on top.

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

Independent of Phase A; landed in parallel.

- [x] §10.3 / §11.1 probe — corrected nested-`parameters` payload accepted on v2.7.5; PUT returns 200 with stamped edit IDs, DELETE returns 204
- [x] `ImmichClient::put_asset_edits` / `delete_asset_edits` (typed wrappers)
- [x] `Mutation::AssetEditsApplied { id }` / `AssetEditsCleared { id }` — **payload-free**, see §4.3
- [x] `OutboxMutation::from_row` decoders + round-trip tests
- [x] `PushManager::push_one` arms (§4.4) — read EditState at drain, project, decide PUT/DELETE/skip
- [x] `AssetEditHandler` / `AssetEditDeleteHandler` for `SyncAssetEditV1` / `SyncAssetEditDeleteV1` (§4.2)
- [x] `migrations/025_add_immich_asset_edits.sql` — provider-scoped per-action cache
- [x] `edit_action::{project, recompose}` in `sync/providers/immich/` — never leaks into library
- [x] `EditingService` records on save/revert; recorder injected via `Library::open`
- [x] `SyncContext.db` field for handlers that maintain provider-specific tables
- [x] Subscription to `"AssetEditsV1"` in `pull.rs` types vec
- [x] Unit tests: mutation round-trips, projection (incl. basis-swap, off-axis rotate), recompose (rotates sum, mirrors XOR, last crop wins, FK cascade), handler SQL helpers, push `lookup_media_dims`, EditingService recording behaviour

**Items dropped from earlier draft**: `get_asset_edits` (no consumer in Phase B); `EditState::is_geometric_only()` (replaced by `Option` return on `project()`).

Acceptance: cropping a photo in the Moments editor + save → photo appears cropped in Immich web within a sync cycle; cropping the same photo on Immich web → Moments shows the crop after next pull; revert clears it from both sides.

### Phase C — pixel-adjustment edits via render+stack+tag+XMP

Depends on Phase A (stacks model). Independent of Phase B.

- [x] §10.3 probes resolved — `PUT /tags { tags: [...] }` idempotent, `POST /tags { name }` 400s on existing tags (use PUT exclusively); `POST /stacks { assetIds }` returns `{id, primaryAssetId, assets}`; `DELETE /stacks/{id}/assets/{aid}` returns 204; **§11.2 XMP-strip question resolved: XMP block survives upload+download bit-exactly on v2.7.5** — Phase D recovery is viable.
- [x] `src/renderer/xmp.rs` encoder/decoder + JPEG APP1 walker (skips past existing EXIF segment); 13 tests including bit-exact roundtrip
- [x] `Mutation::StackCreated`, `StackMemberRemoved`, `AssetTaggedMomentsEdit`, `AssetUntaggedMomentsEdit` variants + outbox round-trips
- [x] `OutboxMutation::from_row` decoders + tests
- [x] `ImmichClient` typed wrappers: `post_stack`, `delete_stack_member`, `ensure_tags`, `add_assets_to_tag`, `remove_assets_from_tag`
- [x] `PushManager::push_one` arms (§4.4) + `ensure_moments_edit_tag` helper with `Mutex<Option<String>>` session cache + `persist_stack_locally` + strict `require_media_external_id`
- [x] `EditState::has_pixel_adjustments()` predicate (domain method; routes UI between Phase B and Phase C save paths) + tests
- [x] `migrations/026_add_media_is_moments_render.sql` + column on `media` row / `MediaItem` / `MediaRecord`
- [x] `Library::save_pixel_edit` orchestrator (§7.2) — UI client renders + JPEG-encodes; Library handles XMP encode/inject + sharded write + media-row insert + mutation sequence
- [x] `Library::revert_edit` orchestrator (§7.3) — stack/tag/delete cleanup + AssetEditsCleared as belt-and-braces
- [x] `MediaClientV2::save_edit_state` / `revert_edits` branch on `has_pixel_adjustments`
- [x] §8.2 grid filter override (`is_moments_render` exception) applied to `MediaRepository::list`, `get_many`, the album per-album queries, and the faces `list_media_for_person`
- [x] Unit tests: XMP roundtrip + APP1 walker, push helpers (`require_media_external_id`, `persist_stack_locally`), library orchestrators (`save_pixel_edit_*`, `revert_edit_*`), grid filter (Moments-edit and ordinary stack cases)

**Items deferred to Phase D**: pull-side toggling of `is_moments_render` based on `AssetV1.tags[]` (requires tag sync, currently a no-op on insert from sync), eager tag-based discovery (`GET /search/metadata?tagIds=…`).

Acceptance: applying a vignette in the Moments editor + save → original photo gets stacked with a new rendered asset on Immich; the rendered asset is the stack primary on the server, has the `moments-edit` tag, and contains the XMP block; locally the grid surfaces the original (§8.2 override). Revert removes the rendered asset and the stack auto-collapses.

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

`http://localhost:2283`, API key at `~/.config/immich-test/api_key`.

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

**Resolved (2026-05-08, Phase B kickoff)**. Probed against the local v2.7.5 instance:

- `PUT /assets/{id}/edits` with `{"edits":[{"action":"rotate","parameters":{"angle":90}}]}` returns 200 with the stamped edit list in the response body. Crop, mirror, and combined multi-action arrays all accepted.
- `DELETE /assets/{id}/edits` returns 204; `isEdited` flips back to false.
- PUT replaces the full edit list (matches design wording).
- `GET /api/assets/{id}` returns `edits: null` even when `isEdited:true` — the detail endpoint doesn't echo edits, so pulls must come via the sync stream (matches §4.2).

### 11.2 Does Immich strip XMP from uploaded JPEGs?

**Resolved (2026-05-10, Phase C kickoff)**. Probed against the local v2.7.5 instance: built a minimal JPEG with a custom XMP APP1 segment carrying a `MOMENTS_PROBE_2026_05_10` marker, uploaded via `POST /assets`, fetched the bytes back via `GET /assets/{id}/original`. `cmp` reports the downloaded bytes are identical to the uploaded bytes. The MOMENTS marker survives unchanged. Phase D recovery via the embedded XMP is viable on v2.7.5; worth re-checking if a future Immich version changes ingestion behaviour.

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
