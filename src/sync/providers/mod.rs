// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync backend providers.
//!
//! Each provider implements the pull/push protocol for a specific server.
//! Currently only Immich is supported.

pub mod immich;
