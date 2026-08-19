// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Render-target hint for edit stages.
//!
//! Lives in its own module so both [`super::pipeline`] and [`super::edits`]
//! can depend on it without creating a cycle.

/// Hint to neighbourhood-operation edit stages (sharpness, noise reduction)
/// about how the rendered image will be used.
///
/// Per-pixel stages (exposure, color, vignette, HSL) ignore this. Stages
/// that use a kernel scaled to image dimensions read it to pick a kernel
/// size appropriate for the output's intended use.
///
/// No `Default` impl — every call site states its intent so the
/// performance-sensitive `Preview` path is never selected by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    /// Live edit-panel preview, typically downsampled. Stages may use
    /// cheaper kernels to keep slider response snappy.
    Preview,
    /// Final render at the image's natural size — persisted thumbnails,
    /// viewer full-resolution display, exports, Immich upload. Stages
    /// use full-quality kernels.
    Final,
}
