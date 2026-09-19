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

/// How far to rotate a camera's video before scaling to its configured
/// resolution - for a camera physically mounted sideways or upside down.
/// Applied in the capture pipeline itself (see `omni-capture::pipeline`),
/// so it affects the live view, recordings, and the RTSP re-serve alike,
/// not just what's displayed in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    None,
    Clockwise90,
    Rotate180,
    CounterClockwise90,
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

/// What gates whether the recording branch actually writes video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordingTrigger {
    /// Always recording while enabled - the "forever loop" default.
    #[default]
    Continuous,
    /// Only recording while motion is detected (implies motion detection
    /// runs for this camera regardless of `MotionSettings::enabled`).
    Motion,
}

/// Continuous loop-recording settings for one camera. When `enabled`, the
/// capture pipeline gains a second branch (GStreamer `splitmuxsink`) that
/// writes fixed-length segment files to disk indefinitely (or only while
/// motion is active, if `trigger` is `Motion` - gated by a GStreamer
/// `valve` toggled live, no pipeline restart needed); a background reaper
/// (`omni-server::retention`) deletes the oldest segments once
/// `retention_max_age_secs` and/or `retention_max_size_bytes` is exceeded -
/// "forever loop" recording bounded by age and/or total size, whichever
/// limit is hit first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSettings {
    pub enabled: bool,
    #[serde(default)]
    pub trigger: RecordingTrigger,
    /// Length of each recorded segment file, in seconds.
    pub segment_seconds: u32,
    /// Delete segments older than this many seconds. `None` = no age limit.
    pub retention_max_age_secs: Option<u64>,
    /// Delete oldest segments once the camera's recordings directory
    /// exceeds this many bytes. `None` = no size limit.
    pub retention_max_size_bytes: Option<u64>,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: RecordingTrigger::Continuous,
            segment_seconds: 300,
            retention_max_age_secs: None,
            retention_max_size_bytes: None,
        }
    }
}

/// Motion detection settings for one camera. Runs independently of
/// recording - it can drive a webhook, a `Motion` recording trigger, or
/// both, or neither (just logged events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionSettings {
    pub enabled: bool,
    /// 1 (least sensitive - needs a large change) to 100 (most sensitive
    /// - a small change triggers it).
    pub sensitivity: u8,
    /// POSTed a JSON body to on motion start. `None` = no webhook.
    pub webhook_url: Option<String>,
}

impl Default for MotionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            sensitivity: 50,
            webhook_url: None,
        }
    }
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
    #[serde(default)]
    pub recording: RecordingSettings,
    #[serde(default)]
    pub motion: MotionSettings,
    #[serde(default)]
    pub rotation: Rotation,
    /// Lower sorts first in the dashboard grid; ties broken by creation
    /// order. Only meaningful relative to other cameras' values - set via
    /// `PUT /api/cameras/reorder`, not directly.
    #[serde(default)]
    pub sort_order: i64,
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
            recording: RecordingSettings::default(),
            motion: MotionSettings::default(),
            rotation: Rotation::default(),
            sort_order: 0,
            status: None,
        }
    }
}

/// A logged motion-detection event for one camera. `ended_at` is `None`
/// while motion is still ongoing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionEvent {
    pub id: Uuid,
    pub camera_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}
