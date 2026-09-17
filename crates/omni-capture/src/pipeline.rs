use std::time::Duration;

use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("gstreamer state change error: {0}")]
    StateChange(#[from] gst::StateChangeError),
    #[error("failed to build capture pipeline: {0}")]
    Build(String),
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub keyframe: bool,
    pub duration: Duration,
}

/// A running capture+encode GStreamer pipeline for one camera device.
/// Dropping this stops the pipeline (best-effort: sets state to Null).
pub struct CaptureSession {
    pipeline: gst::Pipeline,
}

pub struct CaptureHandle {
    pub session: CaptureSession,
    pub frames: mpsc::Receiver<EncodedFrame>,
    /// Resolves with `Some(message)` if the pipeline hits an async
    /// GStreamer bus error (e.g. "device busy") or EOS. `set_state`
    /// returning `Ok` does *not* mean capture is actually working -
    /// V4L2 failures like this surface later on the bus, not as a
    /// synchronous error, so callers must watch this too.
    pub error: tokio::sync::watch::Receiver<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub device_path: String,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    /// Target VP8 bitrate in bits/sec.
    pub bitrate: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            device_path: "/dev/video0".to_string(),
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 2_000_000,
        }
    }
}

impl CaptureSession {
    /// Starts capturing from the given V4L2 device and encoding to VP8.
    ///
    /// Pipeline: `v4l2src -> decodebin -> videoconvert/scale/rate -> vp8enc
    /// -> appsink`. `decodebin` is what lets this work unmodified whether
    /// the camera's native format is MJPG or raw YUYV: it autodetects and
    /// inserts `jpegdec` only when needed.
    pub fn start(config: PipelineConfig) -> Result<CaptureHandle, CaptureError> {
        let description = format!(
            "v4l2src device={device} io-mode=2 ! decodebin ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,width={width},height={height},framerate={fps}/1 \
             ! vp8enc deadline=1 target-bitrate={bitrate} cpu-used=4 keyframe-max-dist=60 end-usage=cbr \
             ! appsink name=omni_sink emit-signals=true sync=false max-buffers=2 drop=true",
            device = config.device_path,
            width = config.width,
            height = config.height,
            fps = config.framerate,
            bitrate = config.bitrate,
        );

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

        let (tx, rx) = mpsc::channel::<EncodedFrame>(8);

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
                    // Best-effort: prefer dropping a frame over blocking the
                    // GStreamer streaming thread if the consumer is slow.
                    // TODO(perf): drop until the next keyframe instead of
                    // dropping individual deltas, to avoid transient VP8
                    // reference corruption on a full channel.
                    if tx.try_send(frame).is_err() {
                        tracing::trace!("dropping encoded frame: receiver full or closed");
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline.set_state(gst::State::Playing)?;

        let (error_tx, error_rx) = tokio::sync::watch::channel(None);
        spawn_bus_watch(pipeline.clone(), error_tx);

        Ok(CaptureHandle {
            session: CaptureSession { pipeline },
            frames: rx,
            error: error_rx,
        })
    }
}

/// Polls the pipeline's bus for `Error`/`Eos` on a dedicated OS thread
/// (GStreamer delivers these asynchronously; nothing about `set_state`
/// or `appsink` callbacks alone will ever see a `v4l2src` failure like
/// "device busy"). Exits once the pipeline is torn down (state -> Null).
fn spawn_bus_watch(pipeline: gst::Pipeline, error_tx: tokio::sync::watch::Sender<Option<String>>) {
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
