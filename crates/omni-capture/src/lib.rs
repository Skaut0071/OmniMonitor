//! USB camera capture: device discovery over V4L2 and an encode pipeline
//! built with GStreamer. The whole point of OmniMonitor's "USB cameras act
//! like network cameras" pitch lives here - a `/dev/videoN` UVC device is
//! turned into the same kind of encoded-frame stream a network camera's
//! RTSP source would produce, so everything downstream (WebRTC track,
//! future recorder) doesn't need to know the difference.

pub mod discover;
pub mod pipeline;

pub use discover::{list_capture_devices, DiscoveredDevice};
pub use pipeline::{CaptureHandle, CaptureSession, EncodedFrame, PipelineConfig};

/// Must be called once before any `CaptureSession` is created.
pub fn init() -> anyhow::Result<()> {
    gstreamer::init()?;
    Ok(())
}
