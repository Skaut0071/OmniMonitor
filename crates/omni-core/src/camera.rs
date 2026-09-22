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

/// Restricts recording to specific days/times of the week - independent
/// of `RecordingTrigger` (e.g. "continuous, but only overnight" or "only
/// on motion, and only on weekdays" both make sense). `enabled: false`
/// means no restriction at all - recording follows `trigger` alone, same
/// as before this existed.
///
/// Deliberately just data plus a pure predicate here (no clock access -
/// this crate stays usable from `omni-wasm`/the browser, which has no
/// business asking "what time is it on the server"); the actual "is it
/// currently within the scheduled window" check lives in
/// `omni-server::supervisor`, which has both a real clock and the
/// server's local timezone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSchedule {
    pub enabled: bool,
    /// One flag per weekday, Monday first (index 0) - matches
    /// `chrono::Weekday::num_days_from_monday()`, so the caller
    /// evaluating this doesn't have to translate between the two.
    pub days: [bool; 7],
    /// Minutes since local midnight, 0-1439.
    pub start_minute: u16,
    /// Minutes since local midnight, 0-1439. Less than `start_minute`
    /// means the window wraps past midnight (e.g. 22:00-06:00).
    pub end_minute: u16,
}

impl Default for RecordingSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            days: [true; 7],
            start_minute: 0,
            end_minute: 1439,
        }
    }
}

impl RecordingSchedule {
    /// Pure predicate: is `minute_of_day` (0-1439) on `weekday_mon0`
    /// (0=Monday..6=Sunday) inside the scheduled window? Always `true`
    /// when the schedule itself is disabled - "no schedule" means "no
    /// restriction", not "never record".
    pub fn is_active_at(&self, weekday_mon0: u8, minute_of_day: u16) -> bool {
        if !self.enabled {
            return true;
        }
        let Some(&is_scheduled_day) = self.days.get(weekday_mon0 as usize) else {
            return false;
        };
        if !is_scheduled_day {
            return false;
        }
        if self.start_minute <= self.end_minute {
            (self.start_minute..=self.end_minute).contains(&minute_of_day)
        } else {
            // Wraps past midnight, e.g. 22:00-06:00: "active" is
            // everything from start to end-of-day, plus everything from
            // start-of-day to end - the two halves of the wrapped range.
            minute_of_day >= self.start_minute || minute_of_day <= self.end_minute
        }
    }
}

/// Continuous loop-recording settings for one camera. When `enabled`, the
/// capture pipeline gains a second branch (GStreamer `splitmuxsink`) that
/// writes fixed-length segment files to disk indefinitely, or only while
/// motion is active (if `trigger` is `Motion`) and/or only within
/// `schedule`'s window if that's enabled - both are whole-pipeline-rebuild
/// decisions (see `docs/ARCHITECTURE.md`'s "Motion detection" section for
/// why, including a GStreamer `valve` that was tried and abandoned), not
/// a live-toggled element. A background reaper (`omni-server::retention`)
/// deletes the oldest segments once `retention_max_age_secs` and/or
/// `retention_max_size_bytes` is exceeded - "forever loop" recording
/// bounded by age and/or total size, whichever limit is hit first.
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
    #[serde(default)]
    pub schedule: RecordingSchedule,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: RecordingTrigger::Continuous,
            segment_seconds: 300,
            retention_max_age_secs: None,
            retention_max_size_bytes: None,
            schedule: RecordingSchedule::default(),
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
    /// Burns the current wall-clock date/time into the video itself (via
    /// the capture pipeline, like `rotation` - so it's in the live view,
    /// recordings, and the RTSP re-serve alike, not just an overlay drawn
    /// by the browser). Off by default since not everyone wants it.
    #[serde(default)]
    pub overlay_timestamp: bool,
    /// Lower sorts first in the dashboard grid; ties broken by creation
    /// order. Only meaningful relative to other cameras' values - set via
    /// `PUT /api/cameras/reorder`, not directly.
    #[serde(default)]
    pub sort_order: i64,
    /// Freeform organizational tag shown as a dashboard tab - deliberately
    /// just a string on each camera rather than a normalized `groups`
    /// table with its own id: nothing else needs to reference a group by
    /// id, and "rename this tab" is just a bulk find-and-replace across
    /// whichever cameras currently have the old name (see
    /// `Db::rename_camera_group`). `None`/empty means ungrouped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
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
            overlay_timestamp: false,
            sort_order: 0,
            group: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(days: [bool; 7], start: u16, end: u16) -> RecordingSchedule {
        RecordingSchedule { enabled: true, days, start_minute: start, end_minute: end }
    }

    #[test]
    fn disabled_schedule_is_always_active() {
        let s = RecordingSchedule { enabled: false, days: [false; 7], start_minute: 0, end_minute: 0 };
        // Every day disallowed, every minute out of a normally-empty
        // window - still "active", because a disabled schedule means no
        // restriction at all, not "restricted to nothing".
        assert!(s.is_active_at(0, 0));
        assert!(s.is_active_at(6, 1439));
    }

    #[test]
    fn same_day_window_boundaries_are_inclusive() {
        let s = schedule([true; 7], 60, 120); // 01:00-02:00
        assert!(!s.is_active_at(0, 59));
        assert!(s.is_active_at(0, 60));
        assert!(s.is_active_at(0, 90));
        assert!(s.is_active_at(0, 120));
        assert!(!s.is_active_at(0, 121));
    }

    #[test]
    fn wrapping_window_spans_midnight() {
        let s = schedule([true; 7], 22 * 60, 6 * 60); // 22:00-06:00
        assert!(s.is_active_at(0, 23 * 60)); // 23:00, before midnight
        assert!(s.is_active_at(0, 0)); // exactly midnight
        assert!(s.is_active_at(0, 5 * 60 + 59)); // 05:59, after midnight
        assert!(!s.is_active_at(0, 12 * 60)); // noon - outside the window
    }

    #[test]
    fn day_of_week_restriction_is_honored() {
        let mut days = [false; 7];
        days[5] = true; // Saturday only
        let s = schedule(days, 0, 1439);
        assert!(s.is_active_at(5, 600));
        assert!(!s.is_active_at(4, 600)); // Friday
    }
}
