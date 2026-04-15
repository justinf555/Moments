//! Immich sync provider.
//!
//! Implements bidirectional sync with an Immich server:
//! - **Pull**: streams from `/sync/stream`, upserts locally
//! - **Push**: drains the outbox, maps mutations to Immich API calls
//! - **ThumbnailDownloader**: bounded worker pool for thumbnail fetching
//! - **CachedResolver**: fetches originals from Immich on cache miss

use std::time::Duration;

pub(crate) mod client;
pub(crate) mod downloader;
pub(crate) mod pull;
pub(crate) mod push;
pub mod resolver;
pub(crate) mod types;

/// Maximum concurrent thumbnail downloads.
pub(crate) const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Bounded channel capacity for thumbnail download requests.
pub(crate) const THUMBNAIL_QUEUE_SIZE: usize = 1000;
/// Delay between dispatching thumbnail downloads to avoid server overload.
pub(crate) const THUMBNAIL_THROTTLE: Duration = Duration::from_millis(5);
/// Flush acks to the database after this many processed entities.
pub(crate) const ACK_FLUSH_THRESHOLD: usize = 500;
