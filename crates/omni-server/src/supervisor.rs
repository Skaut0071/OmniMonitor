//! Owns the one running capture pipeline per camera.
//!
//! Why this exists: a USB device only allows one process to hold it open
//! at a time (verified while building v0.1 - a second `v4l2src` against
//! the same `/dev/videoN` fails with "device busy"), and recording +
//! live-viewing need to run *simultaneously* against the same camera. So
//! there can only be one `CaptureSession` per camera; every live viewer
//! subscribes to its broadcast channel instead of starting their own.
//!
//! Lifecycle:
//! - A camera with standalone motion detection enabled, or with
//!   recording enabled and either no schedule restricting it or its
//!   scheduled window currently active, gets a persistent pipeline,
//!   started at server boot and kept running regardless of viewers -
//!   see `keeps_pipeline_alive`. A schedule-gated recording camera's
//!   pipeline actually stops outside its window (v0.14) rather than
//!   sitting open unused - see `schedule.rs`.
//! - A camera being watched but not recorded/detected gets an ephemeral
//!   pipeline: started on the first viewer, stopped when the last one
//!   disconnects.
//! - Changing a camera's settings applies immediately, via whichever of
//!   two paths `apply_settings` decides is actually safe:
//!   - Resolution, framerate, rotation, the timestamp overlay, an RTSP
//!     URL edit, or motion detection turning on/off all change what's
//!     actually being decoded/encoded - there's no way to apply those
//!     except restarting the whole pipeline (`replace_pipeline`), which
//!     deliberately disconnects active viewers (with a clear error)
//!     rather than leaving them silently frozen; the frontend already
//!     has a retry path for this.
//!   - Recording turning on/off (an explicit toggle, a schedule boundary
//!     crossing, or a segment-length change) doesn't change any of
//!     that - only whether/how the recording branch is attached to the
//!     otherwise-unchanged pipeline - so it's applied by dynamically
//!     adding/removing that branch on the live pipeline instead
//!     (`CaptureSession::set_recording`), which doesn't touch the live-
//!     view branch at all and so doesn't disconnect anyone watching.
//!
//!   `apply_settings` tells these apart by comparing a `StructuralConfig`
//!   snapshot (everything in the first list) against what the currently
//!   running pipeline was actually built with.
//! - `RecordingTrigger::Motion` ("only record while motion is detected")
//!   deliberately does *not* work this way: the recording branch stays
//!   present in the pipeline for as long as recording is enabled at all,
//!   regardless of the current motion state (see `pipeline_config`) - it
//!   used to rebuild the whole pipeline on every motion start/stop, which
//!   both disconnected every live viewer of that camera on every
//!   transition (visible as a brief "connection lost"/black-frame flicker
//!   any time something moved) and meant a recorded clip only started
//!   *after* motion was already detected, with no lead-in. Instead,
//!   `omni-server::motion_retention` prunes after the fact - see
//!   `omni_core::RecordingSettings`'s docs.
//!
//! LED ring control (`omni_core::LedControl`, `omni-server::led`): for a
//! camera that isn't kept alive on its own (no recording, no motion
//! detection - see `keeps_pipeline_alive`), its pipeline only exists
//! while someone is actually watching. `acquire_viewer` runs
//! `led_control.on_command` exactly when such a camera's pipeline is
//! freshly spawned (the first viewer), and `ViewerGuard::drop` runs
//! `off_command` exactly when it's torn down (the last viewer leaving) -
//! so the LED tracks "is anyone watching" for cameras that otherwise have
//! no other reason to be capturing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use omni_capture::{
    CaptureError, CaptureHandle, CaptureSession, CaptureSource, EncodedFrame, MotionConfig,
    PipelineConfig, RecordingSink,
};
use chrono::{Datelike, Timelike};
use omni_core::{Camera, CameraKind, RecordingTrigger};
use omni_db::Db;
use tokio::sync::{broadcast, watch, RwLock};
use uuid::Uuid;

use crate::{led, motion};

const DEFAULT_BITRATE: u32 = 2_000_000;

