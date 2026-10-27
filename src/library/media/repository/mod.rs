// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use super::model::{
    LibraryStats, MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord, MediaType,
    MediaVisibility,
};
use crate::library::db::{id_placeholders, Database};
use crate::library::error::LibraryError;

mod sync;

pub use sync::UpsertOutcome;

/// SQL predicate restricting `{alias}.visibility` to `admitted`.
///
/// Issue #695: every query that lists media builds its visibility clause
/// here, from [`MediaFilter::admitted_visibilities`] or
/// [`MediaVisibility::LISTABLE`], so list, count and in-memory matching
/// share one definition. The values are compile-time constants, so they
/// are inlined rather than bound.
pub(crate) fn visibility_predicate(alias: &str, admitted: &[MediaVisibility]) -> String {
    let values: Vec<String> = admitted
        .iter()
        .map(|v| format!("'{}'", v.as_str()))
        .collect();
    format!("{alias}.visibility IN ({})", values.join(", "))
}

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
    visibility: String,
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
            visibility: MediaVisibility::parse_or_hidden(&self.visibility),
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
    ///
    /// Not a listing, so no visibility filter: callers that already hold
    /// an id (editor, viewer, sync) get the row whatever its visibility.
    pub async fn get(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, taken_at, imported_at, original_filename,
                    width, height, orientation, media_type, is_favorite,
                    is_trashed, trashed_at, duration_ms, stack_id, is_moments_render,
                    visibility
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
    ///
    /// Issue #695: hidden and locked rows are absent too, for the same
    /// reason — a pull that locks an asset emits `Updated`, and the
    /// missing row drops it from every tracked grid. Archived rows are
    /// returned; [`MediaFilter::matches`] keeps them out of the timeline.
    pub async fn get_many(&self, ids: &[MediaId]) -> Result<Vec<MediaItem>, LibraryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = id_placeholders(ids.len());
        let visibility = visibility_predicate("m", MediaVisibility::LISTABLE);
        let sql = format!(
            "SELECT m.id, m.taken_at, m.imported_at, m.original_filename,
                    m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                    m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id, m.is_moments_render,
                    m.visibility
             FROM media m
             LEFT JOIN stacks s ON m.stack_id = s.id
             LEFT JOIN media render ON render.stack_id = s.id AND render.is_moments_render = 1
             WHERE m.id IN ({placeholders})
               AND {visibility}
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
            // Issue #680: faces hidden or soft-deleted server-side don't
            // put their asset in the person's grid. Keep this in step with
            // `FacesRepository::update_face_count`, which counts the same faces.
            MediaFilter::Person { .. } => (
                " AND m.is_trashed = 0 AND m.id IN (SELECT DISTINCT asset_id FROM asset_faces
                   WHERE person_id = ? AND is_visible = 1 AND deleted_at IS NULL)",
                "COALESCE(m.taken_at, 0)",
            ),
        };

        // Issue #695: archived, hidden and locked assets stay out of the
        // views that don't admit them. Same source as `MediaFilter::matches`.
        let filter_clause = format!(
            "{filter_clause} AND {}",
            visibility_predicate("m", filter.admitted_visibilities())
        );

        let extra_bind: Option<String> = match &filter {
            MediaFilter::RecentImports { since } => Some(since.to_string()),
            MediaFilter::Album { album_id } => Some(album_id.as_str().to_owned()),
            MediaFilter::Person { person_id } => Some(person_id.as_str().to_owned()),
            _ => None,
        };

        let columns = "m.id, m.taken_at, m.imported_at, m.original_filename,
                        m.width, m.height, m.orientation, m.media_type, m.is_favorite,
                        m.is_trashed, m.trashed_at, m.duration_ms, m.stack_id, m.is_moments_render,
                        m.visibility";

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
    ///
    /// Issue #695: hidden and locked rows are excluded throughout, so the
    /// photo/video counts and the sidebar trash badge (`trashed_count`)
    /// agree with what the grids can actually show. Archived rows are
    /// counted: they are still part of the library.
    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        let sql = format!(
            "SELECT
                COUNT(CASE WHEN media_type = 0 AND is_trashed = 0 THEN 1 END),
                COUNT(CASE WHEN media_type = 1 AND is_trashed = 0 THEN 1 END),
                COALESCE(SUM(CASE WHEN is_trashed = 0 THEN file_size ELSE 0 END), 0),
                COUNT(CASE WHEN is_trashed = 1 THEN 1 END)
             FROM media m
             WHERE {}",
            visibility_predicate("m", MediaVisibility::LISTABLE)
        );
        let row: (i64, i64, i64, i64) = sqlx::query_as(&sql)
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
    ///
    /// Leaves `taken_at_offset_minutes` NULL; the local importer uses
    /// [`Self::insert_with_taken_at_offset`] to record it.
    pub async fn insert(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.insert_with_taken_at_offset(record, None).await
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
}

