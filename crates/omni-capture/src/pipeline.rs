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
    /// Note this deliberately never interpolates a camera-supplied RTSP
    /// URL into the returned string: `gst::parse::launch` parses that
    /// string with gst-launch syntax, so a URL containing a `"` (or other
    /// launch-syntax metacharacters) would otherwise let a value that's
    /// only supposed to be a `location` property break out and append
    /// arbitrary pipeline elements (e.g. `filesink` to write files as the
    /// service user) - `validate_rtsp_url` only checks for an `rtsp://`
    /// prefix, not the absence of those characters, so this can't rely on
    /// validation alone. The RTSP source instead gets a bare, unparsed
    /// name here; `CaptureSession::start` looks the element up by that
    /// name after parsing and sets `location` as a typed GObject property
    /// (`set_property`, not string formatting), which goes straight to
    /// the property setter with no further parsing of its contents.
    fn gst_bin_description(&self) -> String {
        match self {
            CaptureSource::Usb { device_path } => {
                format!("v4l2src device={device_path} io-mode=2")
            }
            // TCP transport is forced: it's the only reasonable default
            // for a camera reachable over a WiFi LAN or through a NAT/
            // firewall, where the UDP ports rtspsrc would otherwise pick
            // are very likely to get blocked or dropped.
            CaptureSource::Rtsp { .. } => {
                "rtspsrc name=omni_rtsp_src latency=200 protocols=tcp".to_string()
            }
        }
    }
}

/// If set (initially, via `PipelineConfig`, or later via
/// `CaptureSession::set_recording`), the pipeline gains a second branch
/// that writes fixed-length `.webm` segment files to `dir` indefinitely
/// (GStreamer `splitmuxsink`). Retention (deleting old segments) is
/// handled separately by `omni-server::retention`/`motion_retention`,
/// not by this crate.
///
/// Turning this on/off after the pipeline is already running (a
/// recording toggle, or a schedule boundary - see
/// `omni-server::supervisor::apply_settings`) dynamically adds or
/// removes the branch from the live pipeline via GStreamer's `tee`
/// request-pad mechanism, instead of rebuilding the whole pipeline -
/// which is what makes it possible without disconnecting active live
/// viewers. This is a different mechanism from - and doesn't share the
/// failure mode of - an earlier attempt that gated an always-present
/// branch with a GStreamer `valve` toggled from the motion-detection
/// callback (see `docs/ARCHITECTURE.md`'s "Motion detection" section):
/// that approach reused *one* `splitmuxsink` instance across open/close
/// cycles and reliably got it stuck on reopen. `set_recording` instead
/// creates a brand new `queue`+`splitmuxsink` pair every time recording
/// starts and fully removes them (after finalizing the current segment
/// file with a real EOS) every time it stops - there's no element
/// being reused across a stop/start cycle for a stale keyframe/timestamp
/// discontinuity to get stuck in.
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
    /// The currently-attached recording branch, if any - see
    /// `set_recording`. `None` here does not necessarily mean recording
    /// was never configured; it also becomes `None` once a branch has
    /// been fully detached.
    recording: Mutex<Option<RecordingBranch>>,
}

/// The dynamically-added `tee` src pad + `queue`/`splitmuxsink` pair
/// backing an active recording branch - see `attach_recording`/
/// `detach_recording`. `dir`/`segment_seconds` are kept alongside so
/// `set_recording` can tell "recording is on and unchanged" (no-op) apart
/// from "recording is on but the target directory or segment length
/// changed" (detach the old branch, attach a fresh one) without having to
/// inspect the GStreamer elements themselves.
struct RecordingBranch {
    tee_pad: gst::Pad,
    queue: gst::Element,
    splitmuxsink: gst::Element,
    dir: PathBuf,
    segment_seconds: u32,
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
    pub rotation: omni_core::Rotation,
    /// Burns the current wall-clock date/time into the encoded output
    /// (live view, recordings, RTSP re-serve - everything downstream of
    /// `omni_tee`) via a `clockoverlay` element. Deliberately placed
    /// *after* `raw_tee`, not before it, so the motion-detection branch
    /// never sees it: an overlay redrawn once a second would register as
    /// constant "motion" to the consecutive-frame-diffing in
    /// `omni_capture::pipeline`'s motion detector.
    pub overlay_timestamp: bool,
}

