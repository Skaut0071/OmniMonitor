use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use thiserror::Error;
use tokio::sync::{broadcast, watch};

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("gstreamer state change error: {0}")]
    StateChange(#[from] gst::StateChangeError),
    #[error("failed to build capture pipeline: {0}")]
    Build(String),
    #[error("io error preparing recording directory: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub keyframe: bool,
    pub duration: Duration,
}

/// Where a capture pipeline reads video from. A USB device is exclusive
/// (only one pipeline can hold `/dev/videoN` open at a time - see
/// `omni-server::supervisor`, which is why live viewers share one running
/// pipeline instead of each opening their own); an RTSP camera is a normal
/// network connection and doesn't have that constraint.
#[derive(Debug, Clone)]
pub enum CaptureSource {
    Usb { device_path: String },
    Rtsp { url: String },
}

impl CaptureSource {
    fn gst_bin_description(&self) -> String {
        match self {
            CaptureSource::Usb { device_path } => {
                format!("v4l2src device={device_path} io-mode=2")
            }
            // TCP transport is forced: it's the only reasonable default
            // for a camera reachable over a WiFi LAN or through a NAT/
            // firewall, where the UDP ports rtspsrc would otherwise pick
            // are very likely to get blocked or dropped.
            CaptureSource::Rtsp { url } => {
                format!("rtspsrc location=\"{url}\" latency=200 protocols=tcp")
            }
        }
    }
}

/// If set, the pipeline gains a second branch that writes fixed-length
/// `.webm` segment files to `dir` indefinitely (GStreamer `splitmuxsink`).
/// Retention (deleting old segments) is handled separately by
/// `omni-server::retention`, not by this crate.
///
/// For "only record while motion is detected"
/// (`omni_core::RecordingTrigger::Motion`), this branch is simply absent
/// or present depending on the *current* motion state at the moment a
/// pipeline is (re)built - `omni-server::supervisor` rebuilds the whole
/// pipeline on each motion start/stop, it doesn't gate a always-present
/// branch live. That was tried first (a GStreamer `valve` toggled from
/// the motion-detection callback) and abandoned after testing it
/// end-to-end: a `valve` that starts closed blocks the whole pipeline's
/// transition to PLAYING (nothing downstream of `decodebin` preroll's,
/// not just the recording branch - confirmed with a plain buffer-count
/// probe, not just "no errors"), and starting it open and closing it
/// shortly after avoids that but reliably left `splitmuxsink` stuck
/// on re-open (very likely a keyframe/timestamp discontinuity the muxer
/// doesn't recover from). Rebuilding the pipeline is slower per
/// transition but uses the same restart path already proven for settings
/// changes.
#[derive(Debug, Clone)]
pub struct RecordingSink {
    pub dir: PathBuf,
    pub segment_seconds: u32,
}

/// If set, the pipeline gains a low-resolution/low-framerate raw branch
/// used for simple frame-difference motion detection, entirely in Rust -
/// see `motion_diff_fraction`.
#[derive(Debug, Clone)]
pub struct MotionConfig {
    /// 1 (least sensitive) to 100 (most sensitive).
    pub sensitivity: u8,
    /// Seeds the detector's notion of "is motion currently active" at
    /// startup. Matters specifically for `omni-server::supervisor`
    /// rebuilding this pipeline in response to a motion transition
    /// (`RecordingTrigger::Motion`): without this, the fresh pipeline's
    /// detector always starts at "inactive", so if real motion is still
    /// ongoing it immediately "discovers" a false-to-true transition and
    /// asks for *another* rebuild - which does the same thing again, in
    /// a tight loop (caught by testing an end-to-end motion-triggered
    /// recording, not reasoned out up front - the loop was rebuilding
    /// the pipeline several times a second).
    pub initial_active: bool,
}

const MOTION_WIDTH: u32 = 160;
const MOTION_HEIGHT: u32 = 90;
const MOTION_FRAMERATE: u32 = 5;
/// Out of 255 grayscale levels - how much a pixel must change to count as
/// "changed" at all, before the fraction-of-frame-changed check.
const MOTION_PIXEL_THRESHOLD: u8 = 20;
/// Motion stays "active" for this long after the last detected change, so
/// a brief pause doesn't immediately end a motion-gated recording segment
/// or flap the reported motion state.
const MOTION_HOLD: Duration = Duration::from_secs(5);