/// Segment length used for `RecordingTrigger::Motion` instead of the
/// camera's own `segment_seconds` setting - that setting is meant for
/// continuous "forever loop" recording, where a few minutes per file is
/// sensible; motion-triggered recording is pruned to a window around each
/// motion event (see `omni-server::motion_retention`) and a multi-minute
/// segment would often span far more untouched footage than the
/// pre-roll/post-roll margin actually needs, keeping much more than
/// intended.
pub(crate) const MOTION_SEGMENT_SECONDS: u32 = 20;

/// Everything about a `Camera` that determines the *shape* of its
/// GStreamer pipeline - as opposed to whether/how the recording branch
/// is attached to that shape, which `apply_settings` can change live.
/// Recomputed on every settings change and compared against the value
/// captured when the currently-running pipeline was built
/// (`ManagedCamera::structural`); any difference means the pipeline has
/// to be rebuilt, not just have a branch added/removed.
#[derive(Debug, Clone, PartialEq)]
struct StructuralConfig {
    kind: CameraKind,
    width: u32,
    height: u32,
    framerate: u32,
    rotation: omni_core::Rotation,
    overlay_timestamp: bool,
    /// Whether the motion-detection appsink branch needs to exist at
    /// all, i.e. `camera.motion.enabled || camera.recording.trigger ==
    /// RecordingTrigger::Motion`. *Not* whether motion is currently
    /// active - that never changes the pipeline's shape as of v0.12 (see
    /// the module docs), only whether this branch exists in the first
    /// place does.
    need_motion: bool,
}

struct ManagedCamera {
    session: CaptureSession,
    capture_error: watch::Receiver<Option<String>>,
    motion: Option<watch::Receiver<bool>>,
    /// Sent `true` when this instance is replaced (settings restart) or
    /// explicitly stopped, so viewers holding a clone can tell the
    /// difference from a plain capture failure if they ever need to.
    superseded: watch::Sender<bool>,
    viewers: AtomicUsize,
    /// Whether this specific pipeline instance should keep running with
    /// no viewers right now - see `Supervisor::keeps_pipeline_alive`. For
    /// `RecordingTrigger::Motion`, this is true whenever recording is
    /// enabled and not currently gated off by its own schedule: motion
    /// detection itself must keep running throughout that window
    /// regardless of viewers so it can notice the *next* transition,
    /// even while currently not actively recording.
    keep_alive: AtomicBool,
    /// Cloned from `Camera::led_control` at spawn time - see the module
    /// docs' "LED ring control" section.
    led_control: omni_core::LedControl,
    /// The pipeline-shape-relevant fields this instance was actually
    /// built with - see `StructuralConfig`.
    structural: StructuralConfig,
}

pub struct Supervisor {
    data_dir: PathBuf,
    db: Db,
    cameras: RwLock<HashMap<Uuid, Arc<ManagedCamera>>>,
}

/// Held by a live-view WebSocket handler for as long as it's watching a
/// camera. Dropping it releases this viewer's slot; if it was the last
/// one and the camera doesn't need to keep running on its own, the
/// pipeline is torn down (e.g. releasing a USB device) shortly after.
pub struct ViewerGuard {
    camera_id: Uuid,
    managed: Arc<ManagedCamera>,
    supervisor: Arc<Supervisor>,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        let prev = self.managed.viewers.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 && !self.managed.keep_alive.load(Ordering::SeqCst) {
            if let Some(cmd) = &self.managed.led_control.off_command {
                led::spawn_run(self.camera_id, "off", cmd.clone());
            }
            let supervisor = Arc::clone(&self.supervisor);
            let camera_id = self.camera_id;
            let managed = Arc::clone(&self.managed);
            tokio::spawn(async move {
                supervisor.remove_if_current(camera_id, &managed).await;
            });
        }
    }
}

/// Everything a WebRTC viewer needs: the live frame feed, a signal that
/// fires (with a human-readable reason) if the pipeline dies or gets
/// superseded by a settings change, and a guard releasing the viewer slot
/// on drop.
pub struct ViewerHandle {
    pub frames: broadcast::Receiver<EncodedFrame>,
    pub ended: watch::Receiver<Option<String>>,
    pub _guard: ViewerGuard,
}

