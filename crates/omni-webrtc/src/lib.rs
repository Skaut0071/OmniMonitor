//! Bridges a camera's encoded VP8 frames (from `omni_capture::CaptureSession`)
//! to a browser over WebRTC using `webrtc-rs`.
//!
//! GStreamer owns capture + encode (see `omni-capture`); this crate owns
//! nothing about V4L2/RTSP or pixels, only RTP/ICE/DTLS transport. It also
//! doesn't own the capture pipeline itself - that's shared across every
//! viewer of a camera and lives in `omni-server`'s supervisor, so this
//! crate just consumes a `broadcast::Receiver<EncodedFrame>` (one per
//! viewer) plus a shared pipeline-health watch. Signaling (the
//! offer/answer exchange) happens over a plain WebSocket in `omni-server` -
//! this crate turns an offer SDP into an answer SDP and a running
//! frame-forwarding task.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use omni_capture::EncodedFrame;
use tokio::sync::{broadcast, mpsc, watch};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
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

/// Re-exported so `omni-server::ws` can speak the wire format for a
/// trickled ICE candidate without depending on the `webrtc` crate
/// directly - this crate is the only one that needs to.
pub use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;

/// Which STUN/TURN servers a peer connection offers ICE, read once at
/// startup and reused for every viewer rather than re-reading env vars
/// per connection.
///
/// STUN alone (the only thing configured before this) only helps two
/// peers discover their own public address for a *direct* UDP path - it
/// does nothing when the actual network in between (a restrictive VPN,
/// some corporate firewalls, symmetric NAT) blocks or mangles UDP
/// entirely, which shows up as a connection stuck at "connecting" or
/// stuck "live" with no picture (the data path negotiated, but no media
/// packets are actually getting through). TURN is the standard fix: a
/// relay server the client falls back to when a direct path isn't
/// possible, which - depending on how the TURN server itself is
/// configured - can also be reached over TCP/TLS rather than UDP,
/// getting through networks that block UDP outright. There's no TURN
/// server bundled or auto-configured here: running one (e.g. `coturn`)
/// is real infrastructure with its own bandwidth cost (all relayed media
/// flows through it), left to whoever's deploying this to set up and
/// point at, not something to spin up unasked.
#[derive(Debug, Clone)]
pub struct IceServersConfig {
    stun_url: String,
    turn: Option<TurnConfig>,
}

#[derive(Debug, Clone)]
struct TurnConfig {
    url: String,
    username: String,
    password: String,
}

impl IceServersConfig {
    /// `OMNI_STUN_URL` overrides the default public Google STUN server.
    /// `OMNI_TURN_URL` (plus `OMNI_TURN_USERNAME`/`OMNI_TURN_PASSWORD`)
    /// adds a TURN server as a fallback ICE candidate - all three must be
    /// set together, or TURN is left out entirely (a TURN server with no
    /// credentials configured is a relay open to whoever finds it).
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var("OMNI_STUN_URL").ok(),
            std::env::var("OMNI_TURN_URL").ok(),
            std::env::var("OMNI_TURN_USERNAME").ok(),
            std::env::var("OMNI_TURN_PASSWORD").ok(),
        )
    }

    /// Pure logic behind `from_env`, factored out so it's testable
    /// without mutating real process environment variables (which,
    /// shared across a whole test binary run concurrently, is a classic
    /// source of flaky tests).
    fn from_values(
        stun_url: Option<String>,
        turn_url: Option<String>,
        turn_username: Option<String>,
        turn_password: Option<String>,
    ) -> Self {
        let stun_url = stun_url
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "stun:stun.l.google.com:19302".to_string());

        let turn_url = turn_url.filter(|v| !v.is_empty());
        let turn_username = turn_username.filter(|v| !v.is_empty());
        let turn_password = turn_password.filter(|v| !v.is_empty());
        let turn = match (turn_url, turn_username, turn_password) {
            (Some(url), Some(username), Some(password)) => Some(TurnConfig { url, username, password }),
            (None, None, None) => None,
            _ => {
                tracing::warn!(
                    "OMNI_TURN_URL/OMNI_TURN_USERNAME/OMNI_TURN_PASSWORD must all be set together - \
                     ignoring incomplete TURN configuration"
                );
                None
            }
        };

        Self { stun_url, turn }
    }

    fn to_ice_servers(&self) -> Vec<RTCIceServer> {
        let mut servers = vec![RTCIceServer {
            urls: vec![self.stun_url.clone()],
            ..Default::default()
        }];
        if let Some(turn) = &self.turn {
            servers.push(RTCIceServer {
                urls: vec![turn.url.clone()],
                username: turn.username.clone(),
                credential: turn.password.clone(),
                ..Default::default()
            });
        }
        servers
    }
}

/// A live browser<->camera WebRTC session for one viewer. Dropping this
/// tears down the peer connection and stops the frame-forwarding task.
/// The underlying capture pipeline is *not* owned here - see
/// `omni-server::supervisor::ViewerGuard` for that lifecycle.
pub struct StreamSession {
    peer_connection: Arc<RTCPeerConnection>,
    _forward_task: tokio::task::JoinHandle<()>,
    _rtcp_task: tokio::task::JoinHandle<()>,
    _error_watch_task: tokio::task::JoinHandle<()>,
}

