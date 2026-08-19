// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Unified render pipeline.
//!
//! Each step is a separate module:
//! - [`decode`] — magic-byte format detection + decode
//! - [`orientation`] — EXIF orientation correction
//! - [`resize`] — thumbnail sizing
//! - [`edits`] — non-destructive edit application
//! - [`output`] — RGBA / WebP conversion helpers
//! - [`pipeline`] — orchestrator that composes the steps
//! - [`target`] — `RenderTarget` hint for kernel-scaling stages

pub mod decode;
pub mod edits;
pub mod error;
pub mod format;
pub mod orientation;
pub mod output;
pub mod pipeline;
pub mod resize;
pub mod target;
pub mod xmp;
