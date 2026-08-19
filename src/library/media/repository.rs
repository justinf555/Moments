// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use super::model::{MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord, MediaType, Stack};
use crate::library::db::{id_placeholders, Database, LibraryStats};
use crate::library::error::LibraryError;

/// Internal row type for `list_media` — maps SQLite columns to Rust types.
///
/// `pub(crate)` so that per-concept repository modules (e.g. `AlbumRepository`)
/// can reuse the same row mapping and `into_item()` conversion.
#[derive(sqlx::FromRow)]
pub(crate) struct MediaRow {
    id: String,
    taken_at: Option<i64>,
    imported_at: i64,
    original_filename: String,
    width: Option<i64>,
    height: Option<i64>,
    orientation: i64,
    media_type: i64,
    is_favorite: i64,
    is_trashed: i64,
    trashed_at: Option<i64>,
    duration_ms: Option<i64>,
    stack_id: Option<String>,
    is_moments_render: i64,
}

impl MediaRow {
    pub(crate) fn into_item(self) -> MediaItem {
        MediaItem {
            id: MediaId::new(self.id),
            taken_at: self.taken_at,
            imported_at: self.imported_at,
            original_filename: self.original_filename,
            width: self.width,
            height: self.height,
            orientation: self.orientation as u8,
            media_type: if self.media_type == 1 {
                MediaType::Video
            } else {
                MediaType::Image
            },
            is_favorite: self.is_favorite != 0,
            is_trashed: self.is_trashed != 0,
            trashed_at: self.trashed_at,
            duration_ms: self.duration_ms.map(|v| v as u64),
            stack_id: self.stack_id,
            is_moments_render: self.is_moments_render != 0,
        }
    }
}

/// Media persistence layer.
///
/// Encapsulates all `media`-table SQL queries. Used by the `MediaService`
/// (and by sync extensions) — never accessed from the UI layer directly.
#[derive(Clone)]
pub struct MediaRepository {
    db: Database,
}