impl Supervisor {
    pub fn new(data_dir: PathBuf, db: Db) -> Self {
        Self {
            data_dir,
            db,
            cameras: RwLock::new(HashMap::new()),
        }
    }

    pub fn recordings_dir(&self, camera_id: Uuid) -> PathBuf {
        self.data_dir.join("recordings").join(camera_id.to_string())
    }

    /// The running pipeline's health for a camera, or `None` if no
    /// pipeline is currently running for it at all (an ephemeral camera
    /// with no viewers and no recording/motion enabled spends most of its
    /// time in exactly this state - it isn't "broken", just idle). See
    /// `crate::reachability` for how the status API turns this into a
    /// full online/offline/error/streaming answer even for a camera with
    /// no pipeline instantiated right now.
    pub async fn pipeline_status(&self, camera_id: Uuid) -> Option<omni_core::CameraStatus> {
        let map = self.cameras.read().await;
        let managed = map.get(&camera_id)?;
        Some(if managed.capture_error.borrow().is_some() {
            omni_core::CameraStatus::Error
        } else {
            omni_core::CameraStatus::Streaming
        })
    }

    /// Whether motion is considered active right now for a camera, per
    /// its currently-running pipeline (if any - a camera with no pipeline
    /// running has no opinion, reported as inactive).
    pub async fn motion_active(&self, camera_id: Uuid) -> bool {
        self.cameras
            .read()
            .await
            .get(&camera_id)
            .and_then(|m| m.motion.as_ref())
            .map(|rx| *rx.borrow())
            .unwrap_or(false)
    }

    /// Whether the motion-detection appsink branch needs to exist at all
    /// for `camera` - see `StructuralConfig::need_motion`.
    fn need_motion(camera: &Camera) -> bool {
        camera.motion.enabled || camera.recording.trigger == RecordingTrigger::Motion
    }

    fn structural_config(camera: &Camera) -> StructuralConfig {
        StructuralConfig {
            kind: camera.kind.clone(),
            width: camera.width,
            height: camera.height,
            framerate: camera.framerate,
            rotation: camera.rotation,
            overlay_timestamp: camera.overlay_timestamp,
            need_motion: Self::need_motion(camera),
        }
    }

    /// The `RecordingSink` a camera's pipeline should currently have
    /// attached, or `None` if it shouldn't be recording right now -
    /// shared by `pipeline_config` (initial build) and `apply_settings`
    /// (a later dynamic toggle), so the two can never disagree about
    /// what "recording is currently on" means.
    fn desired_recording(&self, camera: &Camera) -> Option<RecordingSink> {
        let recording_now =
            camera.recording.enabled && schedule_is_active_now(&camera.recording.schedule);
        let segment_seconds = if camera.recording.trigger == RecordingTrigger::Motion {
            MOTION_SEGMENT_SECONDS
        } else {
            camera.recording.segment_seconds
        };
        recording_now.then(|| RecordingSink {
            dir: self.recordings_dir(camera.id),
            segment_seconds,
        })
    }

    fn pipeline_config(&self, camera: &Camera, motion_active_now: bool) -> PipelineConfig {
        let source = match &camera.kind {
            CameraKind::Usb { device_path } => CaptureSource::Usb {
                device_path: device_path.clone(),
            },
            CameraKind::Rtsp { url } => CaptureSource::Rtsp { url: url.clone() },
        };
        let need_motion = Self::need_motion(camera);
        // Deliberately *not* gated on `motion_active_now` for
        // `RecordingTrigger::Motion` - see the module docs' explanation of
        // why that used to rebuild the pipeline (and disconnect live
        // viewers) on every motion transition. The recording branch stays
        // present the whole time recording is enabled; `motion_retention`
        // prunes the resulting segments after the fact.
        let recording = self.desired_recording(camera);
        let motion = need_motion.then_some(MotionConfig {
            sensitivity: camera.motion.sensitivity,
            initial_active: motion_active_now,
        });

        // `camera.width`/`height` is the resolution the user picked for
        // the *unrotated* sensor image (e.g. 1280x720 from a landscape
        // USB camera). `videoflip` runs before the scale to that target
        // in the pipeline (see omni-capture::pipeline), so if the target
        // box stays 1280x720 while the content itself is rotated 90/270
        // (now naturally 720x1280-shaped), videoscale has to squash a
        // portrait image into a landscape box - visibly stretched, not
        // just letterboxed. Swapping the target dimensions for a 90/270
        // rotation instead gives the encoded output (live view,
        // recordings, RTSP - all one pipeline) the correct portrait
        // shape, so nothing downstream has to deform it. 180° keeps the
        // same aspect as unrotated, so it doesn't need this.
        let (width, height) = match camera.rotation {
            omni_core::Rotation::Clockwise90 | omni_core::Rotation::CounterClockwise90 => {
                (camera.height, camera.width)
            }
            omni_core::Rotation::None | omni_core::Rotation::Rotate180 => {
                (camera.width, camera.height)
            }
        };

        PipelineConfig {
            source,
            width,
            height,
            framerate: camera.framerate,
            bitrate: DEFAULT_BITRATE,
            recording,
            motion,
            rotation: camera.rotation,
            overlay_timestamp: camera.overlay_timestamp,
        }
    }