/// `gst-plugins-good`'s `videoflip` `method` property for each rotation -
/// applied right after `decodebin`, before the scale to the configured
/// resolution, so the final output is always exactly the configured
/// width/height with the rotated content inside it (matching how a
/// camera physically mounted sideways/upside down should be corrected).
fn videoflip_method(rotation: omni_core::Rotation) -> &'static str {
    match rotation {
        // `videoflip`'s enum nick for "no rotation" is "none" - its
        // human-readable description happens to say "Identity (no
        // rotation)", which is what led to using the wrong nick
        // ("identity") here originally. That's a distinct, real GStreamer
        // element (a no-op passthrough), not a valid value for this
        // property, and `gst_parse_launch` rejects it: "could not set
        // property \"method\" in element \"videoflip\" to \"identity\"" -
        // confirmed against `gst-inspect-1.0 videoflip`'s actual enum
        // nicks, not just assumed a second time.
        omni_core::Rotation::None => "none",
        omni_core::Rotation::Clockwise90 => "clockwise",
        omni_core::Rotation::Rotate180 => "rotate-180",
        omni_core::Rotation::CounterClockwise90 => "counterclockwise",
    }
}

/// Dynamically adds a fresh `queue ! splitmuxsink` recording branch onto
/// `pipeline`'s `omni_tee`, bringing the new elements all the way up to
/// the pipeline's current state *before* linking them to the tee - so
/// the very first buffer the tee forwards to the new branch lands on an
/// element that's actually ready to receive it, not one still in
/// NULL/READY (which pushing into can fail). Used both for a pipeline's
/// initial recording branch (`CaptureSession::start`) and for a later
/// `set_recording` toggle - one codepath either way.
fn attach_recording(
    pipeline: &gst::Pipeline,
    sink: &RecordingSink,
) -> Result<RecordingBranch, CaptureError> {
    let omni_tee = pipeline
        .by_name("omni_tee")
        .ok_or_else(|| CaptureError::Build("tee 'omni_tee' not found".into()))?;

    std::fs::create_dir_all(&sink.dir)?;
    // The run-start prefix is what keeps this safe across repeated
    // attach/detach cycles on the same pipeline (as well as across a
    // full pipeline restart): splitmuxsink always counts segments from 0
    // for a given instance, and each attach creates a brand new
    // instance. Without a unique-per-attach prefix, a second recording
    // session's `seg00000.webm` would silently overwrite the first's
    // already-recorded footage.
    let run_started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let location = sink.dir.join(format!("{run_started_at}-%05d.webm"));
    let max_size_time_ns = (sink.segment_seconds as u64) * 1_000_000_000;

    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()
        .map_err(|e| CaptureError::Build(format!("failed to create recording queue: {e}")))?;
    let splitmuxsink = gst::ElementFactory::make("splitmuxsink")
        .property("location", location.to_string_lossy().as_ref())
        .property("max-size-time", max_size_time_ns)
        .property_from_str("muxer-factory", "matroskamux")
        .property("async-finalize", true)
        .build()
        .map_err(|e| CaptureError::Build(format!("failed to create splitmuxsink: {e}")))?;

    pipeline
        .add_many([&queue, &splitmuxsink])
        .map_err(|e| CaptureError::Build(format!("failed to add recording elements: {e}")))?;
    queue
        .link(&splitmuxsink)
        .map_err(|e| CaptureError::Build(format!("failed to link recording queue to splitmuxsink: {e}")))?;

    splitmuxsink
        .sync_state_with_parent()
        .map_err(|e| CaptureError::Build(format!("failed to start splitmuxsink: {e}")))?;
    queue
        .sync_state_with_parent()
        .map_err(|e| CaptureError::Build(format!("failed to start recording queue: {e}")))?;

    let tee_pad = omni_tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CaptureError::Build("failed to request a new tee pad for recording".into()))?;
    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| CaptureError::Build("recording queue has no sink pad".into()))?;
    tee_pad
        .link(&queue_sink)
        .map_err(|e| CaptureError::Build(format!("failed to link tee to recording branch: {e:?}")))?;

    Ok(RecordingBranch {
        tee_pad,
        queue,
        splitmuxsink,
        dir: sink.dir.clone(),
        segment_seconds: sink.segment_seconds,
    })
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
        // Inserted between raw_tee and the encoder only (see
        // `PipelineConfig::overlay_timestamp`'s docs for why not earlier),
        // so it's present in the encoded output but invisible to the
        // separate low-res motion-detection branch off the same tee.
        let overlay = if config.overlay_timestamp {
            "! clockoverlay time-format=\"%H:%M:%S %d/%m/%Y\" halignment=right valignment=bottom \
             shaded-background=true font-desc=\"Sans, 14\" "
        } else {
            ""
        };
        let mut description = format!(
            "{source} ! decodebin ! videoflip method={flip} ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,width={width},height={height},framerate={fps}/1 \
             ! tee name=raw_tee \
             raw_tee. ! queue max-size-buffers=4 leaky=downstream \
                {overlay}\
                ! vp8enc deadline=1 target-bitrate={bitrate} cpu-used=4 keyframe-max-dist=60 end-usage=cbr \
                ! tee name=omni_tee \
             omni_tee. ! queue max-size-buffers=4 leaky=downstream \
                ! appsink name=omni_sink emit-signals=true sync=false max-buffers=2 drop=true",
            source = config.source.gst_bin_description(),
            flip = videoflip_method(config.rotation),
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

        // The recording branch (if any) is deliberately *not* part of the
        // static parse-launch description - it's attached dynamically
        // below, right after the pipeline reaches PLAYING, via the exact
        // same `attach_recording` codepath `set_recording` uses for a
        // later toggle. One codepath for "recording starts now" whether
        // that's at initial pipeline construction or a later settings
        // change, rather than two that could drift apart.
        tracing::debug!(%description, "generated pipeline description");
        let element =
            gst::parse::launch(&description).map_err(|e| CaptureError::Build(e.to_string()))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| CaptureError::Build("parsed element graph is not a Pipeline".into()))?;

        // See `CaptureSource::gst_bin_description`'s docs: the URL is set
        // as a property here, never formatted into the launch string, so
        // it can't be interpreted as gst-launch syntax no matter what
        // characters it contains.
        if let CaptureSource::Rtsp { url } = &config.source {
            let rtspsrc = pipeline
                .by_name("omni_rtsp_src")
                .ok_or_else(|| CaptureError::Build("rtspsrc 'omni_rtsp_src' not found".into()))?;
            rtspsrc.set_property("location", url);
        }

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

        let recording = match &config.recording {
            Some(sink) => Some(attach_recording(&pipeline, sink)?),
            None => None,
        };

        let (error_tx, error_rx) = watch::channel(None);
        spawn_bus_watch(pipeline.clone(), error_tx);

        Ok(CaptureHandle {
            session: CaptureSession {
                pipeline,
                frames_tx,
                recording: Mutex::new(recording),
            },
            error: error_rx,
            motion: motion_rx,
        })
    }

    /// Dynamically adds or removes the recording branch on the live,
    /// already-PLAYING pipeline - via GStreamer `tee` request-pad
    /// add/remove, not a pipeline restart - so active live viewers (and
    /// any other running branch: motion detection, other recordings)
    /// are completely undisturbed. See `RecordingSink`'s docs for why
    /// this is safe in a way an earlier `valve`-based attempt wasn't.
    ///
    /// A no-op if `desired` already matches what's currently attached
    /// (same `Some`-ness, and same `dir`/`segment_seconds` if `Some`) -
    /// callers are free to call this on every settings change without
    /// checking first, e.g. `omni-server::supervisor::apply_settings`.
    pub async fn set_recording(&self, desired: Option<RecordingSink>) -> Result<(), CaptureError> {
        let (need_detach, need_attach) = {
            let current = self.recording.lock().unwrap();
            match (&*current, &desired) {
                (None, None) => (false, false),
                (Some(cur), Some(want))
                    if cur.dir == want.dir && cur.segment_seconds == want.segment_seconds =>
                {
                    (false, false)
                }
                (existing, wanted) => (existing.is_some(), wanted.is_some()),
            }
        };
        if !need_detach && !need_attach {
            return Ok(());
        }
        if need_detach {
            self.detach_recording().await?;
        }
        if need_attach {
            // `desired` is `Some` whenever `need_attach` is true, by the
            // match above.
            let sink = desired.expect("need_attach implies desired is Some");
            let branch = attach_recording(&self.pipeline, &sink)?;
            *self.recording.lock().unwrap() = Some(branch);
        }
        Ok(())
    }

    /// Removes the currently-attached recording branch, if any -
    /// finalizing its current segment file with a real EOS first (rather
    /// than just yanking the elements out, which would leave a truncated,
    /// possibly-unplayable `.webm`), then tearing the branch's elements
    /// down and off the pipeline.
    async fn detach_recording(&self) -> Result<(), CaptureError> {
        let Some(branch) = self.recording.lock().unwrap().take() else {
            return Ok(());
        };

        let queue_sink = branch
            .queue
            .static_pad("sink")
            .ok_or_else(|| CaptureError::Build("recording queue has no sink pad".into()))?;

        // Fires once the EOS we're about to inject has actually reached
        // the branch (passed the queue's sink pad) - our signal that it's
        // safe to tear the branch's elements down. Bounded by the
        // `tokio::time::timeout` below regardless, so a branch that
        // somehow never sees its own EOS (stuck muxer, etc.) can't hang
        // a settings change forever - it just risks a slightly-truncated
        // final segment file in that rare case, not a stuck server.
        let (eos_tx, eos_rx) = tokio::sync::oneshot::channel();
        let eos_tx = std::sync::Mutex::new(Some(eos_tx));
        let probe_queue_sink = queue_sink.clone();
        probe_queue_sink.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
            if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                if ev.type_() == gst::EventType::Eos {
                    if let Some(tx) = eos_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });

        // Block the tee's pad for this branch first (guarantees no more
        // real video buffers are in flight into it once this callback
        // runs), then inject EOS directly into the branch from inside
        // that same callback - after this, the branch only ever sees the
        // EOS it's about to finalize on, never another real buffer.
        let eos_queue_sink = queue_sink.clone();
        branch
            .tee_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_pad, _info| {
                let _ = eos_queue_sink.send_event(gst::event::Eos::new());
                gst::PadProbeReturn::Ok
            });

        let _ = tokio::time::timeout(Duration::from_secs(5), eos_rx).await;

        // Structural pipeline surgery (removing elements) must not
        // happen synchronously from within the probe callback above -
        // that callback runs on the pipeline's own streaming thread,
        // and blocking it on `Element::remove`/`set_state` here risks
        // deadlocking the pipeline. `call_async` runs this closure on a
        // GStreamer-owned worker thread instead, which is the documented
        // safe way to restructure a pipeline in response to a pad probe.
        let RecordingBranch {
            tee_pad,
            queue,
            splitmuxsink,
            ..
        } = branch;
        let pipeline = self.pipeline.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        pipeline.call_async(move |pipeline| {
            let _ = queue.set_state(gst::State::Null);
            let _ = splitmuxsink.set_state(gst::State::Null);
            let _ = pipeline.remove_many([&queue, &splitmuxsink]);
            if let Some(omni_tee) = pipeline.by_name("omni_tee") {
                omni_tee.release_request_pad(&tee_pad);
            }
            let _ = done_tx.send(());
        });
        let _ = done_rx.await;
        Ok(())
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

    /// Regression test for a real bug that shipped in v0.9:
    /// `Rotation::None` mapped to the `videoflip` nick `"identity"`,
    /// which isn't valid (the correct nick is `"none"` - `videoflip`'s
    /// own docs describe it as "Identity (no rotation)", which is what
    /// led to the wrong value). That broke every camera's pipeline by
    /// default (`Rotation::None` is the default), but the bug survived
    /// v0.9's own testing because nothing actually checked that a
    /// generated pipeline *description* was accepted by GStreamer, only
    /// that the expected string got built - see docs/ARCHITECTURE.md's
    /// "Dashboard UX" section for the full postmortem.
    ///
    /// Uses `videotestsrc` (no real camera needed) with the exact same
    /// `videoflip method={...}` fragment `gst_bin_description`/`start`
    /// would generate, and asserts `gst::parse::launch` - the same call
    /// `CaptureSession::start` makes - accepts it. This is the level a
    /// property-name typo like this actually gets caught at: parsing,
    /// not "does the resulting frame look rotated."
    #[test]
    fn every_rotation_is_a_valid_videoflip_method() {
        omni_gstreamer_test_init();
        for rotation in [
            omni_core::Rotation::None,
            omni_core::Rotation::Clockwise90,
            omni_core::Rotation::Rotate180,
            omni_core::Rotation::CounterClockwise90,
        ] {
            let method = videoflip_method(rotation);
            let description =
                format!("videotestsrc num-buffers=1 ! videoflip method={method} ! fakesink");
            gst::parse::launch(&description).unwrap_or_else(|e| {
                panic!("rotation {rotation:?} (videoflip method={method:?}) rejected by gst_parse_launch: {e}")
            });
        }
    }

    #[test]
    fn timestamp_overlay_fragment_is_valid() {
        omni_gstreamer_test_init();
        let description = "videotestsrc num-buffers=1 \
             ! clockoverlay time-format=\"%H:%M:%S %d/%m/%Y\" halignment=right valignment=bottom \
               shaded-background=true font-desc=\"Sans, 14\" \
             ! fakesink";
        gst::parse::launch(description)
            .unwrap_or_else(|e| panic!("clockoverlay fragment rejected by gst_parse_launch: {e}"));
    }

    fn omni_gstreamer_test_init() {
        // `gst::init()` is safe to call more than once (idempotent), so
        // every test in this module that needs it can just call this
        // rather than relying on test ordering or a shared harness.
        gst::init().expect("gstreamer init failed - is it installed?");
    }
}
