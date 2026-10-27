// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;
use std::path::PathBuf;

use tracing::{debug, info, instrument, warn};

use super::model::{MaintenanceTask, TakenAtBackfillSummary};
use super::repository::MaintenanceRepository;
use crate::library::config::LocalStorageMode;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::repository::{
    MediaRepository, TakenAtOffsetCandidate, TakenAtOffsetRepair,
};
use crate::library::metadata::exif::extract_exif;

/// Rows read and written per batch: one `spawn_blocking` EXIF pass and
/// one transaction each.
const BATCH_SIZE: usize = 256;

/// Log an `info!` progress line each time this many more candidates have
/// been examined, so a long first open on a large library is visible.
const PROGRESS_EVERY: usize = 1000;

/// One-shot library repairs, run on Tokio when a library is opened.
///
/// Every task is recorded in `library_maintenance` once it completes, so
/// it never runs again for that library.
#[derive(Clone)]
pub struct MaintenanceService {
    repo: MaintenanceRepository,
    media: MediaRepository,
    originals_dir: PathBuf,
    mode: LocalStorageMode,
}

impl MaintenanceService {
    pub fn new(db: Database, originals_dir: PathBuf, mode: LocalStorageMode) -> Self {
        Self {
            repo: MaintenanceRepository::new(db.clone()),
            media: MediaRepository::new(db),
            originals_dir,
            mode,
        }
    }

    /// Run every pending task that applies to a **local** library.
    ///
    /// Call before any client loads the media models, so the grids read
    /// the repaired rows from the start. A failure is logged and the
    /// library opens anyway: the task was not marked complete, so it is
    /// retried on the next open.
    #[instrument(skip_all)]
    pub async fn run_pending_local(&self) {
        if let Err(e) = self.backfill_taken_at_offsets().await {
            warn!(error = %e, "capture-time backfill failed; will retry on next open");
        }
    }

    /// Issue #677: repair `taken_at` for local rows imported before #675.
    ///
    /// Before #675 the EXIF extractor subtracted `OffsetTimeOriginal` /
    /// `OffsetTime` from the capture wall clock, so those rows were filed
    /// `offset` minutes early. For every local image row with a `taken_at`
    /// and no recorded offset, re-read the original's EXIF and compare:
    ///
    /// - stored == wall clock − offset → shifted: store the wall clock;
    /// - stored == wall clock → already correct (imported by a build that
    ///   had #675, or a zero offset): leave it;
    /// - anything else → leave it, we can't explain the value.
    ///
    /// The offset column is filled in for every row whose file carries the
    /// tag, which takes the row out of the candidate set. Rows with no tag
    /// or no reachable file are left untouched.
    ///
    /// Returns `None` if the task already completed for this library.
    #[instrument(skip_all)]
    pub async fn backfill_taken_at_offsets(
        &self,
    ) -> Result<Option<TakenAtBackfillSummary>, LibraryError> {
        let task = MaintenanceTask::TakenAtOffsetBackfill;
        if self.repo.is_completed(task).await? {
            debug!("capture-time backfill already completed");
            return Ok(None);
        }

        let candidates = self.media.taken_at_offset_candidates().await?;
        let total = candidates.len();
        let mut summary = TakenAtBackfillSummary {
            scanned: total,
            ..Default::default()
        };
        // The window shows only its "Loading Library…" spinner while this
        // runs, so the log is the one place the cost is visible.
        info!(candidates = total, "running capture-time backfill (#677)");
        let started = std::time::Instant::now();

        let mut processed = 0;
        for batch in candidates.chunks(BATCH_SIZE) {
            self.backfill_batch(batch, &mut summary).await?;
            let before = processed;
            processed += batch.len();
            if processed / PROGRESS_EVERY > before / PROGRESS_EVERY {
                info!(
                    processed,
                    total,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "capture-time backfill progress"
                );
            }
        }

        self.repo
            .mark_completed(task, chrono::Utc::now().timestamp())
            .await?;
        info!(
            scanned = summary.scanned,
            corrected = summary.corrected,
            already_correct = summary.already_correct,
            unexplained = summary.unexplained,
            no_offset_tag = summary.no_offset_tag,
            unreachable = summary.unreachable,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "capture-time backfill complete"
        );
        if summary.unreachable > 0 {
            // The marker is written regardless (a deliberate choice, #677),
            // so these rows keep their pre-#675 `taken_at`.
            warn!(
                unreachable = summary.unreachable,
                "capture-time backfill could not read some originals \
                 (e.g. an unmounted drive); those rows will not be retried"
            );
        }
        Ok(Some(summary))
    }