fn motion_required_fraction(sensitivity: u8) -> f32 {
    let s = sensitivity.clamp(1, 100) as f32;
    // 100 -> 0.002 (a tiny change triggers it), 1 -> 0.06 (needs a lot).
    0.06 - (s - 1.0) / 99.0 * 0.058
}

fn motion_diff_fraction(prev: &[u8], cur: &[u8]) -> f32 {
    if prev.len() != cur.len() || cur.is_empty() {
        return 0.0;
    }
    let changed = prev
        .iter()
        .zip(cur.iter())
        .filter(|(a, b)| a.abs_diff(**b) > MOTION_PIXEL_THRESHOLD)
        .count();
    changed as f32 / cur.len() as f32
}

/// A running capture+encode GStreamer pipeline for one camera. Multiple
/// live viewers subscribe to the same broadcast channel rather than each
/// starting their own pipeline - required for USB devices (which only
/// allow one open handle) and just more efficient for RTSP ones too.
/// Dropping this stops the pipeline (best-effort: sets state to Null).
pub struct CaptureSession {
    pipeline: gst::Pipeline,
    frames_tx: broadcast::Sender<EncodedFrame>,
}

pub struct CaptureHandle {
    pub session: CaptureSession,
    /// Resolves with `Some(message)` if the pipeline hits an async
    /// GStreamer bus error (e.g. "device busy") or EOS. `set_state`
    /// returning `Ok` does *not* mean capture is actually working -
    /// V4L2/RTSP failures like this surface later on the bus, not as a
    /// synchronous error, so callers must watch this too.
    pub error: watch::Receiver<Option<String>>,
    /// `Some` iff `PipelineConfig::motion` was set: reports whether
    /// motion is currently considered active, updated in near-real-time
    /// as frames are analyzed.
    pub motion: Option<watch::Receiver<bool>>,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub source: CaptureSource,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    /// Target VP8 bitrate in bits/sec.
    pub bitrate: u32,
    pub recording: Option<RecordingSink>,
    pub motion: Option<MotionConfig>,
}

