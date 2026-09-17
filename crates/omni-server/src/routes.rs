use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use omni_core::{validate, Camera, CameraKind, StreamCodec};

use crate::discovery::auto_discover_usb_cameras;
use crate::state::AppState;
use crate::ws::stream_ws_handler;

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/config", get(get_config))
        .route("/api/cameras", get(list_cameras).post(create_camera))
        .route("/api/cameras/discover", post(discover_cameras))
        .route("/api/cameras/:id", axum::routing::delete(delete_camera))
        .route("/api/stream/:camera_id", get(stream_ws_handler))
}

type ApiError = (StatusCode, String);

async fn get_config(State(state): State<Arc<AppState>>) -> Json<omni_core::AppConfig> {
    Json(state.config.clone())
}

async fn list_cameras(State(state): State<Arc<AppState>>) -> Json<Vec<Camera>> {
    Json(state.db.list_cameras().await.unwrap_or_default())
}

async fn discover_cameras(State(state): State<Arc<AppState>>) -> Json<Vec<Camera>> {
    auto_discover_usb_cameras(&state.db).await;
    Json(state.db.list_cameras().await.unwrap_or_default())
}

/// Only RTSP cameras can be added by hand through this endpoint - USB
/// cameras are picked up automatically by `/api/cameras/discover` since
/// their identity (the `/dev/videoN` path) comes from the OS, not the user.
#[derive(Deserialize)]
struct CreateCameraRequest {
    name: String,
    url: String,
}

async fn create_camera(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCameraRequest>,
) -> Result<Json<Camera>, ApiError> {
    validate::validate_camera_name(&req.name).map_err(bad_request)?;
    let url = req.url.trim();
    if url.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "rtsp url must not be empty".into()));
    }

    let camera = Camera {
        id: Uuid::new_v4(),
        name: req.name,
        kind: CameraKind::Rtsp { url: url.to_string() },
        enabled: true,
        width: 1280,
        height: 720,
        framerate: 30,
        codec: StreamCodec::Vp8,
        status: None,
    };

    state
        .db
        .upsert_camera(&camera)
        .await
        .map_err(internal_error)?;

    Ok(Json(camera))
}

async fn delete_camera(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.db.delete_camera(id).await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn bad_request<E: std::fmt::Display>(err: E) -> ApiError {
    (StatusCode::BAD_REQUEST, err.to_string())
}

fn internal_error<E: std::fmt::Display>(err: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
