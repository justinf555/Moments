// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

mod builder;
pub mod discovery;
mod error;
mod filter;
mod hasher;
mod metadata;
mod persistence;
mod pipeline;
pub mod thumbnail;
mod types;

pub use builder::ImportPipelineBuilder;
pub use error::ImportError;
pub use pipeline::ImportPipeline;
pub use types::{ImportProgress, ImportSummary, SkipReason};