impl CaptureSession {
    /// Starts capturing from `config.source` and encoding to VP8.
    ///
    /// Pipeline: `{source} -> decodebin -> videoconvert/scale/rate ->
    /// tee(raw) -> vp8enc -> tee(encoded)`, with the encoded tee fanning
    /// out to an `appsink` (live viewers, via the broadcast channel) and,
    /// if `config.recording` is set, a `splitmuxsink` (segmented
    /// recording - present or absent in a given pipeline instance based
    /// on whether recording should currently be happening; see
    /// `RecordingSink`'s docs for why that's a whole-pipeline-rebuild
    /// decision rather than a live-toggled element within one instance).
    /// The raw tee optionally feeds a low-res/low-fps motion-detection
    /// branch. `decodebin` is what lets the same pipeline shape work
    /// unmodified across USB cameras (MJPG or raw YUYV) and RTSP cameras
    /// (H.264 or whatever the camera sends): it autodetects and inserts
    /// the right depayloader/parser/decoder chain.
    pub fn start(config: PipelineConfig) -> Result<CaptureHandle, CaptureError> {
        let mut description = format!(
            "{source} ! decodebin ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,width={width},height={height},framerate={fps}/1 \
             ! tee name=raw_tee \
             raw_tee. ! queue max-size-buffers=4 leaky=downstream \
                ! vp8enc deadline=1 target-bitrate={bitrate} cpu-used=4 keyframe-max-dist=60 end-usage=cbr \
                ! tee name=omni_tee \
             omni_tee. ! queue max-size-buffers=4 leaky=downstream \
                ! appsink name=omni_sink emit-signals=true sync=false max-buffers=2 drop=true",
            source = config.source.gst_bin_description(),
            width = config.width,
            height = config.height,
            fps = config.framerate,
            bitrate = config.bitrate,
        );

        if config.motion.is_some() {
            // The leading `videoconvert` here isn't a format conversion
            // (the raw_tee branch is already plain video/x-raw) - it's a
            // workaround for a real `gst_parse_launch` quirk found while
            // testing this pipeline shape end-to-end: with a `decodebin`
            // upstream (RTSP or MJPG-format USB source) and more than one
            // consumer of the post-decode raw video, `gst_parse_launch`'s
            // delayed-linking resolution for `decodebin`'s dynamic pad can
            // fail ("Delayed linking failed ... some pad of GstDecodeBin
            // to some pad of GstVideoConvert"), and the whole pipeline
            // dies with "streaming stopped, reason not-linked" right after
            // PLAYING. Verified empirically (not just reasoned about) that
            // giving the motion branch its own leading `videoconvert`
            // avoids it, against both an RTSP and a USB source.
            description.push_str(&format!(
                " raw_tee. ! queue max-size-buffers=2 leaky=downstream ! videoconvert ! videoscale ! videorate \
                  ! video/x-raw,format=GRAY8,width={mw},height={mh},framerate={mfps}/1 \
                  ! appsink name=motion_sink emit-signals=true sync=false max-buffers=1 drop=true",
                mw = MOTION_WIDTH,
                mh = MOTION_HEIGHT,
                mfps = MOTION_FRAMERATE,
            ));
        }

        if let Some(recording) = &config.recording {
            std::fs::create_dir_all(&recording.dir)?;
            // The run-start prefix is what keeps this safe across
            // restarts: splitmuxsink always counts segments from 0 within
            // one pipeline instance, and a settings change (or any other
            // reason to restart - see Supervisor::restart_if_running)
            // starts a fresh instance. Without a unique-per-run prefix,
            // that second instance's `seg00000.webm` would silently
            // overwrite the first instance's already-recorded footage.
            let run_started_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let location = recording.dir.join(format!("{run_started_at}-%05d.webm"));
            let max_size_time_ns = (recording.segment_seconds as u64) * 1_000_000_000;
            description.push_str(&format!(
                " omni_tee. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=0 \
                  ! splitmuxsink location=\"{location}\" max-size-time={max_size_time_ns} \
                    muxer-factory=matroskamux async-finalize=true",
                location = location.display(),
            ));
        }

        tracing::debug!(%description, "generated pipeline description");
        let element =
            gst::parse::launch(&description).map_err(|e| CaptureError::Build(e.to_string()))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| CaptureError::Build("parsed element graph is not a Pipeline".into()))?;

