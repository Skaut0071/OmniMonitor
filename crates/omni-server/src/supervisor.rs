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
//! - A camera with recording (continuous) or standalone motion detection
//!   enabled gets a persistent pipeline, started at server boot and kept
//!   running regardless of viewers.
//! - A camera being watched but not recorded/detected gets an ephemeral
//!   pipeline: started on the first viewer, stopped when the last one
//!   disconnects.
//! - Changing a camera's settings (recording on/off, resolution, ...)
//!   restarts its pipeline immediately so the change actually takes
//!   effect - see `restart_if_running`. Active viewers of that camera are
//!   deliberately disconnected (with a clear error) rather than left
//!   silently frozen; the frontend already has a retry path for this.
//!   Applying settings changes without dropping active viewers is future
//!   work (see docs/ROADMAP.md) - it would need dynamic `tee` pad
//!   add/remove instead of a full pipeline restart.
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
    /// no viewers (continuous recording or standalone motion detection).
    /// For `RecordingTrigger::Motion`, this is true whenever recording is
    /// enabled at all - motion detection itself must keep running
    /// regardless of viewers so it can notice the *next* transition,
    /// even while currently not actively recording.
    keep_alive: AtomicBool,
    /// Cloned from `Camera::led_control` at spawn time - see the module
    /// docs' "LED ring control" section.
    led_control: omni_core::LedControl,
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

    fn pipeline_config(&self, camera: &Camera, motion_active_now: bool) -> PipelineConfig {
        let source = match &camera.kind {
            CameraKind::Usb { device_path } => CaptureSource::Usb {
                device_path: device_path.clone(),
            },
            CameraKind::Rtsp { url } => CaptureSource::Rtsp { url: url.clone() },
        };
        let need_motion =
            camera.motion.enabled || camera.recording.trigger == RecordingTrigger::Motion;
        // Deliberately *not* gated on `motion_active_now` for
        // `RecordingTrigger::Motion` - see the module docs' explanation of
        // why that used to rebuild the pipeline (and disconnect live
        // viewers) on every motion transition. The recording branch stays
        // present the whole time recording is enabled; `motion_retention`
        // prunes the resulting segments after the fact.
        let recording_now =
            camera.recording.enabled && schedule_is_active_now(&camera.recording.schedule);
        let segment_seconds = if camera.recording.trigger == RecordingTrigger::Motion {
            MOTION_SEGMENT_SECONDS
        } else {
            camera.recording.segment_seconds
        };

        let recording = recording_now.then(|| RecordingSink {
            dir: self.recordings_dir(camera.id),
            segment_seconds,
        });
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

    fn keeps_pipeline_alive(camera: &Camera) -> bool {
        camera.motion.enabled || camera.recording.enabled
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

    /// Restarts `camera`'s pipeline immediately if one is currently
    /// running, so a settings change (recording toggle, resolution, ...)
    /// actually takes effect right away. No-op if nothing is running for
    /// this camera - the next viewer/recording start will just pick up
    /// the new settings naturally. Preserves the current motion-active
    /// state (if any) across the restart, so an in-progress
    /// motion-triggered recording isn't interrupted by an unrelated
    /// settings tweak.
    pub async fn restart_if_running(self: &Arc<Self>, camera: &Camera) -> Result<(), CaptureError> {
        let motion_active = self.motion_active(camera.id).await;
        self.replace_pipeline(camera, motion_active).await
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
