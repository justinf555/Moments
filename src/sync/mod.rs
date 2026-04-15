//! Bidirectional Immich sync engine.
//!
//! - **Pull**: Immich → Moments via `/sync/stream`
//! - **Push**: Moments → Immich via outbox pattern

pub mod outbox;
