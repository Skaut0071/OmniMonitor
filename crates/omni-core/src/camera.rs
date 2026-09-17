use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a camera's video is being sourced.
///
/// The whole point of OmniMonitor's USB support is that a `Usb` camera is
/// treated as a first-class citizen alongside network cameras: both end up
/// as a WebRTC (and later RTSP) stream with the same downstream pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CameraKind {
    /// A USB Video Class (UVC) device captured via V4L2, e.g. `/dev/video0`.
    Usb { device_path: String },
    /// A network camera reachable over RTSP.
    Rtsp { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCodec {
    Vp8,
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraStatus {
    /// Known in the DB but no capture pipeline is running.
    Idle,
    /// Capture pipeline is running and producing frames.
    Streaming,
    /// Capture pipeline failed to start or died.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Camera {
    pub id: Uuid,
    pub name: String,
    #[serde(flatten)]
    pub kind: CameraKind,
    pub enabled: bool,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub codec: StreamCodec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<CameraStatus>,
}

impl Camera {
    pub fn new_usb(name: impl Into<String>, device_path: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: CameraKind::Usb {
                device_path: device_path.into(),
            },
            enabled: true,
            width: 1280,
            height: 720,
            framerate: 30,
            codec: StreamCodec::Vp8,
            status: None,
        }
    }
}
