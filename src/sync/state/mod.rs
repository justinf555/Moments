// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync-engine state persistence: ack checkpoints + per-line audit log.
//!
//! These tables are sync-engine concerns (resume points, error tracing)
//! and don't belong to any feature service. Mirrors the structure of
//! [`crate::sync::outbox`].

mod repository;

pub use repository::SyncStateRepository;