    /// Whether this camera needs its pipeline running (device open)
    /// regardless of viewers, right now.
    ///
    /// `motion.enabled` (standalone motion detection, independent of
    /// recording) always keeps it alive - there's no schedule on that,
    /// and no way to notice the next motion event on a camera that isn't
    /// open. `recording.enabled` alone only keeps it alive when there's
    /// no schedule restricting it, or the schedule's window is active
    /// right now - as of v0.14, a schedule-gated recording camera (any
    /// trigger, including `Motion`) actually closes its device outside
    /// the scheduled window rather than sitting open unused, at the cost
    /// of open/close cycles at each boundary (see `schedule.rs`'s docs
    /// for the device-busy risk that's the deliberate tradeoff here).
    pub(crate) fn keeps_pipeline_alive(camera: &Camera) -> bool {
        camera.motion.enabled
            || (camera.recording.enabled
                && (!camera.recording.schedule.enabled
                    || schedule_is_active_now(&camera.recording.schedule)))
    }

    fn spawn_managed(
        self: &Arc<Self>,
        camera: &Camera,
        motion_active_now: bool,
    ) -> Result<Arc<ManagedCamera>, CaptureError> {
        let config = self.pipeline_config(camera, motion_active_now);
        let CaptureHandle {
            session,
            error,
            motion,
        } = CaptureSession::start(config)?;
        tracing::debug!(camera = %camera.id, motion_present = motion.is_some(), motion_active_now, "capture session started");
        if let Some(motion_rx) = &motion {
            motion::spawn_watcher(self.db.clone(), camera, motion_rx.clone());
        }
        let (superseded_tx, _) = watch::channel(false);
        Ok(Arc::new(ManagedCamera {
            session,
            capture_error: error,
            motion,
            superseded: superseded_tx,
            viewers: AtomicUsize::new(0),
            keep_alive: AtomicBool::new(Self::keeps_pipeline_alive(camera)),
            led_control: camera.led_control.clone(),
            structural: Self::structural_config(camera),
        }))
    }

    /// Ensures a pipeline is running for `camera` (starting one if needed,
    /// reusing the existing one otherwise) and returns a viewer's live
    /// feed onto it.
    pub async fn acquire_viewer(
        self: &Arc<Self>,
        camera: &Camera,
    ) -> Result<ViewerHandle, CaptureError> {
        let mut map = self.cameras.write().await;
        let managed = if let Some(existing) = map.get(&camera.id) {
            Arc::clone(existing)
        } else {
            let managed = self.spawn_managed(camera, false)?;
            map.insert(camera.id, Arc::clone(&managed));
            // This is the first viewer for a camera whose pipeline only
            // exists while being watched (`spawn_managed` would already
            // be running for a `keep_alive` camera by the time any viewer
            // showed up, via `ensure_running` at boot/settings-change) -
            // so this is exactly the "someone started watching" moment.
            if !managed.keep_alive.load(Ordering::SeqCst) {
                if let Some(cmd) = &managed.led_control.on_command {
                    led::spawn_run(camera.id, "on", cmd.clone());
                }
            }
            managed
        };
        managed.viewers.fetch_add(1, Ordering::SeqCst);
        drop(map);

        let frames = managed.session.subscribe();
        let ended = merge_end_signals(managed.capture_error.clone(), managed.superseded.subscribe());

        Ok(ViewerHandle {
            frames,
            ended,
            _guard: ViewerGuard {
                camera_id: camera.id,
                managed,
                supervisor: Arc::clone(self),
            },
        })
    }

