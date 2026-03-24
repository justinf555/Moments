use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, instrument, warn};

/// Metadata extracted from a video file's container headers.
///
/// All fields are `Option` — missing or unreadable metadata never fails
/// the import pipeline. Future fields (codec, frame rate, etc.) will be
/// added here.
#[derive(Debug, Default)]
pub struct VideoMetaInfo {
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
}

/// Extract metadata from a video file via GStreamer.
///
/// Moves the pipeline to `PAUSED` (reads container headers without
/// decoding frames) and queries duration. Cheap enough to run on a
/// blocking thread during import.
#[instrument(skip_all, fields(path = %path.display()))]
pub fn extract_video_metadata(path: &Path) -> VideoMetaInfo {
    let mut info = VideoMetaInfo::default();

    // Ensure GStreamer is initialized (no-op if already done in main).
    if gst::init().is_err() {
        warn!("GStreamer not available — skipping video metadata extraction");
        return info;
    }

    let uri = match path.canonicalize() {
        Ok(p) => format!("file://{}", p.display()),
        Err(e) => {
            warn!("failed to canonicalize path: {e}");
            return info;
        }
    };

    let pipeline = match gst::parse::launch(&format!("uridecodebin uri={uri} ! fakesink")) {
        Ok(p) => p,
        Err(e) => {
            warn!("GStreamer pipeline error: {e}");
            return info;
        }
    };

    let Ok(pipeline) = pipeline.downcast::<gst::Pipeline>() else {
        warn!("failed to downcast to Pipeline");
        return info;
    };

    // PAUSED reads container headers without decoding frames.
    if pipeline.set_state(gst::State::Paused).is_err() {
        warn!("failed to pause pipeline");
        let _ = pipeline.set_state(gst::State::Null);
        return info;
    }

    // Wait for state change (up to 5 seconds).
    let _ = pipeline.state(Some(gst::ClockTime::from_seconds(5)));

    if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
        let ms = duration.mseconds();
        debug!(duration_ms = ms, "extracted video duration");
        info.duration_ms = Some(ms);
    } else {
        warn!("could not determine video duration");
    }

    let _ = pipeline.set_state(gst::State::Null);
    info
}
