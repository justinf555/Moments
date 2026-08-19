// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod detect;
pub(crate) mod raw;
pub(crate) mod registry;
pub(crate) mod standard;
pub(crate) mod video;

pub use registry::VIDEO_EXTENSIONS;
pub(crate) use registry::{DecodeHint, FormatRegistry};
