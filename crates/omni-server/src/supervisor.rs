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
//! - A camera with recording enabled gets a persistent pipeline, started
//!   at server boot and kept running regardless of viewers.
//! - A camera being watched but not recorded gets an ephemeral pipeline:
//!   started on the first viewer, stopped when the last one disconnects.
//! - Changing a camera's settings (recording on/off, resolution, ...)
//!   restarts its pipeline immediately so the change actually takes
//!   effect - see `restart_if_running`. Active viewers of that camera are
//!   deliberately disconnected (with a clear error) rather than left
//!   silently frozen; the frontend already has a retry path for this.
//!   Applying settings changes without dropping active viewers is future
//!   work (see docs/ROADMAP.md) - it would need dynamic `tee` pad
//!   add/remove instead of a full pipeline restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use omni_capture::{
    CaptureError, CaptureHandle, CaptureSession, CaptureSource, EncodedFrame, PipelineConfig,
    RecordingSink,
};
use omni_core::{Camera, CameraKind};
use tokio::sync::{broadcast, watch, RwLock};
use uuid::Uuid;

const DEFAULT_BITRATE: u32 = 2_000_000;

struct ManagedCamera {
    session: CaptureSession,
    capture_error: watch::Receiver<Option<String>>,
    /// Sent `true` when this instance is replaced (settings restart) or
    /// explicitly stopped, so viewers holding a clone can tell the
    /// difference from a plain capture failure if they ever need to.
    superseded: watch::Sender<bool>,
    viewers: AtomicUsize,
    recording: AtomicBool,
}

pub struct Supervisor {
    data_dir: PathBuf,
    cameras: RwLock<HashMap<Uuid, Arc<ManagedCamera>>>,
}

/// Held by a live-view WebSocket handler for as long as it's watching a
/// camera. Dropping it releases this viewer's slot; if it was the last
/// one and the camera isn't being recorded, the pipeline is torn down
/// (e.g. releasing a USB device) shortly after.
pub struct ViewerGuard {
    camera_id: Uuid,
    managed: Arc<ManagedCamera>,
    supervisor: Arc<Supervisor>,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        let prev = self.managed.viewers.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 && !self.managed.recording.load(Ordering::SeqCst) {
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
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cameras: RwLock::new(HashMap::new()),
        }
    }

    pub fn recordings_dir(&self, camera_id: Uuid) -> PathBuf {
        self.data_dir.join("recordings").join(camera_id.to_string())
    }

    fn pipeline_config(&self, camera: &Camera) -> PipelineConfig {
        let source = match &camera.kind {
            CameraKind::Usb { device_path } => CaptureSource::Usb {
                device_path: device_path.clone(),
            },
            CameraKind::Rtsp { url } => CaptureSource::Rtsp { url: url.clone() },
        };
        let recording = camera.recording.enabled.then(|| RecordingSink {
            dir: self.recordings_dir(camera.id),
            segment_seconds: camera.recording.segment_seconds,
        });
        PipelineConfig {
            source,
            width: camera.width,
            height: camera.height,
            framerate: camera.framerate,
            bitrate: DEFAULT_BITRATE,
            recording,
        }
    }

    fn spawn_managed(&self, camera: &Camera) -> Result<Arc<ManagedCamera>, CaptureError> {
        let config = self.pipeline_config(camera);
        let CaptureHandle { session, error } = CaptureSession::start(config)?;
        let (superseded_tx, _) = watch::channel(false);
        Ok(Arc::new(ManagedCamera {
            session,
            capture_error: error,
            superseded: superseded_tx,
            viewers: AtomicUsize::new(0),
            recording: AtomicBool::new(camera.recording.enabled),
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
            let managed = self.spawn_managed(camera)?;
            map.insert(camera.id, Arc::clone(&managed));
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

    /// Starts a persistent pipeline for a recording-enabled camera if one
    /// isn't already running. Called at boot for every such camera, and
    /// whenever recording is turned on via the API.
    pub async fn ensure_running(&self, camera: &Camera) -> Result<(), CaptureError> {
        let mut map = self.cameras.write().await;
        if let std::collections::hash_map::Entry::Vacant(entry) = map.entry(camera.id) {
            let managed = self.spawn_managed(camera)?;
            entry.insert(managed);
        }
        Ok(())
    }

    /// Restarts `camera`'s pipeline immediately if one is currently
    /// running, so a settings change (recording toggle, resolution, ...)
    /// actually takes effect right away. No-op if nothing is running for
    /// this camera - the next viewer/recording start will just pick up
    /// the new settings naturally.
    ///
    /// The old pipeline is stopped *before* the new one starts, and given
    /// a brief moment to settle. This matters specifically for USB
    /// cameras: starting the replacement first would have its `v4l2src`
    /// race the still-open old one for the same `/dev/videoN`, which
    /// fails with "device busy" (this was caught by testing a real
    /// settings-change restart against real hardware, not reasoned out
    /// up front).
    pub async fn restart_if_running(&self, camera: &Camera) -> Result<(), CaptureError> {
        let mut map = self.cameras.write().await;
        let Some(old) = map.get(&camera.id).cloned() else {
            return Ok(());
        };
        old.session.stop();
        let _ = old.superseded.send(true);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let new_managed = self.spawn_managed(camera)?;
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
