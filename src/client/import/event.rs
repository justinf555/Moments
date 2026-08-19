// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::importer::{ImportProgress, ImportSummary};

/// Client-internal events for decoupling the import pipeline from
/// GObject property updates. The pipeline callback sends these on a
/// channel; the `listen()` loop receives and updates properties.
#[derive(Debug, Clone)]
pub(super) enum ImportEvent {
    Progress(ImportProgress),
    Complete(ImportSummary),
}