    /// Read EXIF for one batch on a blocking thread, then apply the
    /// resulting repairs in one transaction.
    async fn backfill_batch(
        &self,
        batch: &[TakenAtOffsetCandidate],
        summary: &mut TakenAtBackfillSummary,
    ) -> Result<(), LibraryError> {
        let paths: Vec<PathBuf> = batch
            .iter()
            .map(|c| self.original_path(&c.relative_path))
            .collect();
        let probes =
            tokio::task::spawn_blocking(move || paths.iter().map(|p| probe(p)).collect::<Vec<_>>())
                .await
                .map_err(|e| LibraryError::Runtime(e.to_string()))?;

        let mut repairs = Vec::new();
        let mut shifted = Vec::new();
        for (candidate, probe) in batch.iter().zip(probes) {
            let outcome = match probe {
                Probe::Unreachable => {
                    debug!(id = %candidate.id, "original unreachable; leaving taken_at");
                    summary.unreachable += 1;
                    continue;
                }
                Probe::Exif {
                    wall_clock,
                    offset_minutes,
                } => classify(candidate.taken_at, wall_clock, offset_minutes),
            };
            match outcome {
                Outcome::NoOffsetTag => summary.no_offset_tag += 1,
                Outcome::Shifted { taken_at, offset } => {
                    shifted.push(candidate.id.clone());
                    repairs.push(repair(candidate, taken_at, offset));
                }
                Outcome::AlreadyCorrect { offset } => {
                    summary.already_correct += 1;
                    repairs.push(repair(candidate, candidate.taken_at, offset));
                }
                Outcome::Unexplained { offset } => {
                    debug!(id = %candidate.id, "taken_at matches neither reading; leaving it");
                    summary.unexplained += 1;
                    repairs.push(repair(candidate, candidate.taken_at, offset));
                }
            }
        }

        let updated: HashSet<_> = self
            .media
            .apply_taken_at_offset_repairs(&repairs)
            .await?
            .into_iter()
            .collect();
        summary.corrected += shifted.iter().filter(|id| updated.contains(*id)).count();
        Ok(())
    }

    /// Absolute path of an original from its `relative_path` column.
    fn original_path(&self, relative_path: &str) -> PathBuf {
        match self.mode {
            LocalStorageMode::Managed => self.originals_dir.join(relative_path),
            LocalStorageMode::Referenced => PathBuf::from(relative_path),
        }
    }
}

/// What the original file says about a row.
enum Probe {
    /// The file is missing (e.g. a Referenced original that was moved).
    Unreachable,
    /// EXIF as read now: the wall clock and the `OffsetTime*` tag.
    Exif {
        wall_clock: Option<i64>,
        offset_minutes: Option<i64>,
    },
}

/// Blocking: read the capture fields from the original at `path`.
///
/// `extract_exif` reads only the metadata for JPEG (it stops at the
/// start of scan) and HEIF (it seeks to the `Exif` item); TIFF-based
/// files, including CR2/NEF/ARW/DNG raws, are read whole by
/// `kamadak-exif`, which dominates the pass on raw-heavy libraries.
fn probe(path: &std::path::Path) -> Probe {
    if !path.try_exists().unwrap_or(false) {
        return Probe::Unreachable;
    }
    let info = extract_exif(path);
    Probe::Exif {
        wall_clock: info.captured_at,
        offset_minutes: info.captured_at_tz,
    }
}

/// The backfill's decision for one row.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The file has no offset tag, so the row was never shifted.
    NoOffsetTag,
    /// Stored value is the pre-#675 `wall clock − offset`.
    Shifted { taken_at: i64, offset: i64 },
    /// Stored value already is the wall clock.
    AlreadyCorrect { offset: i64 },
    /// Stored value matches neither reading.
    Unexplained { offset: i64 },
}

/// Decide what to do with a row holding `stored`, given the file's
/// current EXIF reading.
fn classify(stored: i64, wall_clock: Option<i64>, offset_minutes: Option<i64>) -> Outcome {
    let Some(offset) = offset_minutes else {
        return Outcome::NoOffsetTag;
    };
    match wall_clock {
        Some(wall) if wall == stored => Outcome::AlreadyCorrect { offset },
        Some(wall) if wall == stored + offset * 60 => Outcome::Shifted {
            taken_at: wall,
            offset,
        },
        _ => Outcome::Unexplained { offset },
    }
}

