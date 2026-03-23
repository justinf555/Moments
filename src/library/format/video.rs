use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{debug, instrument};

use crate::library::error::LibraryError;

use super::registry::{FormatHandler, VIDEO_EXTENSIONS};

/// Extracts a poster frame from video files via GStreamer.
///
/// Uses a pipeline: `filesrc → decodebin → videoconvert → appsink`
/// to pull a single RGB frame, then converts it to an `image::DynamicImage`.
pub struct VideoHandler;

impl FormatHandler for VideoHandler {
    fn extensions(&self) -> &[&str] {
        VIDEO_EXTENSIONS
    }

    #[instrument(skip(self), fields(path = %path.display()))]
    fn decode(&self, path: &Path) -> Result<image::DynamicImage, LibraryError> {
        extract_poster_frame(path)
    }
}

/// Extract the first video frame as an `image::DynamicImage`.
fn extract_poster_frame(path: &Path) -> Result<image::DynamicImage, LibraryError> {
    let uri = format!(
        "file://{}",
        path.canonicalize()
            .map_err(LibraryError::Io)?
            .display()
    );

    let pipeline = gst::parse::launch(&format!(
        "uridecodebin uri={uri} ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink"
    ))
    .map_err(|e| LibraryError::Thumbnail(format!("GStreamer pipeline error: {e}")))?;

    let pipeline = pipeline
        .downcast::<gst::Pipeline>()
        .map_err(|_| LibraryError::Thumbnail("failed to downcast to Pipeline".into()))?;

    let sink = pipeline
        .by_name("sink")
        .ok_or_else(|| LibraryError::Thumbnail("appsink not found in pipeline".into()))?;

    let appsink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| LibraryError::Thumbnail("failed to downcast to AppSink".into()))?;

    // Limit to 1 buffer — we only need one frame.
    appsink.set_max_buffers(1);
    appsink.set_drop(true);

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| LibraryError::Thumbnail(format!("failed to start pipeline: {e}")))?;

    // Pull one sample (blocks until a frame is decoded or EOS/error).
    let sample = appsink
        .pull_sample()
        .map_err(|_| LibraryError::Thumbnail("no frame decoded from video".into()))?;

    let buffer = sample
        .buffer()
        .ok_or_else(|| LibraryError::Thumbnail("sample has no buffer".into()))?;

    let caps = sample
        .caps()
        .ok_or_else(|| LibraryError::Thumbnail("sample has no caps".into()))?;

    let video_info = gstreamer_video::VideoInfo::from_caps(caps)
        .map_err(|e| LibraryError::Thumbnail(format!("invalid video caps: {e}")))?;

    let width = video_info.width();
    let height = video_info.height();

    let map = buffer
        .map_readable()
        .map_err(|_| LibraryError::Thumbnail("failed to map buffer".into()))?;

    debug!(width, height, bytes = map.len(), "extracted poster frame");

    let img = image::RgbImage::from_raw(width, height, map.to_vec()).ok_or_else(|| {
        LibraryError::Thumbnail(format!(
            "RGB buffer size mismatch: {}x{} expected {} bytes, got {}",
            width,
            height,
            width as usize * height as usize * 3,
            map.len()
        ))
    })?;

    // Clean up pipeline.
    let _ = pipeline.set_state(gst::State::Null);

    Ok(image::DynamicImage::ImageRgb8(img))
}