impl StreamSession {
    /// Consumes a browser SDP offer plus a live view onto a running
    /// camera pipeline (frames + pipeline-health), and returns the
    /// session, the SDP answer to send back, and a channel of this
    /// server's own ICE candidates as they're discovered (trickle ICE -
    /// the answer is sent back as soon as the local description is set,
    /// not after gathering completes, so the caller should start
    /// forwarding candidates from this channel to the browser
    /// immediately rather than waiting for it to close).
    pub async fn start(
        offer_sdp: &str,
        frames: broadcast::Receiver<EncodedFrame>,
        mut error: watch::Receiver<Option<String>>,
        ice_servers: &IceServersConfig,
    ) -> Result<(Self, String, mpsc::UnboundedReceiver<RTCIceCandidateInit>)> {
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
            ice_servers: ice_servers.to_ice_servers(),
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

        // Trickle ICE: forward each locally discovered candidate to the
        // caller as soon as it's found, instead of making the browser
        // wait for `on_ice_candidate(None)` (gathering complete) before
        // it gets an answer at all.
        let (candidate_tx, candidate_rx) = mpsc::unbounded_channel::<RTCIceCandidateInit>();
        peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let candidate_tx = candidate_tx.clone();
            Box::pin(async move {
                let Some(candidate) = candidate else {
                    // Gathering finished - nothing to forward, and the
                    // channel closing is enough signal for the receiver.
                    return;
                };
                match candidate.to_json() {
                    Ok(init) => {
                        let _ = candidate_tx.send(init);
                    }
                    Err(err) => tracing::warn!(%err, "failed to serialize local ICE candidate"),
                }
            })
        }));

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

        peer_connection
            .set_local_description(answer)
            .await
            .context("setting local description")?;

        let local_desc = peer_connection
            .local_description()
            .await
            .context("no local description after set_local_description")?;

        let forward_task = tokio::spawn(forward_frames(track, frames));

        // If the shared capture pipeline dies (e.g. the V4L2 device turned
        // out to be busy, or the RTSP camera dropped the connection -
        // GStreamer only reports that asynchronously on its bus, well
        // after `set_state` returned Ok), close this viewer's peer
        // connection so the browser sees the failure instead of a
        // connection that looks "connected" but never carries any video.
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
                _forward_task: forward_task,
                _rtcp_task: rtcp_task,
                _error_watch_task: error_watch_task,
            },
            local_desc.sdp,
            candidate_rx,
        ))
    }

    /// Feeds a trickled ICE candidate from the browser in. Safe to call
    /// before or after the connection reaches a connected state -
    /// `webrtc-rs` queues candidates that arrive before the remote
    /// description would otherwise make them usable.
    pub async fn add_ice_candidate(&self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.peer_connection
            .add_ice_candidate(candidate)
            .await
            .context("adding remote ICE candidate")
    }

    pub async fn close(&self) -> Result<()> {
        self.peer_connection.close().await?;
        Ok(())
    }
}

async fn forward_frames(
    track: Arc<TrackLocalStaticSample>,
    mut frames: broadcast::Receiver<EncodedFrame>,
) {
    loop {
        let frame = match frames.recv().await {
            Ok(frame) => frame,
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                // This viewer's consumer (write_sample -> WebRTC send)
                // fell behind the shared pipeline; the oldest frames it
                // missed are gone. Just resume from the newest one - the
                // stream self-heals at the next VP8 keyframe.
                tracing::debug!(skipped, "viewer lagged behind live frame stream");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_google_stun_with_no_turn() {
        let cfg = IceServersConfig::from_values(None, None, None, None);
        let servers = cfg.to_ice_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:stun.l.google.com:19302"]);
    }

    #[test]
    fn empty_stun_url_falls_back_to_default() {
        let cfg = IceServersConfig::from_values(Some(String::new()), None, None, None);
        assert_eq!(cfg.to_ice_servers()[0].urls, vec!["stun:stun.l.google.com:19302"]);
    }

    #[test]
    fn custom_stun_url_is_used() {
        let cfg = IceServersConfig::from_values(Some("stun:my-stun:3478".to_string()), None, None, None);
        assert_eq!(cfg.to_ice_servers()[0].urls, vec!["stun:my-stun:3478"]);
    }

    #[test]
    fn complete_turn_config_is_added_as_second_server() {
        let cfg = IceServersConfig::from_values(
            None,
            Some("turn:my-turn:3478".to_string()),
            Some("user".to_string()),
            Some("pass".to_string()),
        );
        let servers = cfg.to_ice_servers();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1].urls, vec!["turn:my-turn:3478"]);
        assert_eq!(servers[1].username, "user");
        assert_eq!(servers[1].credential, "pass");
    }

    #[test]
    fn incomplete_turn_config_is_dropped_not_partially_applied() {
        // Missing password - this must never send a TURN server with an
        // empty credential to the browser, which would just be a relay
        // with an empty password rather than "TURN not configured".
        let cfg = IceServersConfig::from_values(
            None,
            Some("turn:my-turn:3478".to_string()),
            Some("user".to_string()),
            None,
        );
        assert_eq!(cfg.to_ice_servers().len(), 1);
    }
}
