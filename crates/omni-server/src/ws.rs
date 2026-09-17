use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use omni_webrtc::{RTCIceCandidateInit, StreamSession};

use crate::state::AppState;
use crate::supervisor::ViewerHandle;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Offer { sdp: String },
    IceCandidate { candidate: RTCIceCandidateInit },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    Answer { sdp: String },
    IceCandidate { candidate: RTCIceCandidateInit },
    Error { message: String },
}

/// Signaling + lifecycle endpoint for one camera's live preview.
///
/// Protocol (trickle ICE, since v0.6): the browser opens the socket,
/// sends `{"type":"offer","sdp":...}` as soon as it has one (not waiting
/// for its own ICE gathering to finish), and the server starts (or
/// joins) that camera's shared capture pipeline via `Supervisor` and a
/// WebRTC peer connection, replying with `{"type":"answer","sdp":...}`
/// as soon as the local description is set - also without waiting for
/// gathering. Both sides then trickle `{"type":"ice_candidate",...}`
/// messages as candidates are discovered, in both directions, for the
/// life of the socket. The viewer's slot on the shared pipeline, and the
/// peer connection, both live for exactly as long as this socket stays
/// open; if the underlying pipeline fails or is superseded by a settings
/// change, the server sends `{"type":"error",...}` and closes the socket
/// itself.
pub async fn stream_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<Uuid>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, camera_id))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>, camera_id: Uuid) {
    let camera = match state.db.get_camera(camera_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            let _ = send_error(&mut socket, "camera not found").await;
            return;
        }
        Err(err) => {
            let _ = send_error(&mut socket, &format!("db error: {err}")).await;
            return;
        }
    };

    // The browser sends its offer as soon as it has one (trickle ICE -
    // no waiting for gathering), but its own candidates can start
    // trickling in a moment later, potentially before the session below
    // exists to receive them - queue any that arrive first.
    let mut early_candidates = Vec::new();
    let offer_sdp = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(ClientMsg::Offer { sdp }) => break sdp,
                Ok(ClientMsg::IceCandidate { candidate }) => {
                    early_candidates.push(candidate);
                    continue;
                }
                Err(err) => {
                    let _ = send_error(&mut socket, &format!("invalid message: {err}")).await;
                    return;
                }
            },
            Some(Ok(_)) => continue,
            _ => return,
        }
    };

    let ViewerHandle {
        frames,
        mut ended,
        _guard,
    } = match state.supervisor.acquire_viewer(&camera).await {
        Ok(handle) => handle,
        Err(err) => {
            let _ = send_error(&mut socket, &format!("failed to start capture: {err}")).await;
            return;
        }
    };

    let (session, answer_sdp, mut local_candidates) =
        match StreamSession::start(&offer_sdp, frames, ended.clone()).await {
            Ok(triple) => triple,
            Err(err) => {
                let _ = send_error(
                    &mut socket,
                    &format!("failed to start webrtc session: {err}"),
                )
                .await;
                return;
            }
        };

    for candidate in early_candidates {
        if let Err(err) = session.add_ice_candidate(candidate).await {
            tracing::debug!(%err, "failed to add early trickled ICE candidate");
        }
    }

    let answer = ServerMsg::Answer { sdp: answer_sdp };
    if socket
        .send(Message::Text(serde_json::to_string(&answer).unwrap()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMsg>(&text) {
                            Ok(ClientMsg::IceCandidate { candidate }) => {
                                if let Err(err) = session.add_ice_candidate(candidate).await {
                                    tracing::debug!(%err, "failed to add trickled ICE candidate");
                                }
                            }
                            Ok(ClientMsg::Offer { .. }) => {
                                // Renegotiation isn't supported - one
                                // offer per socket, ignore anything else.
                            }
                            Err(err) => {
                                tracing::debug!(%err, "ignoring unparseable signaling message");
                            }
                        }
                    }
                    Some(Ok(_)) => continue,
                    _ => break,
                }
            }
            candidate = local_candidates.recv() => {
                let Some(candidate) = candidate else {
                    // Gathering finished - nothing more to trickle, but
                    // the socket itself stays open for the life of the
                    // viewer.
                    continue;
                };
                let msg = ServerMsg::IceCandidate { candidate };
                if socket.send(Message::Text(serde_json::to_string(&msg).unwrap())).await.is_err() {
                    break;
                }
            }
            changed = ended.changed() => {
                if changed.is_err() {
                    break;
                }
                let message = ended.borrow().clone();
                if let Some(message) = message {
                    let _ = send_error(&mut socket, &message).await;
                }
                break;
            }
        }
    }

    let _ = session.close().await;
    // `_guard` drops here, releasing this viewer's slot on the shared
    // capture pipeline.
}

async fn send_error(socket: &mut WebSocket, message: &str) -> Result<(), axum::Error> {
    let msg = ServerMsg::Error {
        message: message.to_string(),
    };
    socket
        .send(Message::Text(serde_json::to_string(&msg).unwrap()))
        .await
}
