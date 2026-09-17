use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use omni_capture::{CaptureSession, PipelineConfig};
use omni_core::CameraKind;
use omni_webrtc::StreamSession;

use crate::state::AppState;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Offer { sdp: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    Answer { sdp: String },
    Error { message: String },
}

/// Signaling + lifecycle endpoint for one camera's live preview.
///
/// Protocol (non-trickle ICE, kept deliberately simple for v0.1): the
/// browser opens the socket, sends `{"type":"offer","sdp":...}`, and the
/// server starts a capture pipeline + WebRTC peer connection and replies
/// with `{"type":"answer","sdp":...}` once ICE gathering is complete. The
/// capture pipeline and peer connection both live for exactly as long as
/// this socket stays open.
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

    let device_path = match &camera.kind {
        CameraKind::Usb { device_path } => device_path.clone(),
        CameraKind::Rtsp { .. } => {
            let _ = send_error(&mut socket, "RTSP camera streaming not implemented yet").await;
            return;
        }
    };

    let offer_sdp = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(ClientMsg::Offer { sdp }) => break sdp,
                Err(err) => {
                    let _ = send_error(&mut socket, &format!("invalid message: {err}")).await;
                    return;
                }
            },
            Some(Ok(_)) => continue,
            _ => return,
        }
    };

    let pipeline_config = PipelineConfig {
        device_path,
        width: camera.width,
        height: camera.height,
        framerate: camera.framerate,
        bitrate: 2_000_000,
    };

    let capture = match CaptureSession::start(pipeline_config) {
        Ok(handle) => handle,
        Err(err) => {
            let _ = send_error(&mut socket, &format!("failed to start capture: {err}")).await;
            return;
        }
    };

    let (session, answer_sdp) = match StreamSession::start(&offer_sdp, capture).await {
        Ok(pair) => pair,
        Err(err) => {
            let _ = send_error(
                &mut socket,
                &format!("failed to start webrtc session: {err}"),
            )
            .await;
            return;
        }
    };

    let answer = ServerMsg::Answer { sdp: answer_sdp };
    if socket
        .send(Message::Text(serde_json::to_string(&answer).unwrap()))
        .await
        .is_err()
    {
        return;
    }

    while let Some(msg) = socket.recv().await {
        if msg.is_err() {
            break;
        }
    }
    let _ = session.close().await;
}

async fn send_error(socket: &mut WebSocket, message: &str) -> Result<(), axum::Error> {
    let msg = ServerMsg::Error {
        message: message.to_string(),
    };
    socket
        .send(Message::Text(serde_json::to_string(&msg).unwrap()))
        .await
}