        let appsink = pipeline
            .by_name("omni_sink")
            .ok_or_else(|| CaptureError::Build("appsink 'omni_sink' not found".into()))?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| CaptureError::Build("omni_sink element is not an AppSink".into()))?;

        // Bounded mainly to cap memory if a subscriber falls badly behind;
        // a lagging subscriber gets `RecvError::Lagged` and just skips
        // ahead (see omni-webrtc's forward_frames), it doesn't block
        // everyone else.
        let (frames_tx, _) = broadcast::channel::<EncodedFrame>(32);
        let frames_tx_cb = frames_tx.clone();

        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                    let duration = buffer
                        .duration()
                        .map(|d| Duration::from_nanos(d.nseconds()))
                        .unwrap_or_default();
                    let frame = EncodedFrame {
                        data: Bytes::copy_from_slice(&map),
                        keyframe,
                        duration,
                    };
                    // No receivers (no live viewers currently connected,
                    // e.g. this pipeline only exists to record) is not an
                    // error - `send` just reports 0 receivers.
                    let _ = frames_tx_cb.send(frame);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let motion_rx = if let Some(motion_cfg) = &config.motion {
            let motion_sink = pipeline
                .by_name("motion_sink")
                .ok_or_else(|| CaptureError::Build("appsink 'motion_sink' not found".into()))?
                .downcast::<gst_app::AppSink>()
                .map_err(|_| CaptureError::Build("motion_sink element is not an AppSink".into()))?;

            let required_fraction = motion_required_fraction(motion_cfg.sensitivity);
            let (motion_tx, motion_rx) = watch::channel(motion_cfg.initial_active);
            let motion_state = Arc::new(Mutex::new(MotionDetectState {
                prev_frame: None,
                last_motion_at: motion_cfg.initial_active.then(Instant::now),
                active: motion_cfg.initial_active,
            }));

            motion_sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                        let mut state = motion_state.lock().unwrap();
                        let now = Instant::now();
                        if let Some(prev) = &state.prev_frame {
                            let frac = motion_diff_fraction(prev, &map);
                            tracing::debug!(frac, required_fraction, len = map.len(), "motion frame diff");
                            if frac > required_fraction {
                                state.last_motion_at = Some(now);
                            }
                        }
                        state.prev_frame = Some(map.to_vec());

                        let active_now = state
                            .last_motion_at
                            .map(|t| now.duration_since(t) < MOTION_HOLD)
                            .unwrap_or(false);
                        if active_now != state.active {
                            state.active = active_now;
                            let _ = motion_tx.send(active_now);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );

            Some(motion_rx)
        } else {
            None
        };

        pipeline.set_state(gst::State::Playing)?;

        let (error_tx, error_rx) = watch::channel(None);
        spawn_bus_watch(pipeline.clone(), error_tx);

        Ok(CaptureHandle {
            session: CaptureSession {
                pipeline,
                frames_tx,
            },
            error: error_rx,
            motion: motion_rx,
        })
    }

    /// A fresh view of every `EncodedFrame` produced from this moment
    /// onward. Multiple viewers of the same camera each get their own
    /// receiver over the one running pipeline.
    pub fn subscribe(&self) -> broadcast::Receiver<EncodedFrame> {
        self.frames_tx.subscribe()
    }

    /// Synchronously stops the pipeline, releasing any exclusive device
    /// (a USB camera can only be opened by one pipeline at a time - see
    /// `omni-server::supervisor`). Safe to call even while other `Arc`
    /// holders elsewhere still reference the surrounding `ManagedCamera`:
    /// there is exactly one underlying GStreamer pipeline regardless of
    /// how many Rust-side references point at it, so this affects all of
    /// them immediately rather than waiting for the last one to drop.
    /// Callers that need to *replace* a running pipeline (e.g. a settings
    /// change) must call this before starting the replacement, not after.
    /// Otherwise the new pipeline's `v4l2src`/`rtspsrc` races the old one
    /// for the same device and can fail with "device busy".
    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct MotionDetectState {
    prev_frame: Option<Vec<u8>>,
    last_motion_at: Option<Instant>,
    active: bool,
}

/// Polls the pipeline's bus for `Error`/`Eos` on a dedicated OS thread
/// (GStreamer delivers these asynchronously; nothing about `set_state`
/// or `appsink` callbacks alone will ever see a `v4l2src`/`rtspsrc`
/// failure like "device busy" or "connection refused"). Exits once the
/// pipeline is torn down (state -> Null).
fn spawn_bus_watch(pipeline: gst::Pipeline, error_tx: watch::Sender<Option<String>>) {
    std::thread::spawn(move || {
        let Some(bus) = pipeline.bus() else { return };
        loop {
            if pipeline.current_state() == gst::State::Null {
                return;
            }
            match bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(1),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            ) {
                Some(msg) => {
                    let text = match msg.view() {
                        gst::MessageView::Error(err) => {
                            format!("{} ({})", err.error(), err.debug().unwrap_or_default())
                        }
                        gst::MessageView::Eos(_) => "pipeline reached end of stream".to_string(),
                        _ => continue,
                    };
                    tracing::error!(%text, "gstreamer pipeline error");
                    let _ = error_tx.send(Some(text));
                    return;
                }
                None => continue,
            }
        }
    });
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_have_no_diff() {
        let frame = vec![100u8; 1000];
        assert_eq!(motion_diff_fraction(&frame, &frame), 0.0);
    }

    #[test]
    fn fully_changed_frame_has_full_diff() {
        let prev = vec![0u8; 1000];
        let cur = vec![255u8; 1000];
        assert_eq!(motion_diff_fraction(&prev, &cur), 1.0);
    }

    #[test]
    fn mismatched_sizes_are_treated_as_no_diff() {
        assert_eq!(motion_diff_fraction(&[1, 2, 3], &[1, 2]), 0.0);
    }

    #[test]
    fn higher_sensitivity_requires_smaller_fraction() {
        assert!(motion_required_fraction(100) < motion_required_fraction(1));
    }
}
