//! Immich sync provider.
//!
//! Implements bidirectional sync with an Immich server:
//! - **Pull**: streams from `/sync/stream`, upserts locally
//! - **Push**: drains the outbox, maps mutations to Immich API calls
//! - **ThumbnailDownloader**: bounded worker pool for thumbnail fetching
//! - **CachedResolver**: fetches originals from Immich on cache miss

pub(crate) mod client;
pub(crate) mod pull;
pub(crate) mod push;
pub mod resolver;
pub(crate) mod types;

/// Flush acks to the database after this many processed entities.
pub(crate) const ACK_FLUSH_THRESHOLD: usize = 500;