impl MediaRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    // ── Read queries ─────────────────────────────────────────────────

    /// Return `true` if an asset with this [`MediaId`] is already stored.
    pub async fn exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM media WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    /// Return `true` if an asset with this content hash already exists (dedup check).
    pub async fn exists_by_content_hash(&self, hash: &str) -> Result<bool, LibraryError> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM media WHERE content_hash = ?")
            .bind(hash)
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    /// Fetch the full media row by ID, including columns omitted from
    /// [`MediaItem`] (content_hash, external_id, relative_path,
    /// file_size). Used by Phase C save to populate the embedded XMP
    /// block and by other paths that need the full row.
    pub async fn get_record(&self, id: &MediaId) -> Result<Option<MediaRecord>, LibraryError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: String,
            content_hash: Option<String>,
            external_id: Option<String>,
            relative_path: String,
            original_filename: String,
            file_size: i64,
            imported_at: i64,
            media_type: i64,
            taken_at: Option<i64>,
            width: Option<i64>,
            height: Option<i64>,
            orientation: i64,
            duration_ms: Option<i64>,
            is_favorite: i64,
            is_trashed: i64,
            trashed_at: Option<i64>,
            is_moments_render: i64,
        }

        let row: Option<Row> = sqlx::query_as(
            "SELECT id, content_hash, external_id, relative_path, original_filename,
                    file_size, imported_at, media_type, taken_at, width, height,
                    orientation, duration_ms, is_favorite, is_trashed, trashed_at,
                    is_moments_render
             FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(row.map(|r| MediaRecord {
            id: MediaId::new(r.id),
            content_hash: r.content_hash,
            external_id: r.external_id,
            relative_path: r.relative_path,
            original_filename: r.original_filename,
            file_size: r.file_size,
            imported_at: r.imported_at,
            media_type: if r.media_type == 1 {
                MediaType::Video
            } else {
                MediaType::Image
            },
            taken_at: r.taken_at,
            width: r.width,
            height: r.height,
            orientation: r.orientation as u8,
            duration_ms: r.duration_ms.map(|v| v as u64),
            is_favorite: r.is_favorite != 0,
            is_trashed: r.is_trashed != 0,
            trashed_at: r.trashed_at,
            is_moments_render: r.is_moments_render != 0,
        }))
    }

    /// Fetch a single media item by ID.
    pub async fn get(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, taken_at, imported_at, original_filename,
                    width, height, orientation, media_type, is_favorite,
                    is_trashed, trashed_at, duration_ms, stack_id, is_moments_render
             FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.map(MediaRow::into_item))
    }

    /// Fetch media items for a batch of IDs in one query.
    ///
    /// IDs that don't match any row are silently absent from the result;
    /// callers compare the returned vec length with the request length to
    /// detect missing rows.
    ///
    /// Issue #224: applies the same primary-only stack filter as
    /// [`Self::list`]. Non-primary stack members are absent from the
    /// result so `MediaClientV2`'s reconciliation path treats them
    /// the same way it treats deleted rows — removed from any tracked
    /// grid model. This keeps the grid in sync with stack membership
    /// changes without a separate event channel.
    pub async fn get_many(&self, ids: &[MediaId]) -> Result<Vec<MediaItem>, LibraryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                    m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                    m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id, m.is_moments_render
             FROM media m
             LEFT JOIN stacks s ON m.stack_id = s.id
             LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
             WHERE m.id IN ({placeholders})
               AND (s.id IS NULL AND m.is_moments_render = 0
                    OR (render.id IS NULL AND s.primary_asset_id = m.id)
                    OR (render.id IS NOT NULL AND m.is_moments_render = 0))"
        );
        let mut query = sqlx::query_as::<_, MediaRow>(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        let rows: Vec<MediaRow> = query
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(MediaRow::into_item).collect())
    }

    /// For a batch of `media` ids about to be deleted, return the
    /// surviving sibling ids that the FK cascade chain will rejoin to
    /// the un-stacked timeline.
    ///
    /// Issue #224: when a stack primary is permanently deleted, the
    /// `stacks` row cascades away and surviving members get
    /// `stack_id` SET NULL. Those siblings need a `MediaEvent::Updated`
    /// so they reappear in the live grid. The service-layer delete
    /// path captures this list before the delete fires.
    ///
    /// Excludes ids that are themselves being deleted, and dedupes.
    pub async fn siblings_freed_by_deletion(
        &self,
        deleted_ids: &[MediaId],
    ) -> Result<Vec<MediaId>, LibraryError> {
        if deleted_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(deleted_ids.len());
        let sql = format!(
            "SELECT DISTINCT m.id FROM media m
             JOIN stacks s ON m.stack_id = s.id
             WHERE s.primary_asset_id IN ({placeholders})
               AND m.id NOT IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (String,)>(&sql);
        for id in deleted_ids {
            q = q.bind(id.as_str());
        }
        for id in deleted_ids {
            q = q.bind(id.as_str());
        }
        let rows: Vec<(String,)> = q
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Return the local `MediaId`s of every member of a stack
    /// (regardless of whether each member is the primary). Used by
    /// the service layer to fan out `MediaEvent::Updated` to all
    /// affected rows when a stack is mutated server-side.
    pub async fn list_stack_members(&self, stack_id: &str) -> Result<Vec<MediaId>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM media WHERE stack_id = ?")
            .bind(stack_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Return the `original_filename` column for `id`, or `None` if no row exists.
    pub async fn original_filename(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT original_filename FROM media WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return the `relative_path` column for `id`, or `None` if no row exists.
    ///
    /// Used by backends to construct the absolute original-file path.
    pub async fn relative_path(&self, id: &MediaId) -> Result<Option<String>, LibraryError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT relative_path FROM media WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return path resolution fields for a single asset in one query.
    pub async fn resolve_info(
        &self,
        id: &MediaId,
    ) -> Result<Option<(String, String, Option<String>)>, LibraryError> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT relative_path, original_filename, external_id FROM media WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row)
    }

    /// Return a page of [`MediaItem`]s in reverse chronological order.
    pub async fn list(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        // Issue #224: stacked-but-not-primary members are hidden from
        // every grid view. The `LEFT JOIN stacks` keeps un-stacked rows
        // visible while collapsing each stack to its visible representative.
        //
        // Phase A's filter was a plain primary-only check. Phase C §8.2
        // adds the override for Moments-edit stacks: when the stack
        // contains a Moments-render member (the rendered JPEG sibling
        // we uploaded for a pixel-adjustment edit), surface the
        // *other* sibling — the original — so the timeline shows the
        // artifact the user can still edit. The extra `LEFT JOIN media
        // render … is_moments_render = 1` is the existence test for
        // that override branch.
        //
        // Bare column references are qualified with `m.` to disambiguate
        // from `stacks.id` / `stacks.primary_asset_id` (and now `render.*`).
        let (filter_clause, sort_expr) = match &filter {
            MediaFilter::All => (" AND m.is_trashed = 0", "COALESCE(m.taken_at, 0)"),
            MediaFilter::Favorites => (
                " AND m.is_trashed = 0 AND m.is_favorite = 1",
                "COALESCE(m.taken_at, 0)",
            ),
            MediaFilter::Trashed => (" AND m.is_trashed = 1", "COALESCE(m.trashed_at, 0)"),
            MediaFilter::RecentImports { .. } => (
                " AND m.is_trashed = 0 AND m.imported_at > ?",
                "m.imported_at",
            ),
            MediaFilter::Album { .. } => (
                " AND m.is_trashed = 0 AND m.id IN (SELECT media_id FROM album_media WHERE album_id = ?)",
                "COALESCE(m.taken_at, 0)",
            ),
            MediaFilter::Person { .. } => (
                " AND m.is_trashed = 0 AND m.id IN (SELECT DISTINCT asset_id FROM asset_faces WHERE person_id = ?)",
                "COALESCE(m.taken_at, 0)",
            ),
        };

        let extra_bind: Option<String> = match &filter {
            MediaFilter::RecentImports { since } => Some(since.to_string()),
            MediaFilter::Album { album_id } => Some(album_id.as_str().to_owned()),
            MediaFilter::Person { person_id } => Some(person_id.as_str().to_owned()),
            _ => None,
        };

        let columns = "m.id, m.taken_at, m.imported_at, m.original_filename,
                        m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                        m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id, m.is_moments_render";

        let rows = match cursor {
            None => {
                let sql = format!(
                    "SELECT {columns}
                     FROM media m
                     LEFT JOIN stacks s ON m.stack_id = s.id
                     LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
                     WHERE (s.id IS NULL AND m.is_moments_render = 0
                            OR (render.id IS NULL AND s.primary_asset_id = m.id)
                            OR (render.id IS NOT NULL AND m.is_moments_render = 0)){filter_clause}
                     ORDER BY {sort_expr} DESC, m.id DESC
                     LIMIT ?"
                );
                let mut q = sqlx::query_as::<_, MediaRow>(&sql);
                if let Some(ref val) = extra_bind {
                    q = q.bind(val.as_str());
                }
                q.bind(limit as i64)
                    .fetch_all(self.db.pool())
                    .await
                    .map_err(LibraryError::Db)?
            }
            Some(cur) => {
                let sql = format!(
                    "SELECT {columns}
                     FROM media m
                     LEFT JOIN stacks s ON m.stack_id = s.id
                     LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
                     WHERE (s.id IS NULL AND m.is_moments_render = 0
                            OR (render.id IS NULL AND s.primary_asset_id = m.id)
                            OR (render.id IS NOT NULL AND m.is_moments_render = 0))
                       AND ({sort_expr} < ?
                            OR ({sort_expr} = ? AND m.id < ?)){filter_clause}
                     ORDER BY {sort_expr} DESC, m.id DESC
                     LIMIT ?"
                );
                let mut q = sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(cur.sort_key)
                    .bind(cur.sort_key)
                    .bind(cur.id.as_str());
                if let Some(ref val) = extra_bind {
                    q = q.bind(val.as_str());
                }
                q.bind(limit as i64)
                    .fetch_all(self.db.pool())
                    .await
                    .map_err(LibraryError::Db)?
            }
        };

        Ok(rows.into_iter().map(MediaRow::into_item).collect())
    }

    /// Return IDs of items trashed longer than `max_age_secs` ago.
    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM media WHERE is_trashed = 1 AND trashed_at < ?")
                .bind(cutoff)
                .fetch_all(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Return aggregate library statistics for the preferences overview.
    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                COUNT(CASE WHEN media_type = 0 AND is_trashed = 0 THEN 1 END),
                COUNT(CASE WHEN media_type = 1 AND is_trashed = 0 THEN 1 END),
                COALESCE(SUM(CASE WHEN is_trashed = 0 THEN file_size ELSE 0 END), 0),
                COUNT(CASE WHEN is_trashed = 1 THEN 1 END)
             FROM media",
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        let album_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM albums")
            .fetch_one(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;

        let people_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM people WHERE name != '' AND is_hidden = 0")
                .fetch_one(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        Ok(LibraryStats {
            photo_count: row.0 as u64,
            video_count: row.1 as u64,
            album_count: album_count.0 as u64,
            total_file_size: row.2 as u64,
            trashed_count: row.3 as u64,
            cache_used_bytes: 0,
            people_count: people_count.0 as u64,
            server: None,
        })
    }

    // ── Write queries ────────────────────────────────────────────────

    /// Persist a newly imported media asset record.
    pub async fn insert(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO media (id, content_hash, external_id, relative_path,
                                original_filename, file_size, imported_at, media_type,
                                taken_at, width, height, orientation, duration_ms,
                                is_favorite, is_trashed, trashed_at, is_moments_render)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.content_hash)
        .bind(&record.external_id)
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(record.imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .bind(record.is_moments_render as i64)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Upsert a media record (used by sync).
    ///
    /// Uses `INSERT OR REPLACE` so existing records are fully overwritten.
    /// Before inserting, removes any existing row whose `external_id`
    /// matches the incoming `id` — this handles the case where a locally
    /// imported asset (local UUID) was uploaded to Immich and the server
    /// now streams it back with its own UUID as the `id`.
    ///
    /// Returns the id of the replaced local row if one was deleted by the
    /// external_id match, so the caller can emit a `Removed` event for it.
    /// Without this, the UI would still hold a model item keyed on the
    /// local id even though the row has been replaced server-side
    /// (issue #610).
    ///
    /// `imported_at` is treated as a **local-only** field: when an existing
    /// row is being replaced (matched by id, or by external_id during the
    /// local→server UUID swap), its `imported_at` is preserved instead of
    /// being overwritten by the incoming value. This stops sync from
    /// retroactively rewriting "when this asset entered my library" to
    /// the server's `file_created_at` (the photo's capture time), which
    /// would silently move assets out of the Recent Imports view (issue #614).
    pub async fn upsert(&self, record: &MediaRecord) -> Result<Option<MediaId>, LibraryError> {
        // Capture the existing row's locally-only fields so we can
        // preserve them across the INSERT OR REPLACE:
        //
        // * `imported_at` — see #614. The server's `file_created_at`
        //   is the photo's capture time, not when it entered this
        //   library; overwriting moves assets out of Recent Imports.
        // * `is_moments_render` — Phase C (#224). The Phase C save
        //   sets this to 1 on the rendered child it produces; sync's
        //   `AssetHandler` always passes `false` (pull-side tag-based
        //   derivation is Phase D scope). Without preserving the
        //   column, the §8.2 grid filter would flip the wrong
        //   sibling visible after the next pull.
        //
        // Post-#626 the upserting handler always passes the locally-owned
        // `MediaId` (resolved via `id_by_external_id` /
        // `id_by_content_hash_pending_push`), so a plain `id = ?` lookup
        // is sufficient; the older `OR external_id = ?` arm was for the
        // local→server UUID swap that no longer happens.
        let existing: Option<(i64, i64)> =
            sqlx::query_as("SELECT imported_at, is_moments_render FROM media WHERE id = ?")
                .bind(record.id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        let imported_at = existing.map(|(t, _)| t).unwrap_or(record.imported_at);
        let is_moments_render = existing
            .map(|(_, r)| r != 0)
            .unwrap_or(record.is_moments_render);

        // Atomic delete-and-return so the replaced-id observation can
        // never disagree with the row that was actually removed. SQLite
        // 3.35+ supports RETURNING; sqlx surfaces it via fetch_optional.
        let replaced: Option<String> =
            sqlx::query_scalar("DELETE FROM media WHERE external_id = ? AND id != ? RETURNING id")
                .bind(record.id.as_str())
                .bind(record.id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        sqlx::query(
            "INSERT OR REPLACE INTO media (id, content_hash, external_id, relative_path,
                                           original_filename, file_size, imported_at,
                                           media_type, taken_at, width, height,
                                           orientation, duration_ms, is_favorite,
                                           is_trashed, trashed_at, is_moments_render)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.content_hash)
        .bind(&record.external_id)
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .bind(is_moments_render as i64)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(replaced.map(MediaId::new))
    }

    /// Set or clear the favourite flag on one or more assets.
    pub async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let value: i64 = if favorite { 1 } else { 0 };
        let placeholders = id_placeholders(ids.len());
        let sql = format!("UPDATE media SET is_favorite = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(value);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Look up a locally-imported row by `content_hash` that has not yet
    /// been pushed to a server (i.e. `external_id IS NULL`).
    ///
    /// Used by the sync handler to *adopt* a local row when the same
    /// asset arrives over the pull stream before push has finished
    /// stamping the server id. Without this step the AssetHandler would
    /// generate a fresh `MediaId` and `INSERT` a parallel stub row,
    /// then push's eventual `UPDATE … SET external_id = ?` would hit
    /// the unique partial index on `external_id` and fail.
    ///
    /// Restricting the match to `external_id IS NULL` is deliberate:
    /// rows already mapped to a *different* Immich asset must never be
    /// silently re-pointed by a hash collision.
    pub async fn id_by_content_hash_pending_push(
        &self,
        content_hash: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT id FROM media WHERE content_hash = ? AND external_id IS NULL",
        )
        .bind(content_hash)
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(row.map(MediaId::new))
    }

    /// Look up the local [`MediaId`] for a given `external_id`.
    ///
    /// Used by sync handlers to translate Immich-side asset UUIDs into the
    /// stable, locally-owned id under which we actually store the row.
    /// Returns `None` if no row carries that `external_id`.
    pub async fn id_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<MediaId>, LibraryError> {
        let row: Option<String> = sqlx::query_scalar("SELECT id FROM media WHERE external_id = ?")
            .bind(external_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(row.map(MediaId::new))
    }

    /// Look up external_ids for a batch of media IDs.
    ///
    /// Returns `(local_id, external_id)` pairs. IDs without an external_id
    /// are omitted from the result.
    pub async fn external_ids(
        &self,
        ids: &[MediaId],
    ) -> Result<Vec<(String, String)>, LibraryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "SELECT id, external_id FROM media WHERE id IN ({placeholders}) AND external_id IS NOT NULL"
        );
        let mut query = sqlx::query_as::<_, (String, String)>(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)
    }

    /// Move assets to the trash (soft delete).
    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let placeholders = id_placeholders(ids.len());
        let sql =
            format!("UPDATE media SET is_trashed = 1, trashed_at = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(now);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Restore trashed assets back to the library.
    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "UPDATE media SET is_trashed = 0, trashed_at = NULL WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Permanently delete assets and all related rows in a transaction.
    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        for (table, col) in [
            ("edits", "media_id"),
            ("asset_faces", "asset_id"),
            ("media_metadata", "media_id"),
            ("thumbnails", "media_id"),
            ("album_media", "media_id"),
            ("media", "id"),
        ] {
            let sql = format!("DELETE FROM {table} WHERE {col} IN ({placeholders})");
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(id.as_str());
            }
            query.execute(&mut *tx).await.map_err(LibraryError::Db)?;
        }
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return media ids whose heartbeat lags the given checkpoint and
    /// have a non-null `external_id`.
    ///
    /// Issue #628: callers (the reset-cycle orphan sweep) pass the
    /// returned ids to `delete_permanently_from_sync` so the removal
    /// goes through the recorder + on-disk file cleanup.
    /// `external_id IS NOT NULL` excludes locally-imported rows the
    /// server never knew about.
    pub async fn ids_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM media
             WHERE last_seen_at < ? AND external_id IS NOT NULL",
        )
        .bind(checkpoint)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| MediaId::new(id)).collect())
    }

    /// Update `last_seen_at` to the given unix timestamp for one media row.
    ///
    /// Issue #628: this is the heartbeat that the reset-cycle orphan
    /// sweep compares against. Sync paths call it whenever the server
    /// confirms an asset is still alive — pull `AssetV1` after the
    /// upsert, push completion after stamping `external_id`, and any
    /// server-confirmed write-through (favorite, restore). Rows whose
    /// heartbeat lags the cycle's checkpoint and have a non-null
    /// `external_id` are treated as deleted server-side.
    ///
    /// Missing row is a no-op — it was deleted under us, which is fine.
    pub async fn bump_last_seen_at(&self, id: &MediaId, now: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE media SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Stacks (issue #224) ─────────────────────────────────────────

    /// Upsert a stack row. Idempotent — reuses the existing id and
    /// overwrites `primary_asset_id` if it changed (e.g. the user
    /// re-stacked on the server). Does not bump `last_seen_at`; the
    /// caller does that separately so the heartbeat write stays
    /// uniform across all sync paths.
    pub async fn upsert_stack(&self, stack: &Stack) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO stacks (id, primary_asset_id, last_seen_at)
             VALUES (?, ?, COALESCE((SELECT last_seen_at FROM stacks WHERE id = ?), 0))
             ON CONFLICT(id) DO UPDATE SET primary_asset_id = excluded.primary_asset_id",
        )
        .bind(&stack.id)
        .bind(stack.primary_asset_id.as_str())
        .bind(&stack.id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Ensure a `stacks` row exists for `stack_id`, creating a stub
    /// pointing at `primary_fallback` if it doesn't.
    ///
    /// Used by `AssetHandler` when it sees an `AssetV1.stackId` for a
    /// stack we haven't received the matching `StackV1` for yet — the
    /// FK on `media.stack_id REFERENCES stacks(id)` is enforced
    /// (sqlx 0.8 enables `PRAGMA foreign_keys` by default), so we
    /// can't bind the asset to a non-existent stack. The stub uses
    /// the current asset as a placeholder primary; `StackV1` later
    /// overwrites it via `upsert_stack`'s ON CONFLICT clause.
    /// Idempotent — `ON CONFLICT DO NOTHING` preserves any stub or
    /// authoritative row already present.
    ///
    /// `now` seeds the stub's `last_seen_at`. Issue #628 reset-cycle
    /// reconciliation: if we left the stub at `0`, the same cycle's
    /// sweep at `finish_sync` would delete it before the matching
    /// `StackV1` got a chance to bump it (or before it has a chance
    /// to arrive at all). Seeding with the cycle's wall time keeps
    /// the stub alive for at least one cycle; if `StackV1` never
    /// arrives, the next reset-cycle's checkpoint will be `> now` and
    /// the orphan sweep will clean it up.
    pub async fn ensure_stack_stub(
        &self,
        stack_id: &str,
        primary_fallback: &MediaId,
        now: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO stacks (id, primary_asset_id, last_seen_at)
             VALUES (?, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(stack_id)
        .bind(primary_fallback.as_str())
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Point a media row at a stack. Used when the server announces
    /// stack membership through `AssetV1.stackId`.
    ///
    /// Returns `true` if the row's `stack_id` actually changed. The
    /// SQL guards on `(stack_id IS NULL OR stack_id != ?)` so the
    /// UPDATE is a no-op when the asset is already bound to the
    /// requested stack — saves a redundant `MediaEvent::Updated` on
    /// the steady-state re-sync of an unchanged stack membership.
    pub async fn set_media_stack_id(
        &self,
        media_id: &MediaId,
        stack_id: &str,
    ) -> Result<bool, LibraryError> {
        let result = sqlx::query(
            "UPDATE media SET stack_id = ?
             WHERE id = ? AND (stack_id IS NULL OR stack_id != ?)",
        )
        .bind(stack_id)
        .bind(media_id.as_str())
        .bind(stack_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.rows_affected() > 0)
    }

    /// Clear the stack pointer on a media row. Used when an `AssetV1`
    /// arrives with `stackId: null` (the asset was un-stacked
    /// server-side).
    ///
    /// Returns `true` if the row's `stack_id` actually changed. The
    /// SQL guards on `stack_id IS NOT NULL` so re-sync of an
    /// already-un-stacked asset (the common case for the vast
    /// majority of `AssetV1` payloads) doesn't emit a spurious event.
    pub async fn clear_media_stack_id(&self, media_id: &MediaId) -> Result<bool, LibraryError> {
        let result = sqlx::query(
            "UPDATE media SET stack_id = NULL
             WHERE id = ? AND stack_id IS NOT NULL",
        )
        .bind(media_id.as_str())
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.rows_affected() > 0)
    }

    /// Update a stack's heartbeat. Issue #628 reset-cycle reconciliation
    /// — stacks join the four pre-existing tables (media, albums,
    /// people, asset_faces) in the orphan sweep.
    pub async fn bump_stack_last_seen_at(
        &self,
        stack_id: &str,
        now: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE stacks SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(stack_id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return stack ids whose heartbeat lags the given checkpoint.
    /// Unlike media/albums, stacks have no local-only counterpart —
    /// they only exist as a server projection — so no `external_id`
    /// gate is needed.
    pub async fn ids_with_stale_stack_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM stacks WHERE last_seen_at < ?")
            .bind(checkpoint)
            .fetch_all(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Delete one stack by id, rejoining its members to the un-stacked
    /// timeline. The matching path for `SyncStackDeleteV1` events.
    /// Idempotent — a missing stack id is a no-op.
    ///
    /// `media.stack_id REFERENCES stacks(id) ON DELETE SET NULL`
    /// (migration 023) cascades automatically with `PRAGMA
    /// foreign_keys` enabled (the sqlx 0.8 default). The explicit
    /// `UPDATE … SET stack_id = NULL` here is defensive — if FK
    /// enforcement were ever flipped off, this still does the right
    /// thing — but on a current build it's a redundant no-op.
    pub async fn delete_stack(&self, stack_id: &str) -> Result<(), LibraryError> {
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        sqlx::query("UPDATE media SET stack_id = NULL WHERE stack_id = ?")
            .bind(stack_id)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        sqlx::query("DELETE FROM stacks WHERE id = ?")
            .bind(stack_id)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete stacks whose heartbeat lags the checkpoint, rejoining
    /// each member to the un-stacked timeline. Returns the removed
    /// stack ids for logging.
    ///
    /// As with `delete_stack`, the explicit `UPDATE` is defensive
    /// against future FK pragma changes; under `PRAGMA foreign_keys = ON`
    /// (sqlx 0.8 default) the `media.stack_id` `ON DELETE SET NULL`
    /// cascade does the same work automatically.
    pub async fn delete_stacks_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let ids = self.ids_with_stale_stack_heartbeat(checkpoint).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;

        let unbind_sql =
            format!("UPDATE media SET stack_id = NULL WHERE stack_id IN ({placeholders})");
        let mut unbind = sqlx::query(&unbind_sql);
        for id in &ids {
            unbind = unbind.bind(id);
        }
        unbind.execute(&mut *tx).await.map_err(LibraryError::Db)?;

        let delete_sql = format!("DELETE FROM stacks WHERE id IN ({placeholders})");
        let mut delete = sqlx::query(&delete_sql);
        for id in &ids {
            delete = delete.bind(id);
        }
        delete.execute(&mut *tx).await.map_err(LibraryError::Db)?;

        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::*;
    use tempfile::tempdir;

    async fn test_repo(dir: &std::path::Path) -> (MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = MediaRepository::new(db.clone());
        (repo, db)
    }

    #[tokio::test]
    async fn exists_returns_false_initially() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("a".repeat(64));
        assert!(!repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn insert_and_exists_roundtrip() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("b".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        assert!(repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn get_returns_inserted_item() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("c".repeat(64));
        repo.insert(&record_with_taken_at(id.clone(), "photo.jpg", Some(5000)))
            .await
            .unwrap();
        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.id, id);
        assert_eq!(item.taken_at, Some(5000));
    }

    /// Phase C: `is_moments_render` must round-trip through INSERT
    /// and SELECT correctly. Silent column drift here would surface as
    /// the §8.2 grid filter flipping the wrong sibling visible.
    #[tokio::test]
    async fn is_moments_render_round_trips_insert_and_get() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        // is_moments_render = false (the default for ordinary imports)
        let unset_id = MediaId::new("a".repeat(64));
        let mut unset_rec = test_record(unset_id.clone());
        unset_rec.relative_path = "unset.jpg".into();
        repo.insert(&unset_rec).await.unwrap();
        let item = repo.get(&unset_id).await.unwrap().unwrap();
        assert!(!item.is_moments_render);

        // is_moments_render = true (Phase C save sets this)
        let render_id = MediaId::new("b".repeat(64));
        let mut rec = test_record(render_id.clone());
        rec.relative_path = "render.jpg".into();
        rec.is_moments_render = true;
        repo.insert(&rec).await.unwrap();
        let item = repo.get(&render_id).await.unwrap().unwrap();
        assert!(item.is_moments_render);

        // get_record reflects the same value.
        let full = repo.get_record(&render_id).await.unwrap().unwrap();
        assert!(full.is_moments_render);
    }

    /// Phase C blocker #2: `upsert` is used by the pull-side
    /// `AssetHandler`, which constructs the record with
    /// `is_moments_render: false` (Phase D will derive it from tags).
    /// Without preservation, every pull would clobber the Phase-C-set
    /// `true` and break §8.2.
    #[tokio::test]
    async fn upsert_preserves_existing_is_moments_render() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("c".repeat(64));

        // Insert with the Phase C save flag set.
        let mut rec = test_record(id.clone());
        rec.is_moments_render = true;
        repo.insert(&rec).await.unwrap();

        // Upsert with the column false (simulating pull from Immich).
        let mut sync_rec = test_record(id.clone());
        sync_rec.is_moments_render = false;
        repo.upsert(&sync_rec).await.unwrap();

        // The locally-set flag survives the pull.
        let item = repo.get(&id).await.unwrap().unwrap();
        assert!(
            item.is_moments_render,
            "upsert must preserve the local is_moments_render flag"
        );
    }

    #[tokio::test]
    async fn list_ordered_reverse_chronological() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id_a = MediaId::new("a".repeat(64));
        let id_b = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(id_a.clone(), "a.jpg", Some(1000)))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(id_b.clone(), "b.jpg", Some(3000)))
            .await
            .unwrap();
        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        assert_eq!(items[0].id, id_b);
        assert_eq!(items[1].id, id_a);
    }

    #[tokio::test]
    async fn set_favorite_and_read_back() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("d".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        assert!(!repo.list(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
        repo.set_favorite(std::slice::from_ref(&id), true)
            .await
            .unwrap();
        assert!(repo.list(MediaFilter::All, None, 10).await.unwrap()[0].is_favorite);
    }

    #[tokio::test]
    async fn trash_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("e".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.trash(std::slice::from_ref(&id)).await.unwrap();
        assert!(repo
            .list(MediaFilter::All, None, 10)
            .await
            .unwrap()
            .is_empty());
        repo.restore(&[id]).await.unwrap();
        assert_eq!(
            repo.list(MediaFilter::All, None, 10).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn delete_permanently_removes_row() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("f".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.delete_permanently(std::slice::from_ref(&id))
            .await
            .unwrap();
        assert!(!repo.exists(&id).await.unwrap());
    }

    #[tokio::test]
    async fn upsert_inserts_and_replaces() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("g".repeat(64));
        let mut record = test_record(id.clone());
        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none(), "fresh insert returns None");
        assert!(repo.exists(&id).await.unwrap());

        record.original_filename = "updated.jpg".to_string();
        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none(), "same-id upsert returns None");
        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.original_filename, "updated.jpg");
    }

    /// Round-trip a locally-imported asset that gets uploaded to Immich
    /// and streamed back with the server's UUID — `upsert` must report
    /// the local id it just replaced so the service can emit a
    /// `Removed` event for it (issue #610).
    #[tokio::test]
    async fn upsert_returns_replaced_local_id_when_external_id_matches() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        // Local row keyed on a local UUID, with the server UUID stored
        // as external_id (this is the state after a successful push).
        let local_id = MediaId::new("local-uuid-aaaaaaaaaaaaaaaaaaaa".to_string());
        let server_id = MediaId::new("server-uuid-bbbbbbbbbbbbbbbbbbbb".to_string());
        let mut local = test_record(local_id.clone());
        local.external_id = Some(server_id.as_str().to_string());
        repo.insert(&local).await.unwrap();

        // Pull-sync now upserts the asset keyed on the server UUID.
        let mut from_server = test_record(server_id.clone());
        from_server.external_id = Some(server_id.as_str().to_string());
        // Different relative_path is fine — the upsert REPLACEs.
        from_server.relative_path = "from-server.jpg".to_string();

        let replaced = repo.upsert(&from_server).await.unwrap();

        assert_eq!(
            replaced.as_ref().map(|i| i.as_str()),
            Some(local_id.as_str())
        );
        assert!(
            !repo.exists(&local_id).await.unwrap(),
            "old local row deleted"
        );
        assert!(
            repo.exists(&server_id).await.unwrap(),
            "new server row inserted"
        );
    }

    /// Adopt-by-content_hash: pull's `AssetHandler` looks up a local
    /// row that has the same hash but no `external_id`, so the same
    /// asset arriving from sync before push has stamped the server id
    /// is merged in place. A row that *already* has an `external_id`
    /// — i.e. is mapped to a different (or even the same) Immich
    /// asset — must never be returned, since silently re-pointing it
    /// would corrupt the mapping.
    #[tokio::test]
    async fn id_by_content_hash_pending_push_only_matches_unstamped_rows() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let hash = "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=".to_string();

        // Locally-imported, push hasn't run yet → eligible for adoption.
        let pending = MediaId::new("local-pending-aaaaaaaaaaaaaaaaaaa".to_string());
        let mut pending_rec = test_record(pending.clone());
        pending_rec.content_hash = Some(hash.clone());
        pending_rec.external_id = None;
        repo.insert(&pending_rec).await.unwrap();

        let found = repo.id_by_content_hash_pending_push(&hash).await.unwrap();
        assert_eq!(found.as_ref().map(|m| m.as_str()), Some(pending.as_str()));

        // Now stamp external_id — the row must no longer be eligible.
        sqlx::query("UPDATE media SET external_id = ? WHERE id = ?")
            .bind("immich-uuid")
            .bind(pending.as_str())
            .execute(db.pool())
            .await
            .unwrap();

        let after_stamp = repo.id_by_content_hash_pending_push(&hash).await.unwrap();
        assert!(
            after_stamp.is_none(),
            "stamped rows must never be adopted by content_hash"
        );

        // A different hash never matches.
        let miss = repo.id_by_content_hash_pending_push("other").await.unwrap();
        assert!(miss.is_none());
    }

    /// Issue #626: sync handlers translate Immich UUIDs to local
    /// `MediaId`s via `id_by_external_id`. The lookup must return the
    /// row's primary key when `external_id` matches, and `None`
    /// otherwise.
    #[tokio::test]
    async fn id_by_external_id_finds_row_or_returns_none() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let local_id = MediaId::new("local-uuid-eeeeeeeeeeeeeeeeeeeeeee".to_string());
        let server_id = "server-uuid-ffffffffffffffffffffff".to_string();
        let mut rec = test_record(local_id.clone());
        rec.external_id = Some(server_id.clone());
        repo.insert(&rec).await.unwrap();

        let found = repo.id_by_external_id(&server_id).await.unwrap();
        assert_eq!(found.as_ref().map(|m| m.as_str()), Some(local_id.as_str()));

        let missing = repo.id_by_external_id("nope").await.unwrap();
        assert!(missing.is_none());
    }

    /// Upsert with an external_id that doesn't match any existing row
    /// must not report a phantom replacement.
    #[tokio::test]
    async fn upsert_returns_none_when_no_external_id_match() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let id = MediaId::new("just-a-fresh-row-cccccccccccccccccc".to_string());
        let mut record = test_record(id.clone());
        record.external_id = Some("server-uuid-not-otherwise-known".to_string());

        let replaced = repo.upsert(&record).await.unwrap();
        assert!(replaced.is_none());
    }

    /// Re-syncing an existing asset must not overwrite its `imported_at` —
    /// that field belongs to the local library's view of "when did this
    /// arrive", not the server's view of "when was it captured". See #614.
    #[tokio::test]
    async fn upsert_preserves_imported_at_for_existing_id() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("e".repeat(64));

        let mut record = test_record(id.clone());
        record.imported_at = 1_700_000_000; // original local import
        repo.insert(&record).await.unwrap();

        // Pull-sync upserts with a much older capture-time-derived value
        // (what the asset handler used to do).
        record.imported_at = 1_400_000_000;
        repo.upsert(&record).await.unwrap();

        let item = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(item.imported_at, 1_700_000_000);
    }

    #[tokio::test]
    async fn library_stats_counts() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        repo.insert(&record_with_taken_at(
            MediaId::new("a".repeat(64)),
            "a.jpg",
            Some(1000),
        ))
        .await
        .unwrap();
        repo.insert(&record_with_taken_at(
            MediaId::new("b".repeat(64)),
            "b.jpg",
            Some(2000),
        ))
        .await
        .unwrap();
        let stats = repo.library_stats().await.unwrap();
        assert_eq!(stats.photo_count, 2);
    }

    #[tokio::test]
    async fn bump_last_seen_at_writes_value() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;
        let id = MediaId::new("a".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();

        repo.bump_last_seen_at(&id, 12345).await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT last_seen_at FROM media WHERE id = ?")
            .bind(id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 12345);
    }

    #[tokio::test]
    async fn bump_last_seen_at_missing_id_is_noop() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;
        let id = MediaId::new("z".repeat(64));
        // No row exists; the UPDATE matches nothing. Must not error —
        // sync handlers may bump after a row was deleted under them.
        repo.bump_last_seen_at(&id, 12345).await.unwrap();
    }

    #[tokio::test]
    async fn ids_with_stale_heartbeat_finds_only_eligible_rows() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        // Server-sourced row, heartbeat older than checkpoint → orphan.
        let stale_synced = MediaId::new("a".repeat(64));
        let mut rec = record_with_taken_at(stale_synced.clone(), "stale.jpg", Some(1));
        rec.external_id = Some("immich-uuid-a".to_string());
        repo.insert(&rec).await.unwrap();
        repo.bump_last_seen_at(&stale_synced, 100).await.unwrap();

        // Server-sourced row, heartbeat newer than checkpoint → safe.
        let fresh_synced = MediaId::new("b".repeat(64));
        let mut rec = record_with_taken_at(fresh_synced.clone(), "fresh.jpg", Some(2));
        rec.external_id = Some("immich-uuid-b".to_string());
        repo.insert(&rec).await.unwrap();
        repo.bump_last_seen_at(&fresh_synced, 300).await.unwrap();

        // Local-only row (no external_id), heartbeat doesn't matter →
        // immune via the external_id IS NOT NULL filter.
        let local_only = MediaId::new("c".repeat(64));
        let mut rec = record_with_taken_at(local_only.clone(), "local.jpg", Some(3));
        rec.external_id = None;
        repo.insert(&rec).await.unwrap();
        // Heartbeat at 0 (insert default) is < checkpoint, but filter saves it.

        let orphans = repo.ids_with_stale_heartbeat(200).await.unwrap();

        assert_eq!(orphans.len(), 1, "only the stale synced row is orphaned");
        assert_eq!(orphans[0].as_str(), stale_synced.as_str());
    }

    // ── Stacks (issue #224) ─────────────────────────────────────────

    /// `upsert_stack` is idempotent on the id and overwrites the
    /// `primary_asset_id` when re-stacking happens server-side.
    /// `last_seen_at` must be preserved across upserts so heartbeat
    /// state isn't reset on every pull.
    #[tokio::test]
    async fn upsert_stack_is_idempotent_and_preserves_heartbeat() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let primary_a = MediaId::new("a".repeat(64));
        let primary_b = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary_a.clone(), "a.jpg", None))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(primary_b.clone(), "b.jpg", None))
            .await
            .unwrap();

        repo.upsert_stack(&Stack {
            id: "stk1".to_string(),
            primary_asset_id: primary_a.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("stk1", 555).await.unwrap();

        // Upsert again — primary changed (server re-pinned), heartbeat
        // must NOT be reset to 0.
        repo.upsert_stack(&Stack {
            id: "stk1".to_string(),
            primary_asset_id: primary_b.clone(),
        })
        .await
        .unwrap();

        let row: (String, i64) =
            sqlx::query_as("SELECT primary_asset_id, last_seen_at FROM stacks WHERE id = ?")
                .bind("stk1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, primary_b.as_str(), "primary updated by upsert");
        assert_eq!(row.1, 555, "heartbeat preserved across upsert");
    }

    /// Stack ids whose heartbeat lags the checkpoint are returned.
    /// Stacks have no `external_id` gate (server-only construct), so
    /// every stale row is eligible.
    #[tokio::test]
    async fn ids_with_stale_stack_heartbeat_returns_only_stale() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        repo.insert(&test_record(primary.clone())).await.unwrap();

        repo.upsert_stack(&Stack {
            id: "stale".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("stale", 100).await.unwrap();

        repo.upsert_stack(&Stack {
            id: "fresh".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.bump_stack_last_seen_at("fresh", 300).await.unwrap();

        let mut stale = repo.ids_with_stale_stack_heartbeat(200).await.unwrap();
        stale.sort();
        assert_eq!(stale, vec!["stale".to_string()]);
    }

    /// Deleting a stale stack must clear `media.stack_id` on its
    /// members via the migration's `ON DELETE SET NULL` cascade so
    /// surviving members rejoin the un-stacked timeline.
    #[tokio::test]
    async fn delete_stacks_with_stale_heartbeat_clears_member_stack_id() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let member = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary.clone(), "p.jpg", None))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(member.clone(), "m.jpg", None))
            .await
            .unwrap();

        repo.upsert_stack(&Stack {
            id: "doomed".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&primary, "doomed").await.unwrap();
        repo.set_media_stack_id(&member, "doomed").await.unwrap();
        repo.bump_stack_last_seen_at("doomed", 50).await.unwrap();

        let removed = repo.delete_stacks_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(removed, vec!["doomed".to_string()]);

        // FK cascade must have nulled stack_id on both members.
        let primary_stack: (Option<String>,) =
            sqlx::query_as("SELECT stack_id FROM media WHERE id = ?")
                .bind(primary.as_str())
                .fetch_one(db.pool())
                .await
                .unwrap();
        let member_stack: (Option<String>,) =
            sqlx::query_as("SELECT stack_id FROM media WHERE id = ?")
                .bind(member.as_str())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(primary_stack.0.is_none());
        assert!(member_stack.0.is_none());
    }

    /// The grid query collapses stacks to their primary — non-primary
    /// members must not appear in the listing. Un-stacked rows are
    /// unaffected.
    #[tokio::test]
    async fn list_hides_non_primary_stack_members() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        let unstacked = MediaId::new("c".repeat(64));
        repo.insert(&record_with_taken_at(primary.clone(), "p.jpg", Some(1000)))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(sibling.clone(), "s.jpg", Some(2000)))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(
            unstacked.clone(),
            "u.jpg",
            Some(3000),
        ))
        .await
        .unwrap();

        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&primary, "stk").await.unwrap();
        repo.set_media_stack_id(&sibling, "stk").await.unwrap();

        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(items.len(), 2, "sibling must be hidden");
        assert!(ids.contains(&unstacked.as_str()), "un-stacked visible");
        assert!(ids.contains(&primary.as_str()), "primary visible");
        assert!(
            !ids.contains(&sibling.as_str()),
            "non-primary stack member hidden"
        );

        // The `MediaItem.stack_id` field is populated for the primary
        // (so the cell can render the stack badge) and absent on
        // un-stacked rows.
        let primary_item = items.iter().find(|i| i.id == primary).unwrap();
        let unstacked_item = items.iter().find(|i| i.id == unstacked).unwrap();
        assert_eq!(primary_item.stack_id.as_deref(), Some("stk"));
        assert!(unstacked_item.stack_id.is_none());
    }

    /// Phase C §8.2: when a stack contains a Moments-render member
    /// (the rendered sibling we upload for a pixel-adjustment edit),
    /// the grid filter surfaces the *other* sibling — the original.
    /// The rendered child is still the stack primary on Immich, but
    /// users edit the original.
    #[tokio::test]
    async fn list_surfaces_original_for_moments_edit_stack() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let original = MediaId::new("a".repeat(64));
        let rendered = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(original.clone(), "o.jpg", Some(1000)))
            .await
            .unwrap();
        let mut rendered_rec = record_with_taken_at(rendered.clone(), "r.jpg", Some(1000));
        rendered_rec.is_moments_render = true;
        repo.insert(&rendered_rec).await.unwrap();

        // The rendered child is the stack primary on Immich.
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: rendered.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&rendered, "stk").await.unwrap();
        repo.set_media_stack_id(&original, "stk").await.unwrap();

        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        // Expect the original visible, the rendered child hidden.
        assert!(
            ids.contains(&original.as_str()),
            "original must be visible: {ids:?}"
        );
        assert!(
            !ids.contains(&rendered.as_str()),
            "rendered sibling must be hidden: {ids:?}"
        );
    }

    /// Phase C review fix #3: an unstacked row with
    /// `is_moments_render = 1` must not appear in the grid — this is
    /// the transient state between `save_pixel_edit` inserting the
    /// rendered media row and the push manager's `StackCreated`
    /// draining. Without the extra clause, the rendered child would
    /// briefly appear as a duplicate of the original.
    #[tokio::test]
    async fn list_hides_unstacked_moments_render_rows() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let original = MediaId::new("a".repeat(64));
        let rendered = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(original.clone(), "o.jpg", Some(1000)))
            .await
            .unwrap();
        let mut rendered_rec = record_with_taken_at(rendered.clone(), "r.jpg", Some(1000));
        rendered_rec.is_moments_render = true;
        repo.insert(&rendered_rec).await.unwrap();

        // Note: no stack created yet — simulating the window between
        // save_pixel_edit's media insert and the push manager's
        // StackCreated draining.

        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert!(
            ids.contains(&original.as_str()),
            "original visible: {ids:?}"
        );
        assert!(
            !ids.contains(&rendered.as_str()),
            "unstacked rendered child must be hidden: {ids:?}"
        );
    }

    /// Sanity: the §8.2 override only kicks in for Moments-edit
    /// stacks. Ordinary stacks (e.g. a panorama burst with no
    /// `is_moments_render` member) still collapse to the server's
    /// primary.
    #[tokio::test]
    async fn list_keeps_primary_when_no_moments_render_member() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary.clone(), "p.jpg", Some(1000)))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(sibling.clone(), "s.jpg", Some(1000)))
            .await
            .unwrap();
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&primary, "stk").await.unwrap();
        repo.set_media_stack_id(&sibling, "stk").await.unwrap();

        let items = repo.list(MediaFilter::All, None, 50).await.unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&primary.as_str()), "primary visible: {ids:?}");
        assert!(
            !ids.contains(&sibling.as_str()),
            "non-primary sibling hidden"
        );
    }

    /// Issue #224: `AssetV1` may arrive before its matching `StackV1`,
    /// and `media.stack_id` REFERENCES `stacks(id)` is enforced. The
    /// stub-then-bind path must succeed; the later `StackV1` upsert
    /// overwrites the placeholder primary.
    #[tokio::test]
    async fn ensure_stack_stub_lets_asset_handler_bind_before_stack_arrives() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let asset = MediaId::new("a".repeat(64));
        repo.insert(&record_with_taken_at(asset.clone(), "a.jpg", None))
            .await
            .unwrap();

        // Stub-then-bind, mirroring `AssetHandler::apply_stack_membership`.
        // Pass a non-zero `now` so the stub survives a same-cycle reset
        // sweep (issue #224 / #628).
        repo.ensure_stack_stub("late-stack", &asset, 555)
            .await
            .unwrap();
        repo.set_media_stack_id(&asset, "late-stack").await.unwrap();

        // Stub exists with the placeholder primary and a live heartbeat.
        let row: (String, i64) =
            sqlx::query_as("SELECT primary_asset_id, last_seen_at FROM stacks WHERE id = ?")
                .bind("late-stack")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, asset.as_str(), "stub primary is the binding asset");
        assert_eq!(
            row.1, 555,
            "stub heartbeat seeded from `now`, not 0 — survives same-cycle sweep"
        );

        // Real StackV1 arrives later: upsert overwrites the primary.
        let real_primary = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(real_primary.clone(), "b.jpg", None))
            .await
            .unwrap();
        repo.upsert_stack(&Stack {
            id: "late-stack".to_string(),
            primary_asset_id: real_primary.clone(),
        })
        .await
        .unwrap();

        let primary_after: String =
            sqlx::query_scalar("SELECT primary_asset_id FROM stacks WHERE id = ?")
                .bind("late-stack")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(primary_after, real_primary.as_str());
    }

    /// Issue #224: when a stack primary is deleted, `siblings_freed_by_deletion`
    /// must return the rows whose `stack_id` will be SET NULL by the
    /// cascade — but never return ids that are themselves being deleted.
    #[tokio::test]
    async fn siblings_freed_by_deletion_returns_cascade_affected_only() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let sibling_a = MediaId::new("b".repeat(64));
        let sibling_b = MediaId::new("c".repeat(64));
        let unrelated = MediaId::new("d".repeat(64));
        for (id, name) in [
            (&primary, "p.jpg"),
            (&sibling_a, "a.jpg"),
            (&sibling_b, "b.jpg"),
            (&unrelated, "u.jpg"),
        ] {
            repo.insert(&record_with_taken_at(id.clone(), name, None))
                .await
                .unwrap();
        }
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        for id in [&primary, &sibling_a, &sibling_b] {
            repo.set_media_stack_id(id, "stk").await.unwrap();
        }

        // Deleting just the primary frees both siblings.
        let mut freed = repo
            .siblings_freed_by_deletion(std::slice::from_ref(&primary))
            .await
            .unwrap();
        freed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(freed, vec![sibling_a.clone(), sibling_b.clone()]);

        // Deleting the primary AND sibling_a frees only sibling_b —
        // the other deleted id must be excluded from the freed list.
        let to_delete = vec![primary.clone(), sibling_a.clone()];
        let freed = repo.siblings_freed_by_deletion(&to_delete).await.unwrap();
        assert_eq!(freed, vec![sibling_b.clone()]);

        // Deleting an un-stacked row frees nobody.
        let freed = repo
            .siblings_freed_by_deletion(std::slice::from_ref(&unrelated))
            .await
            .unwrap();
        assert!(freed.is_empty());

        // Deleting a non-primary member frees nobody (its deletion
        // doesn't trigger the stacks-cascade chain).
        let freed = repo
            .siblings_freed_by_deletion(std::slice::from_ref(&sibling_a))
            .await
            .unwrap();
        assert!(freed.is_empty());
    }

    /// Migration 023 declares two cascades (`stacks.primary_asset_id
    /// ON DELETE CASCADE` and `media.stack_id ON DELETE SET NULL`).
    /// They're load-bearing for the AssetDelete-of-primary path:
    /// when a stack primary is removed, the `stacks` row vanishes
    /// and surviving siblings rejoin the un-stacked timeline.
    /// This test exercises the cascade chain directly — without
    /// going through `delete_stack`'s explicit-clear belt-and-braces
    /// — to confirm sqlx 0.8's default `PRAGMA foreign_keys = ON` is
    /// in effect.
    #[tokio::test]
    async fn fk_cascade_clears_stack_and_member_stack_id_on_primary_delete() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        let primary = MediaId::new("a".repeat(64));
        let sibling = MediaId::new("b".repeat(64));
        repo.insert(&record_with_taken_at(primary.clone(), "p.jpg", None))
            .await
            .unwrap();
        repo.insert(&record_with_taken_at(sibling.clone(), "s.jpg", None))
            .await
            .unwrap();
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: primary.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&primary, "stk").await.unwrap();
        repo.set_media_stack_id(&sibling, "stk").await.unwrap();

        // Delete the primary media row directly — bypassing
        // `delete_stack`. We rely purely on the FK cascade chain.
        sqlx::query("DELETE FROM media WHERE id = ?")
            .bind(primary.as_str())
            .execute(db.pool())
            .await
            .unwrap();

        // stacks.primary_asset_id ON DELETE CASCADE should have removed
        // the stack row.
        let stack_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM stacks WHERE id = ?")
            .bind("stk")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            stack_count.0, 0,
            "stack row must cascade-delete when its primary media is removed"
        );

        // media.stack_id ON DELETE SET NULL should have cleared the
        // sibling's stack pointer.
        let sibling_stack: (Option<String>,) =
            sqlx::query_as("SELECT stack_id FROM media WHERE id = ?")
                .bind(sibling.as_str())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(
            sibling_stack.0.is_none(),
            "surviving sibling's stack_id must cascade to NULL"
        );
    }

    /// `clear_media_stack_id` is the path AssetHandler takes when
    /// `AssetV1.stack` arrives as `null` — the asset was un-stacked
    /// server-side.
    #[tokio::test]
    async fn clear_media_stack_id_unbinds() {
        let dir = tempdir().unwrap();
        let (repo, _db) = test_repo(dir.path()).await;

        let id = MediaId::new("a".repeat(64));
        repo.insert(&test_record(id.clone())).await.unwrap();
        repo.upsert_stack(&Stack {
            id: "stk".to_string(),
            primary_asset_id: id.clone(),
        })
        .await
        .unwrap();
        repo.set_media_stack_id(&id, "stk").await.unwrap();
        repo.clear_media_stack_id(&id).await.unwrap();

        let items = repo.list(MediaFilter::All, None, 10).await.unwrap();
        assert!(items[0].stack_id.is_none());
    }
}
