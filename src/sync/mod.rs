//! Bidirectional Immich sync engine.
//!
//! - **Pull**: Immich → Moments via `/sync/stream`
//! - **Push**: Moments → Immich via outbox pattern

pub(crate) mod client;
pub mod outbox;
pub(crate) mod types;
