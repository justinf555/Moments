// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

/// A one-shot maintenance task, keyed in the `library_maintenance` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceTask {
    /// Issue #677: add the EXIF `OffsetTime*` back to `taken_at` for local
    /// rows imported before #675 stopped subtracting it.
    TakenAtOffsetBackfill,
}

impl MaintenanceTask {
    /// Stable key stored in `library_maintenance.task`. Never change an
    /// existing key: a renamed task would run again.
    pub fn key(self) -> &'static str {
        match self {
            MaintenanceTask::TakenAtOffsetBackfill => "taken_at_offset_backfill_677",
        }
    }
}

/// What the #677 backfill did, for logging and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TakenAtBackfillSummary {
    /// Local image rows with a `taken_at` and no recorded offset.
    pub scanned: usize,
    /// Rows whose `taken_at` was shifted back to the capture wall clock.
    pub corrected: usize,
    /// Rows that already held the wall clock (imported after #675, or an
    /// offset of zero); only the offset was recorded.
    pub already_correct: usize,
    /// Rows whose stored value matched neither the shifted nor the
    /// correct reading. Left alone; only the offset was recorded.
    pub unexplained: usize,
    /// Rows whose file has no `OffsetTime*` tag. Left alone.
    pub no_offset_tag: usize,
    /// Rows whose original file could not be found. Left alone.
    pub unreachable: usize,
}