// ── Capture-time offset (#677) ───────────────────────────────────────

/// A local row the #677 backfill still has to examine.
#[derive(Debug, Clone)]
pub struct TakenAtOffsetCandidate {
    pub id: MediaId,
    /// `relative_path` column: relative to `originals/` in Managed mode,
    /// absolute in Referenced mode.
    pub relative_path: String,
    /// The `taken_at` value as currently stored.
    pub taken_at: i64,
}

/// One row update produced by the #677 backfill.
#[derive(Debug, Clone)]
pub struct TakenAtOffsetRepair {
    pub id: MediaId,
    /// The `taken_at` the decision was based on. The update only applies
    /// while the row still holds this value, so a stale decision is a no-op.
    pub expected_taken_at: i64,
    /// The `taken_at` to store (equal to `expected_taken_at` when only
    /// the offset is being recorded).
    pub taken_at: i64,
    /// Offset in minutes east of UTC, from the file's `OffsetTime*` tag.
    pub offset_minutes: i64,
}

impl MediaRepository {
    /// Persist a newly imported media asset record together with the
    /// capture-time UTC offset read from its EXIF `OffsetTime*` tag.
    ///
    /// Written in the same statement as the row, so a local import is
    /// never visible to the #677 backfill with a NULL offset.
    ///
    /// The local importer (`importer/persistence.rs`, via
    /// `MediaService::insert_media_with_taken_at_offset`) is the only
    /// caller that passes an offset; every other path (sync, edit renders,
    /// tests) goes through [`Self::insert`], which leaves it NULL.
    pub async fn insert_with_taken_at_offset(
        &self,
        record: &MediaRecord,
        taken_at_offset_minutes: Option<i64>,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO media (id, content_hash, external_id, relative_path,
                                original_filename, file_size, imported_at, media_type,
                                taken_at, width, height, orientation, duration_ms,
                                is_favorite, is_trashed, trashed_at, is_moments_render,
                                taken_at_offset_minutes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(taken_at_offset_minutes)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return `(taken_at, taken_at_offset_minutes)` for `id`, or `None`
    /// if no such row exists.
    pub async fn taken_at_with_offset(
        &self,
        id: &MediaId,
    ) -> Result<Option<(Option<i64>, Option<i64>)>, LibraryError> {
        sqlx::query_as("SELECT taken_at, taken_at_offset_minutes FROM media WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.db.pool())
            .await
            .map_err(LibraryError::Db)
    }

    /// Local-origin image rows that have a `taken_at` but no recorded
    /// offset: the rows the #677 backfill has to examine.
    ///
    /// `external_id IS NULL` excludes every row that came from (or was
    /// matched to) an Immich asset. Those carry the server's
    /// `localDateTime` and were never shifted.
    ///
    /// `is_moments_render = 0` excludes edit renders: a render copies
    /// `taken_at` from its source row and its re-encoded JPEG carries no
    /// capture EXIF, so it follows the source rather than being repaired
    /// on its own. (Renders are only created for Immich-backed originals,
    /// but one whose upload hasn't drained yet still has no external id.)
    pub async fn taken_at_offset_candidates(
        &self,
    ) -> Result<Vec<TakenAtOffsetCandidate>, LibraryError> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT id, relative_path, taken_at FROM media
             WHERE external_id IS NULL
               AND taken_at IS NOT NULL
               AND taken_at_offset_minutes IS NULL
               AND is_moments_render = 0
               AND media_type = ?
             ORDER BY id",
        )
        .bind(MediaType::Image as i64)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(rows
            .into_iter()
            .map(|(id, relative_path, taken_at)| TakenAtOffsetCandidate {
                id: MediaId::new(id),
                relative_path,
                taken_at,
            })
            .collect())
    }

    /// Apply a batch of #677 repairs in one transaction.
    ///
    /// Each update is a compare-and-set: it only touches a row whose
    /// offset is still NULL and whose `taken_at` still holds the value the
    /// repair was computed from. Re-applying a batch, or a batch computed
    /// before a crash, therefore never shifts a row twice. Returns the ids
    /// that were actually updated.
    pub async fn apply_taken_at_offset_repairs(
        &self,
        repairs: &[TakenAtOffsetRepair],
    ) -> Result<Vec<MediaId>, LibraryError> {
        if repairs.is_empty() {
            return Ok(Vec::new());
        }
        let mut updated = Vec::new();
        let mut tx = self.db.pool().begin().await.map_err(LibraryError::Db)?;
        for repair in repairs {
            let result = sqlx::query(
                "UPDATE media SET taken_at = ?, taken_at_offset_minutes = ?
                 WHERE id = ? AND taken_at = ? AND taken_at_offset_minutes IS NULL",
            )
            .bind(repair.taken_at)
            .bind(repair.offset_minutes)
            .bind(repair.id.as_str())
            .bind(repair.expected_taken_at)
            .execute(&mut *tx)
            .await
            .map_err(LibraryError::Db)?;
            if result.rows_affected() > 0 {
                updated.push(repair.id.clone());
            }
        }
        tx.commit().await.map_err(LibraryError::Db)?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::*;
    use crate::library::media::model::Stack;
    use tempfile::tempdir;

    pub(super) async fn test_repo(dir: &std::path::Path) -> (MediaRepository, Database) {
        let db = open_test_db(dir).await;
        let repo = MediaRepository::new(db.clone());
        (repo, db)
    }

    /// Issue #680: a face hidden or soft-deleted server-side must not
    /// put its asset in the person grid.
    #[tokio::test]
    async fn person_filter_excludes_hidden_and_soft_deleted_faces() {
        use crate::library::faces::PersonId;

        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;

        sqlx::query("INSERT INTO people (id, name) VALUES ('p1', 'Alice')")
            .execute(db.pool())
            .await
            .unwrap();
        for (n, taken) in [(1, 1000), (2, 2000), (3, 3000)] {
            let rec = record_with_taken_at(
                MediaId::new(format!("m{n}")),
                &format!("a/photo{n}.jpg"),
                Some(taken),
            );
            repo.insert(&rec).await.unwrap();
        }

        // f1 live and visible, f2 hidden, f3 soft-deleted.
        for (id, asset, visible, deleted) in [
            ("f1", "m1", 1, None),
            ("f2", "m2", 0, None),
            ("f3", "m3", 1, Some(12345_i64)),
        ] {
            sqlx::query(
                "INSERT INTO asset_faces (id, asset_id, person_id, is_visible, deleted_at)
                 VALUES (?, ?, 'p1', ?, ?)",
            )
            .bind(id)
            .bind(asset)
            .bind(visible)
            .bind(deleted)
            .execute(db.pool())
            .await
            .unwrap();
        }

        let items = repo
            .list(
                MediaFilter::Person {
                    person_id: PersonId::new("p1".to_string()),
                },
                None,
                50,
            )
            .await
            .unwrap();

        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["m1"]);
    }

    /// Issue #695: seed one asset per visibility. Each is a favourite, in
    /// album `a1`, has a visible face of person `p1`, and was imported
    /// after `since = 0`, so it qualifies for every filter except on
    /// visibility. Returns the ids in `[timeline, archive, hidden, locked]`
    /// order.
    async fn seed_one_per_visibility(repo: &MediaRepository, db: &Database) -> Vec<MediaId> {
        sqlx::query(
            "INSERT INTO albums (id, name, created_at, updated_at) VALUES ('a1', 'A', 0, 0)",
        )
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO people (id, name) VALUES ('p1', 'Alice')")
            .execute(db.pool())
            .await
            .unwrap();

        let mut ids = Vec::new();
        for (n, visibility) in [
            MediaVisibility::Timeline,
            MediaVisibility::Archive,
            MediaVisibility::Hidden,
            MediaVisibility::Locked,
        ]
        .into_iter()
        .enumerate()
        {
            let id = MediaId::new(visibility.as_str().to_string());
            let mut rec =
                record_with_taken_at(id.clone(), &format!("v/{n}.jpg"), Some(1000 + n as i64));
            rec.is_favorite = true;
            repo.upsert_with_visibility(&rec, visibility).await.unwrap();
            sqlx::query(
                "INSERT INTO album_media (album_id, media_id, added_at) VALUES ('a1', ?, 0)",
            )
            .bind(id.as_str())
            .execute(db.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO asset_faces (id, asset_id, person_id, is_visible)
                 VALUES (?, ?, 'p1', 1)",
            )
            .bind(format!("f-{}", id.as_str()))
            .bind(id.as_str())
            .execute(db.pool())
            .await
            .unwrap();
            ids.push(id);
        }
        ids
    }

    async fn listed(repo: &MediaRepository, filter: MediaFilter) -> Vec<String> {
        let mut ids: Vec<String> = repo
            .list(filter, None, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.id.as_str().to_owned())
            .collect();
        ids.sort();
        ids
    }

    /// Issue #695: the main views list only timeline assets; albums and
    /// people also list archived ones; hidden and locked appear nowhere.
    #[tokio::test]
    async fn list_filters_by_visibility() {
        use crate::library::album::AlbumId;
        use crate::library::faces::PersonId;

        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;
        seed_one_per_visibility(&repo, &db).await;

        for filter in [
            MediaFilter::All,
            MediaFilter::Favorites,
            MediaFilter::RecentImports { since: 0 },
        ] {
            assert_eq!(
                listed(&repo, filter.clone()).await,
                ["timeline"],
                "{filter:?}"
            );
        }
        for filter in [
            MediaFilter::Album {
                album_id: AlbumId::new("a1".to_string()),
            },
            MediaFilter::Person {
                person_id: PersonId::new("p1".to_string()),
            },
        ] {
            assert_eq!(
                listed(&repo, filter.clone()).await,
                ["archive", "timeline"],
                "{filter:?}"
            );
        }
    }

    /// Issue #695: the trash, the trash badge count and the photo count
    /// agree with the grids: hidden and locked rows are never counted.
    #[tokio::test]
    async fn trash_and_stats_exclude_hidden_and_locked() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;
        let ids = seed_one_per_visibility(&repo, &db).await;

        let stats = repo.library_stats().await.unwrap();
        assert_eq!(stats.photo_count, 2, "timeline + archive");

        repo.trash(&ids).await.unwrap();
        assert_eq!(
            listed(&repo, MediaFilter::Trashed).await,
            ["archive", "timeline"]
        );
        let stats = repo.library_stats().await.unwrap();
        assert_eq!(stats.trashed_count, 2);
        assert_eq!(stats.photo_count, 0);
    }

    /// Issue #695: `get_many` drives the client's patch-on-`Updated`
    /// path, so it must omit hidden and locked rows (dropping them from
    /// every open grid) while returning archived ones for
    /// `MediaFilter::matches` to judge.
    #[tokio::test]
    async fn get_many_omits_hidden_and_locked() {
        let dir = tempdir().unwrap();
        let (repo, db) = test_repo(dir.path()).await;
        let ids = seed_one_per_visibility(&repo, &db).await;

        let items = repo.get_many(&ids).await.unwrap();
        let mut got: Vec<(&str, MediaVisibility)> = items
            .iter()
            .map(|i| (i.id.as_str(), i.visibility))
            .collect();
        got.sort_by_key(|(id, _)| *id);
        assert_eq!(
            got,
            [
                ("archive", MediaVisibility::Archive),
                ("timeline", MediaVisibility::Timeline)
            ]
        );
    }

    #[test]
    fn visibility_predicate_renders_in_clause() {
        assert_eq!(
            visibility_predicate("m", MediaVisibility::LISTABLE),
            "m.visibility IN ('timeline', 'archive')"
        );
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
}
