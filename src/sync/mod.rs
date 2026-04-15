//! Bidirectional Immich sync engine.
//!
//! - **Pull**: Immich → Moments via `/sync/stream`
//! - **Push**: Moments → Immich via outbox pattern

use std::time::Duration;

pub(crate) mod client;
pub(crate) mod downloader;
pub mod outbox;
pub(crate) mod types;

/// How often to check for expired trash items.
const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Maximum number of thumbnail download requests in the queue.
const THUMBNAIL_QUEUE_SIZE: usize = 1000;
/// Delay between dispatching thumbnail downloads to avoid server overload.
const THUMBNAIL_THROTTLE: Duration = Duration::from_millis(5);
/// Flush acks to the database after this many processed entities.
const ACK_FLUSH_THRESHOLD: usize = 500;
