use std::path::PathBuf;
use std::time::Duration;

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
                format!(
                    "rtspsrc location=\"{url}\" latency=200 protocols=tcp"
                )
            }
        }
    }
}

/// If set, the pipeline gains a second branch that writes fixed-length
/// `.webm` segment files to `dir` indefinitely (GStreamer `splitmuxsink`).
/// Retention (deleting old segments) is handled separately by
/// `omni-server::retention`, not by this crate.
#[derive(Debug, Clone)]
pub struct RecordingSink {
    pub dir: PathBuf,
    pub segment_seconds: u32,
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
}

impl CaptureSession {
    /// Starts capturing from `config.source` and encoding to VP8.
    ///
    /// Pipeline: `{source} -> decodebin -> videoconvert/scale/rate ->
    /// vp8enc -> tee`, with `tee` fanning out to an `appsink` (live
    /// viewers, via the broadcast channel) and, if `config.recording` is
    /// set, a `splitmuxsink` (continuous segmented recording).
    /// `decodebin` is what lets the same pipeline shape work unmodified
    /// across USB cameras (MJPG or raw YUYV) and RTSP cameras (H.264 or
    /// whatever the camera sends): it autodetects and inserts the right
    /// depayloader/parser/decoder chain.
    pub fn start(config: PipelineConfig) -> Result<CaptureHandle, CaptureError> {
        let mut description = format!(
            "{source} ! decodebin ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,width={width},height={height},framerate={fps}/1 \
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
            let location = recording
                .dir
                .join(format!("{run_started_at}-%05d.webm"));
            let max_size_time_ns = (recording.segment_seconds as u64) * 1_000_000_000;
            description.push_str(&format!(
                " omni_tee. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=0 \
                  ! splitmuxsink location=\"{location}\" max-size-time={max_size_time_ns} \
                    muxer-factory=matroskamux async-finalize=true",
                location = location.display(),
            ));
        }

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

        pipeline.set_state(gst::State::Playing)?;

        let (error_tx, error_rx) = watch::channel(None);
        spawn_bus_watch(pipeline.clone(), error_tx);

        Ok(CaptureHandle {
            session: CaptureSession {
                pipeline,
                frames_tx,
            },
            error: error_rx,
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
                        gst::MessageView::Error(err) => format!(
                            "{} ({})",
                            err.error(),
                            err.debug().unwrap_or_default()
                        ),
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
