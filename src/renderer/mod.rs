//! Unified render pipeline.
//!
//! Each step is a separate module:
//! - [`decode`] — magic-byte format detection + decode
//! - [`orientation`] — EXIF orientation correction
//! - [`resize`] — thumbnail sizing
//! - [`edits`] — non-destructive edit application
//! - [`output`] — RGBA / WebP conversion helpers
//! - [`pipeline`] — orchestrator that composes the steps

pub mod decode;
pub mod edits;
pub mod format;
pub mod orientation;
pub mod output;
pub mod pipeline;
pub mod resize;

// TODO: Remove once all callers use the pipeline directly.
pub use edits::apply_edits;