fn repair(candidate: &TakenAtOffsetCandidate, taken_at: i64, offset: i64) -> TakenAtOffsetRepair {
    TakenAtOffsetRepair {
        id: candidate.id.clone(),
        expected_taken_at: candidate.taken_at,
        taken_at,
        offset_minutes: offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, record_with_taken_at};
    use crate::library::media::MediaId;

    /// 2025-10-18 11:56:18 read as UTC: the camera's wall clock in the
    /// real-world case from #675.
    const WALL: i64 = 1_760_788_578;
    const WALL_EXIF: &str = "2025:10:18 11:56:18";
    /// `+11:00` (Sydney DST) in minutes.
    const OFFSET: i64 = 660;
    /// What a pre-#675 import stored: 00:56:18.
    const SHIFTED: i64 = WALL - OFFSET * 60;

    /// A minimal JPEG (SOI, APP1 Exif, EOI) carrying `DateTimeOriginal`
    /// and, optionally, `OffsetTimeOriginal`.
    fn jpeg_with_exif(datetime: &str, offset: Option<&str>) -> Vec<u8> {
        use exif::experimental::Writer;
        use exif::{Field, In, Tag, Value};

        let ascii = |tag, s: &str| Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![s.as_bytes().to_vec()]),
        };
        let dt = ascii(Tag::DateTimeOriginal, datetime);
        let off = offset.map(|o| ascii(Tag::OffsetTimeOriginal, o));

        let mut writer = Writer::new();
        writer.push_field(&dt);
        if let Some(ref f) = off {
            writer.push_field(f);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();

        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let len = u16::try_from(2 + 6 + tiff.len()).unwrap();
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&tiff);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        originals: PathBuf,
        db: Database,
        media: MediaRepository,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let originals = dir.path().join("originals");
            std::fs::create_dir_all(&originals).unwrap();
            let db = open_test_db(dir.path()).await;
            let media = MediaRepository::new(db.clone());
            Self {
                _dir: dir,
                originals,
                db,
                media,
            }
        }

        fn service(&self, mode: LocalStorageMode) -> MaintenanceService {
            MaintenanceService::new(self.db.clone(), self.originals.clone(), mode)
        }

        /// Write `bytes` as a managed original and insert a local row for
        /// it holding `taken_at`.
        async fn add(&self, name: &str, bytes: &[u8], taken_at: i64) -> MediaId {
            std::fs::write(self.originals.join(name), bytes).unwrap();
            let id = MediaId::generate();
            self.media
                .insert(&record_with_taken_at(id.clone(), name, Some(taken_at)))
                .await
                .unwrap();
            id
        }

        async fn taken_at(&self, id: &MediaId) -> (Option<i64>, Option<i64>) {
            self.media.taken_at_with_offset(id).await.unwrap().unwrap()
        }

        async fn clear_marker(&self) {
            sqlx::query("DELETE FROM library_maintenance")
                .execute(self.db.pool())
                .await
                .unwrap();
        }
    }

    #[test]
    fn fixture_round_trips_through_extractor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, jpeg_with_exif(WALL_EXIF, Some("+11:00"))).unwrap();
        let info = extract_exif(&path);
        assert_eq!(info.captured_at, Some(WALL));
        assert_eq!(info.captured_at_tz, Some(OFFSET));
    }

    #[test]
    fn classify_cases() {
        assert_eq!(classify(SHIFTED, Some(WALL), None), Outcome::NoOffsetTag);
        assert_eq!(
            classify(SHIFTED, Some(WALL), Some(OFFSET)),
            Outcome::Shifted {
                taken_at: WALL,
                offset: OFFSET
            }
        );
        assert_eq!(
            classify(WALL, Some(WALL), Some(OFFSET)),
            Outcome::AlreadyCorrect { offset: OFFSET }
        );
        // A zero offset never shifted anything.
        assert_eq!(
            classify(WALL, Some(WALL), Some(0)),
            Outcome::AlreadyCorrect { offset: 0 }
        );
        // Negative offsets were subtracted too: 12:00 at -05:00 was 17:00.
        assert_eq!(
            classify(WALL + 300 * 60, Some(WALL), Some(-300)),
            Outcome::Shifted {
                taken_at: WALL,
                offset: -300
            }
        );
        assert_eq!(
            classify(WALL + 7, Some(WALL), Some(OFFSET)),
            Outcome::Unexplained { offset: OFFSET }
        );
        assert_eq!(
            classify(WALL, None, Some(OFFSET)),
            Outcome::Unexplained { offset: OFFSET }
        );
    }

    #[tokio::test]
    async fn shifted_row_is_corrected() {
        let fx = Fixture::new().await;
        let id = fx
            .add("a.jpg", &jpeg_with_exif(WALL_EXIF, Some("+11:00")), SHIFTED)
            .await;

        let summary = fx
            .service(LocalStorageMode::Managed)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));
        assert_eq!(summary.scanned, 1);
        assert_eq!(summary.corrected, 1);
    }

    #[tokio::test]
    async fn running_twice_does_not_double_shift() {
        let fx = Fixture::new().await;
        let id = fx
            .add("a.jpg", &jpeg_with_exif(WALL_EXIF, Some("+11:00")), SHIFTED)
            .await;
        let svc = fx.service(LocalStorageMode::Managed);

        svc.backfill_taken_at_offsets().await.unwrap().unwrap();
        // The global marker short-circuits a second run.
        assert!(svc.backfill_taken_at_offsets().await.unwrap().is_none());
        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));

        // Even with the marker gone (e.g. a crash before it was written),
        // the per-row offset keeps the row out of the scan.
        fx.clear_marker().await;
        let summary = svc.backfill_taken_at_offsets().await.unwrap().unwrap();
        assert_eq!(summary.scanned, 0);
        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));
    }

    #[tokio::test]
    async fn row_imported_after_675_is_left_alone() {
        // A build with #675 but without this column stored the wall clock
        // and no offset. It must not be shifted.
        let fx = Fixture::new().await;
        let id = fx
            .add("a.jpg", &jpeg_with_exif(WALL_EXIF, Some("+11:00")), WALL)
            .await;

        let summary = fx
            .service(LocalStorageMode::Managed)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));
        assert_eq!(summary.already_correct, 1);
        assert_eq!(summary.corrected, 0);
    }

    #[tokio::test]
    async fn immich_rows_are_untouched() {
        let fx = Fixture::new().await;
        std::fs::write(
            fx.originals.join("a.jpg"),
            jpeg_with_exif(WALL_EXIF, Some("+11:00")),
        )
        .unwrap();
        let id = MediaId::generate();
        let mut record = record_with_taken_at(id.clone(), "a.jpg", Some(SHIFTED));
        record.external_id = Some("immich-asset-uuid".to_string());
        fx.media.insert(&record).await.unwrap();

        let summary = fx
            .service(LocalStorageMode::Managed)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.scanned, 0);
        assert_eq!(fx.taken_at(&id).await, (Some(SHIFTED), None));
    }

    #[tokio::test]
    async fn moments_renders_are_untouched() {
        // A render copies its source's `taken_at`; it is never repaired
        // on its own, even before its upload stamps an external id.
        let fx = Fixture::new().await;
        std::fs::write(
            fx.originals.join("a-edit.jpg"),
            jpeg_with_exif(WALL_EXIF, Some("+11:00")),
        )
        .unwrap();
        let id = MediaId::generate();
        let mut record = record_with_taken_at(id.clone(), "a-edit.jpg", Some(SHIFTED));
        record.is_moments_render = true;
        fx.media.insert(&record).await.unwrap();

        let summary = fx
            .service(LocalStorageMode::Managed)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.scanned, 0);
        assert_eq!(fx.taken_at(&id).await, (Some(SHIFTED), None));
    }

    #[tokio::test]
    async fn row_without_offset_tag_is_untouched() {
        let fx = Fixture::new().await;
        let id = fx
            .add("a.jpg", &jpeg_with_exif(WALL_EXIF, None), WALL)
            .await;

        let summary = fx
            .service(LocalStorageMode::Managed)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.no_offset_tag, 1);
        assert_eq!(fx.taken_at(&id).await, (Some(WALL), None));
    }

    #[tokio::test]
    async fn unreachable_file_is_skipped() {
        let fx = Fixture::new().await;
        // Referenced mode: `relative_path` is absolute, and the file is gone.
        let missing = fx.originals.join("moved-away.jpg");
        let id = MediaId::generate();
        fx.media
            .insert(&record_with_taken_at(
                id.clone(),
                missing.to_str().unwrap(),
                Some(SHIFTED),
            ))
            .await
            .unwrap();

        let summary = fx
            .service(LocalStorageMode::Referenced)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.unreachable, 1);
        assert_eq!(fx.taken_at(&id).await, (Some(SHIFTED), None));
    }

    #[tokio::test]
    async fn referenced_file_is_corrected() {
        let fx = Fixture::new().await;
        let path = fx.originals.join("elsewhere.jpg");
        std::fs::write(&path, jpeg_with_exif(WALL_EXIF, Some("+11:00"))).unwrap();
        let id = MediaId::generate();
        fx.media
            .insert(&record_with_taken_at(
                id.clone(),
                path.to_str().unwrap(),
                Some(SHIFTED),
            ))
            .await
            .unwrap();

        fx.service(LocalStorageMode::Referenced)
            .backfill_taken_at_offsets()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));
    }

    #[tokio::test]
    async fn import_insert_records_offset() {
        let fx = Fixture::new().await;
        let id = MediaId::generate();
        fx.media
            .insert_with_taken_at_offset(
                &record_with_taken_at(id.clone(), "a.jpg", Some(WALL)),
                Some(OFFSET),
            )
            .await
            .unwrap();

        // Recorded at import, so the backfill never looks at it.
        assert_eq!(fx.taken_at(&id).await, (Some(WALL), Some(OFFSET)));
        assert!(fx
            .media
            .taken_at_offset_candidates()
            .await
            .unwrap()
            .is_empty());
    }
}
