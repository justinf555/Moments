use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{debug, instrument, warn};

use crate::renderer::error::RenderError;
use crate::library::thumbnail::thumbnailer::apply_orientation;

use super::registry::{FormatHandler, VIDEO_EXTENSIONS};

/// Extracts a poster frame from video files via GStreamer.
///
/// Uses a pipeline: `filesrc → decodebin → videoconvert → appsink`
/// to pull a single RGB frame. Reads rotation metadata from the
/// container and applies orientation correction manually, since
/// `videoflip method=automatic` doesn't work for all container types.
pub struct VideoHandler;

impl FormatHandler for VideoHandler {
    fn extensions(&self) -> &[&str] {
        VIDEO_EXTENSIONS
    }

    #[instrument(skip(self), fields(path = %path.display()))]
    fn decode(&self, path: &Path) -> Result<image::DynamicImage, RenderError> {
        extract_poster_frame(path)
    }
}

/// Extract the first video frame as an `image::DynamicImage`.
fn extract_poster_frame(path: &Path) -> Result<image::DynamicImage, RenderError> {
    let uri = format!(
        "file://{}",
        path.canonicalize().map_err(RenderError::Io)?.display()
    );

    let pipeline = gst::parse::launch(&format!(
        "uridecodebin uri={uri} ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink"
    ))
    .map_err(|e| RenderError::DecodeFailed(format!("GStreamer pipeline error: {e}")))?;

    let pipeline = pipeline
        .downcast::<gst::Pipeline>()
        .map_err(|_| RenderError::DecodeFailed("failed to downcast to Pipeline".into()))?;

    let sink = pipeline
        .by_name("sink")
        .ok_or_else(|| RenderError::DecodeFailed("appsink not found in pipeline".into()))?;

    let appsink = sink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| RenderError::DecodeFailed("failed to downcast to AppSink".into()))?;

    // Limit to 1 buffer — we only need one frame.
    appsink.set_max_buffers(1);
    appsink.set_drop(true);

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| RenderError::DecodeFailed(format!("failed to start pipeline: {e}")))?;

    // Pull one sample (blocks until a frame is decoded or EOS/error).
    let sample = appsink
        .pull_sample()
        .map_err(|_| RenderError::DecodeFailed("no frame decoded from video".into()))?;

    let buffer = sample
        .buffer()
        .ok_or_else(|| RenderError::DecodeFailed("sample has no buffer".into()))?;

    let caps = sample
        .caps()
        .ok_or_else(|| RenderError::DecodeFailed("sample has no caps".into()))?;

    let video_info = gstreamer_video::VideoInfo::from_caps(caps)
        .map_err(|e| RenderError::DecodeFailed(format!("invalid video caps: {e}")))?;

    let width = video_info.width();
    let height = video_info.height();

    let map = buffer
        .map_readable()
        .map_err(|_| RenderError::DecodeFailed("failed to map buffer".into()))?;

    debug!(width, height, bytes = map.len(), "extracted poster frame");

    let img = image::RgbImage::from_raw(width, height, map.to_vec()).ok_or_else(|| {
        RenderError::DecodeFailed(format!(
            "RGB buffer size mismatch: {}x{} expected {} bytes, got {}",
            width,
            height,
            width as usize * height as usize * 3,
            map.len()
        ))
    })?;

    // Read rotation from stream tags before shutting down the pipeline.
    let rotation = read_rotation_tag(&appsink);

    // Best-effort: pipeline may already be torn down.
    let _ = pipeline.set_state(gst::State::Null);

    let img = image::DynamicImage::ImageRgb8(img);

    // Apply rotation if present.
    let orientation = rotation_to_exif_orientation(rotation);
    if orientation != 1 {
        debug!(rotation, orientation, "applying video rotation correction");
    }
    Ok(apply_orientation(img, orientation))
}

/// Read the rotation tag from the appsink's sticky tag event.
///
/// Returns the rotation in degrees (0, 90, 180, 270). Falls back to 0 if
/// no rotation metadata is found.
fn read_rotation_tag(appsink: &gst_app::AppSink) -> i32 {
    // Check sticky Tag events on the appsink's sink pad — tags propagate
    // downstream through the pipeline.
    let Some(pad) = appsink.static_pad("sink") else {
        debug!("appsink has no sink pad");
        return 0;
    };

    // Iterate sticky events looking for Tag events.
    let mut rotation = 0i32;
    pad.sticky_events_foreach(|event| {
        if let gst::EventView::Tag(tag_event) = event.view() {
            let tag_list = tag_event.tag();
            if let Some(orient) = tag_list.get::<gst::tags::ImageOrientation>() {
                let val = orient.get();
                debug!(orientation = %val, "found image-orientation tag");
                rotation = match val {
                    "rotate-90" => 90,
                    "rotate-180" => 180,
                    "rotate-270" => 270,
                    _ => 0,
                };
            }
        }
        std::ops::ControlFlow::Continue(gst::EventForeachAction::Keep)
    });

    if rotation == 0 {
        debug!("no rotation tag found in video");
    }

    rotation
}

/// Map video rotation degrees to EXIF orientation values.
fn rotation_to_exif_orientation(degrees: i32) -> u8 {
    match degrees {
        90 => 6,  // Rotated 90° CW
        180 => 3, // Rotated 180°
        270 => 8, // Rotated 90° CCW (270° CW)
        _ => 1,   // Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_to_orientation_mapping() {
        assert_eq!(rotation_to_exif_orientation(0), 1);
        assert_eq!(rotation_to_exif_orientation(90), 6);
        assert_eq!(rotation_to_exif_orientation(180), 3);
        assert_eq!(rotation_to_exif_orientation(270), 8);
        assert_eq!(rotation_to_exif_orientation(45), 1); // Unknown → normal
    }
}
