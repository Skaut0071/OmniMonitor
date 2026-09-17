//! Bridges an `omni_capture::CaptureSession`'s encoded VP8 frames to a
//! browser over WebRTC using `webrtc-rs`.
//!
//! GStreamer owns capture + encode (see `omni-capture`); this crate owns
//! nothing about V4L2 or pixels, only RTP/ICE/DTLS transport. Signaling
//! (the offer/answer exchange itself) happens over a plain WebSocket in
//! `omni-server` - this crate just turns an offer SDP into an answer SDP
//! and a running frame-forwarding task.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use omni_capture::{CaptureHandle, CaptureSession, EncodedFrame};
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

/// A live browser<->camera WebRTC session. Dropping this tears down the
/// peer connection, stops the frame-forwarding task, and (via owning the
/// `CaptureSession`) stops the GStreamer capture pipeline.
pub struct StreamSession {
    peer_connection: Arc<RTCPeerConnection>,
    _capture: CaptureSession,
    _forward_task: tokio::task::JoinHandle<()>,
    _rtcp_task: tokio::task::JoinHandle<()>,
    _error_watch_task: tokio::task::JoinHandle<()>,
}

impl StreamSession {
    /// Consumes a browser SDP offer and a running capture pipeline, and
    /// returns the session plus the SDP answer to send back.
    pub async fn start(offer_sdp: &str, capture: CaptureHandle) -> Result<(Self, String)> {
        let CaptureHandle {
            session: capture_session,
            frames,
            mut error,
        } = capture;

        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .context("registering default codecs")?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .context("registering default interceptors")?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let peer_connection = Arc::new(
            api.new_peer_connection(config)
                .await
                .context("creating peer connection")?,
        );

        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_VP8.to_owned(),
                ..Default::default()
            },
            "video".to_owned(),
            "omnimonitor".to_owned(),
        ));

        let rtp_sender = peer_connection
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .context("adding video track")?;

        // RTCP must be read even if we don't act on it, or the sender can
        // stall internally.
        let rtcp_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 1500];
            while rtp_sender.read(&mut buf).await.is_ok() {}
        });

        peer_connection.on_peer_connection_state_change(Box::new(
            move |state: RTCPeerConnectionState| {
                tracing::info!(?state, "peer connection state changed");
                Box::pin(async {})
            },
        ));

        let offer = RTCSessionDescription::offer(offer_sdp.to_owned())
            .context("parsing offer SDP")?;
        peer_connection
            .set_remote_description(offer)
            .await
            .context("setting remote description")?;

        let answer = peer_connection
            .create_answer(None)
            .await
            .context("creating answer")?;

        let mut gather_complete = peer_connection.gathering_complete_promise().await;
        peer_connection
            .set_local_description(answer)
            .await
            .context("setting local description")?;
        let _ = gather_complete.recv().await;

        let local_desc = peer_connection
            .local_description()
            .await
            .context("no local description after gathering")?;

        let forward_task = tokio::spawn(forward_frames(track, frames));

        // If the capture pipeline dies after negotiation (e.g. the V4L2
        // device turned out to be busy - GStreamer only reports that
        // asynchronously on its bus, well after `set_state` returned Ok),
        // close the peer connection so the browser sees the failure
        // instead of a connection that looks "connected" but never
        // carries any video.
        let error_watch_pc = Arc::clone(&peer_connection);
        let error_watch_task = tokio::spawn(async move {
            if error.changed().await.is_ok() {
                let message = error.borrow().clone();
                if let Some(message) = message {
                    tracing::warn!(%message, "closing peer connection: capture pipeline failed");
                    let _ = error_watch_pc.close().await;
                }
            }
        });

        Ok((
            Self {
                peer_connection,
                _capture: capture_session,
                _forward_task: forward_task,
                _rtcp_task: rtcp_task,
                _error_watch_task: error_watch_task,
            },
            local_desc.sdp,
        ))
    }

    pub async fn close(&self) -> Result<()> {
        self.peer_connection.close().await?;
        Ok(())
    }
}

async fn forward_frames(
    track: Arc<TrackLocalStaticSample>,
    mut frames: mpsc::Receiver<EncodedFrame>,
) {
    while let Some(frame) = frames.recv().await {
        let duration = if frame.duration.is_zero() {
            Duration::from_millis(33)
        } else {
            frame.duration
        };
        let sample = Sample {
            data: frame.data,
            duration,
            ..Default::default()
        };
        if let Err(err) = track.write_sample(&sample).await {
            tracing::debug!(%err, "stopping frame forwarding: track write failed");
            break;
        }
    }
}
