// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! One-shot library repairs run when a library is opened.
//!
//! Each task records completion in the `library_maintenance` table so it
//! runs at most once per library. Today there is one task: the #677
//! capture-time backfill for local rows imported before #675.

mod model;
pub mod repository;
mod service;

pub use model::{MaintenanceTask, TakenAtBackfillSummary};
pub use service::MaintenanceService;