    /// Starts a persistent pipeline for a camera that needs to keep
    /// running on its own (recording and/or standalone motion detection)
    /// if one isn't already running. Called at boot for every such
    /// camera, and whenever recording/motion is turned on via the API.
    pub async fn ensure_running(self: &Arc<Self>, camera: &Camera) -> Result<(), CaptureError> {
        let mut map = self.cameras.write().await;
        if let std::collections::hash_map::Entry::Vacant(entry) = map.entry(camera.id) {
            let managed = self.spawn_managed(camera, false)?;
            entry.insert(managed);
        }
        Ok(())
    }

    /// Unconditionally restarts `camera`'s pipeline if one is currently
    /// running - no-op otherwise. Preserves the current motion-active
    /// state (if any) across the restart, so an in-progress
    /// motion-triggered recording isn't interrupted by an unrelated
    /// settings tweak. Prefer `apply_settings` for an actual settings
    /// change - it only pays this cost (and disconnects active viewers)
    /// when the change genuinely requires it; this is the primitive it
    /// falls back to, kept available directly for callers that always
    /// want a hard restart regardless (there currently are none outside
    /// `apply_settings` itself, but it's a reasonable thing to want).
    pub async fn restart_if_running(self: &Arc<Self>, camera: &Camera) -> Result<(), CaptureError> {
        let motion_active = self.motion_active(camera.id).await;
        self.replace_pipeline(camera, motion_active).await
    }

    /// Applies a settings change to `camera`'s already-running pipeline,
    /// if one is running - no-op otherwise (the caller, e.g.
    /// `routes::update_camera`, separately calls `ensure_running` for a
    /// camera that needs to start fresh). Takes the cheapest path that's
    /// actually safe - see the module docs' "Changing a camera's
    /// settings" section for the full explanation:
    ///
    /// - If `StructuralConfig` is unchanged from what the running
    ///   pipeline was built with, only the recording branch is
    ///   added/removed/reconfigured dynamically
    ///   (`CaptureSession::set_recording`) - active viewers are
    ///   completely undisturbed.
    /// - Otherwise, falls back to `restart_if_running` (a full rebuild,
    ///   disconnecting active viewers) - resolution, rotation, the
    ///   overlay, an RTSP URL edit, or motion detection turning on/off
    ///   all change what's actually being decoded/encoded, which a
    ///   branch add/remove can't express.
    pub async fn apply_settings(self: &Arc<Self>, camera: &Camera) -> Result<(), CaptureError> {
        let managed = {
            let map = self.cameras.read().await;
            map.get(&camera.id).cloned()
        };
        let Some(managed) = managed else {
            return Ok(());
        };

        if Self::structural_config(camera) != managed.structural {
            return self.restart_if_running(camera).await;
        }

        managed
            .session
            .set_recording(self.desired_recording(camera))
            .await
            .map_err(|err| {
                tracing::error!(camera = %camera.id, %err, "failed to apply recording change dynamically");
                err
            })?;

        // Recording turning off can also mean this camera no longer
        // needs to keep running with no viewers - mirror
        // `ViewerGuard::drop`'s "last reason to exist just went away"
        // teardown (LED off included) rather than leaving an idle
        // pipeline holding the device open for nothing.
        let new_keep_alive = Self::keeps_pipeline_alive(camera);
        managed.keep_alive.store(new_keep_alive, Ordering::SeqCst);
        if !new_keep_alive && managed.viewers.load(Ordering::SeqCst) == 0 {
            if let Some(cmd) = &managed.led_control.off_command {
                led::spawn_run(camera.id, "off", cmd.clone());
            }
            self.remove_if_current(camera.id, &managed).await;
        }
        Ok(())
    }

    /// Replaces `camera`'s running pipeline with a freshly built one
    /// reflecting `motion_active_now`, if a pipeline is currently
    /// running for it. No-op otherwise.
    ///
    /// The old pipeline is stopped *before* the new one starts, and given
    /// a brief moment to settle. This matters specifically for USB
    /// cameras: starting the replacement first would have its `v4l2src`
    /// race the still-open old one for the same `/dev/videoN`, which
    /// fails with "device busy" (this was caught by testing a real
    /// settings-change restart against real hardware, not reasoned out
    /// up front).
    async fn replace_pipeline(
        self: &Arc<Self>,
        camera: &Camera,
        motion_active_now: bool,
    ) -> Result<(), CaptureError> {
        let mut map = self.cameras.write().await;
        let Some(old) = map.get(&camera.id).cloned() else {
            return Ok(());
        };
        old.session.stop();
        let _ = old.superseded.send(true);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let new_managed = self.spawn_managed(camera, motion_active_now)?;
        map.insert(camera.id, new_managed);
        Ok(())
    }

    /// Stops a camera's pipeline unconditionally (used when a camera is
    /// deleted).
    pub async fn stop(&self, camera_id: Uuid) {
        if let Some(old) = self.cameras.write().await.remove(&camera_id) {
            let _ = old.superseded.send(true);
        }
    }

    async fn remove_if_current(&self, camera_id: Uuid, expected: &Arc<ManagedCamera>) {
        let mut map = self.cameras.write().await;
        let is_current = map.get(&camera_id).is_some_and(|c| Arc::ptr_eq(c, expected));
        if is_current {
            map.remove(&camera_id);
        }
    }
}

/// Evaluates a camera's recording schedule against the real current
/// local time - the one place `chrono::Local::now()` (a real clock read,
/// not available/meaningful in `omni-core`, which also compiles to wasm)
/// meets `RecordingSchedule::is_active_at`'s pure day/minute predicate.
pub(crate) fn schedule_is_active_now(schedule: &omni_core::RecordingSchedule) -> bool {
    let now = chrono::Local::now();
    let weekday_mon0 = now.weekday().num_days_from_monday() as u8;
    let minute_of_day = (now.hour() * 60 + now.minute()) as u16;
    schedule.is_active_at(weekday_mon0, minute_of_day)
}

/// Merges "the capture pipeline hit a bus error" and "this pipeline
/// instance was superseded/stopped" into one channel, so `omni-webrtc`
/// only has to watch a single `Option<String>` to know when to close a
/// viewer's peer connection - it doesn't need to know about supervisor
/// concerns like settings-change restarts.
fn merge_end_signals(
    mut capture_error: watch::Receiver<Option<String>>,
    mut superseded: watch::Receiver<bool>,
) -> watch::Receiver<Option<String>> {
    let (tx, rx) = watch::channel(None);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                res = capture_error.changed() => {
                    if res.is_err() {
                        break;
                    }
                    let message = capture_error.borrow().clone();
                    if let Some(message) = message {
                        let _ = tx.send(Some(message));
                        break;
                    }
                }
                res = superseded.changed() => {
                    if res.is_err() {
                        break;
                    }
                    if *superseded.borrow() {
                        let _ = tx.send(Some(
                            "camera settings changed; please reconnect".to_string(),
                        ));
                        break;
                    }
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb_camera() -> Camera {
        Camera::new_usb("test", "/dev/video0")
    }

    #[test]
    fn nothing_enabled_does_not_keep_the_pipeline_alive() {
        assert!(!Supervisor::keeps_pipeline_alive(&usb_camera()));
    }

    #[test]
    fn standalone_motion_detection_always_keeps_it_alive() {
        let mut camera = usb_camera();
        camera.motion.enabled = true;
        assert!(Supervisor::keeps_pipeline_alive(&camera));
    }

    /// Regression guard for exactly the v0.14 change: recording with no
    /// schedule restriction (the common case) must keep behaving like
    /// before - always alive while enabled, not accidentally gated by a
    /// schedule that was never turned on.
    #[test]
    fn recording_with_no_schedule_restriction_always_keeps_it_alive() {
        let mut camera = usb_camera();
        camera.recording.enabled = true;
        assert!(!camera.recording.schedule.enabled);
        assert!(Supervisor::keeps_pipeline_alive(&camera));
    }
}
