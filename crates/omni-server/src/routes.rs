use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use omni_core::{
    validate, Camera, CameraKind, MotionEvent, MotionSettings, RecordingSettings,
    RecordingTrigger, StreamCodec,
};

use crate::auth;
use crate::discovery::auto_discover_usb_cameras;
use crate::state::AppState;
use crate::ws::stream_ws_handler;

/// `/api/auth/login` is the only endpoint reachable without a session;
/// everything else requires one - applied via `route_layer` below, which
/// (unlike `.layer`) only wraps routes added *before* it in this router,
/// not the whole `Router` it later gets merged into.
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let protected = Router::new()
        .route("/api/config", get(get_config))
        .route("/api/cameras", get(list_cameras).post(create_camera))
        .route("/api/cameras/discover", post(discover_cameras))
        .route(
            "/api/cameras/:id",
            axum::routing::patch(update_camera).delete(delete_camera),
        )
        .route("/api/cameras/:id/recordings", get(list_recordings))
        .route(
            "/api/recordings/:id/:filename",
            get(get_recording).delete(delete_recording),
        )
        .route("/api/cameras/:id/motion", get(get_motion_status))
        .route("/api/cameras/:id/events", get(list_events))
        .route("/api/stream/:camera_id", get(stream_ws_handler))
        .route("/api/auth/me", get(auth_me))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/change-password", post(auth_change_password))
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            auth::require_auth,
        ));

    Router::new()
        .route("/api/auth/login", post(auth_login))
        .merge(protected)
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let Some((username, hash)) = state.db.admin_user().await.map_err(internal_error)? else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "no admin account configured".to_string()));
    };
    if req.username != username || !auth::verify_password(&req.password, &hash) {
        return Err((StatusCode::UNAUTHORIZED, "invalid username or password".to_string()));
    }
    let token = auth::generate_token();
    state
        .db
        .create_session(&token, auth::session_expiry())
        .await
        .map_err(internal_error)?;
    let (name, value) = auth::set_cookie_header(&token);
    Ok((StatusCode::NO_CONTENT, [(name, value)]).into_response())
}

async fn auth_logout(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> Result<Response, ApiError> {
    if let Some(cookie) = req.headers().get(axum::http::header::COOKIE) {
        if let Ok(cookie) = cookie.to_str() {
            for part in cookie.split(';') {
                if let Some(token) = part.trim().strip_prefix(&format!("{}=", auth::SESSION_COOKIE)) {
                    let _ = state.db.delete_session(token).await;
                }
            }
        }
    }
    let (name, value) = auth::clear_cookie_header();
    Ok((StatusCode::NO_CONTENT, [(name, value)]).into_response())
}

#[derive(Serialize)]
struct MeResponse {
    username: String,
}

async fn auth_me(State(state): State<Arc<AppState>>) -> Result<Json<MeResponse>, ApiError> {
    let (username, _) = state
        .db
        .admin_user()
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no admin account configured".to_string()))?;
    Ok(Json(MeResponse { username }))
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

async fn auth_change_password(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    validate::validate_password(&req.new_password).map_err(bad_request)?;
    let (username, hash) = state
        .db
        .admin_user()
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no admin account configured".to_string()))?;
    if !auth::verify_password(&req.current_password, &hash) {
        return Err((StatusCode::UNAUTHORIZED, "current password is incorrect".to_string()));
    }
    state
        .db
        .set_admin_user(&username, &auth::hash_password(&req.new_password))
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
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
    validate::validate_rtsp_url(&req.url).map_err(bad_request)?;

    let camera = Camera {
        id: Uuid::new_v4(),
        name: req.name,
        kind: CameraKind::Rtsp {
            url: req.url.trim().to_string(),
        },
        enabled: true,
        width: 1280,
        height: 720,
        framerate: 30,
        codec: StreamCodec::Vp8,
        recording: RecordingSettings::default(),
        motion: MotionSettings::default(),
        status: None,
    };

    state
        .db
        .upsert_camera(&camera)
        .await
        .map_err(internal_error)?;

    Ok(Json(camera))
}

#[derive(Deserialize)]
struct UpdateRecordingRequest {
    enabled: bool,
    #[serde(default)]
    trigger: RecordingTrigger,
    segment_seconds: u32,
    retention_max_age_secs: Option<u64>,
    retention_max_size_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct UpdateMotionRequest {
    enabled: bool,
    sensitivity: u8,
    webhook_url: Option<String>,
}

/// All fields optional (PATCH semantics): only provided fields are
/// changed. If anything is running for this camera when it's updated,
/// its pipeline is restarted immediately so the change takes effect -
/// see `Supervisor::restart_if_running`.
#[derive(Deserialize)]
struct UpdateCameraRequest {
    name: Option<String>,
    /// RTSP cameras only - a USB camera's device path is OS-assigned and
    /// can't be edited.
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    framerate: Option<u32>,
    recording: Option<UpdateRecordingRequest>,
    motion: Option<UpdateMotionRequest>,
}

async fn update_camera(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCameraRequest>,
) -> Result<Json<Camera>, ApiError> {
    let mut camera = state
        .db
        .get_camera(id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "camera not found".to_string()))?;

    if let Some(name) = req.name {
        validate::validate_camera_name(&name).map_err(bad_request)?;
        camera.name = name;
    }

    if let Some(url) = req.url {
        validate::validate_rtsp_url(&url).map_err(bad_request)?;
        match &mut camera.kind {
            CameraKind::Rtsp { url: existing } => *existing = url.trim().to_string(),
            CameraKind::Usb { .. } => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "cannot set a URL on a USB camera".to_string(),
                ))
            }
        }
    }

    if req.width.is_some() || req.height.is_some() {
        let width = req.width.unwrap_or(camera.width);
        let height = req.height.unwrap_or(camera.height);
        validate::validate_resolution(width, height).map_err(bad_request)?;
        camera.width = width;
        camera.height = height;
    }

    if let Some(fps) = req.framerate {
        validate::validate_framerate(fps).map_err(bad_request)?;
        camera.framerate = fps;
    }

    if let Some(rec) = req.recording {
        validate::validate_segment_seconds(rec.segment_seconds).map_err(bad_request)?;
        validate::validate_retention(
            rec.enabled,
            rec.retention_max_age_secs,
            rec.retention_max_size_bytes,
        )
        .map_err(bad_request)?;
        camera.recording = RecordingSettings {
            enabled: rec.enabled,
            trigger: rec.trigger,
            segment_seconds: rec.segment_seconds,
            retention_max_age_secs: rec.retention_max_age_secs,
            retention_max_size_bytes: rec.retention_max_size_bytes,
        };
    }

    if let Some(motion) = req.motion {
        validate::validate_sensitivity(motion.sensitivity).map_err(bad_request)?;
        if let Some(url) = &motion.webhook_url {
            if !url.trim().is_empty() {
                validate::validate_webhook_url(url).map_err(bad_request)?;
            }
        }
        camera.motion = MotionSettings {
            enabled: motion.enabled,
            sensitivity: motion.sensitivity,
            webhook_url: motion
                .webhook_url
                .filter(|u| !u.trim().is_empty())
                .map(|u| u.trim().to_string()),
        };
    }

    state
        .db
        .upsert_camera(&camera)
        .await
        .map_err(internal_error)?;

    state
        .supervisor
        .restart_if_running(&camera)
        .await
        .map_err(internal_error)?;
    if camera.recording.enabled || camera.motion.enabled {
        state
            .supervisor
            .ensure_running(&camera)
            .await
            .map_err(internal_error)?;
    }

    Ok(Json(camera))
}

#[derive(Serialize)]
struct MotionStatus {
    active: bool,
}

async fn get_motion_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Json<MotionStatus> {
    Json(MotionStatus {
        active: state.supervisor.motion_active(id).await,
    })
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<MotionEvent>>, ApiError> {
    let events = state
        .db
        .list_motion_events(id, 100)
        .await
        .map_err(internal_error)?;
    Ok(Json(events))
}

async fn delete_camera(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.supervisor.stop(id).await;
    state.db.delete_camera(id).await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct RecordingInfo {
    filename: String,
    size_bytes: u64,
    modified: String,
}

async fn list_recordings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RecordingInfo>>, ApiError> {
    let dir = state.supervisor.recordings_dir(id);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Json(Vec::new())),
        Err(e) => return Err(internal_error(e)),
    };

    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(internal_error)? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("webm") {
            continue;
        }
        let meta = entry.metadata().await.map_err(internal_error)?;
        let modified: chrono::DateTime<chrono::Utc> =
            meta.modified().map_err(internal_error)?.into();
        out.push(RecordingInfo {
            filename: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            size_bytes: meta.len(),
            modified: modified.to_rfc3339(),
        });
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(Json(out))
}

async fn get_recording(
    State(state): State<Arc<AppState>>,
    Path((id, filename)): Path<(Uuid, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let safe_name = sanitize_filename(&filename)
        .ok_or((StatusCode::BAD_REQUEST, "invalid filename".to_string()))?;
    let path = state.supervisor.recordings_dir(id).join(safe_name);
    if !path.is_file() {
        return Err((StatusCode::NOT_FOUND, "recording not found".to_string()));
    }
    // ServeFile gives us correct Content-Type/Range/conditional-GET
    // handling for free, which is what lets a `<video>` element seek
    // within a segment instead of only playing start-to-end.
    let response = ServeFile::new(&path).oneshot(request).await.unwrap();
    Ok(response.into_response())
}

async fn delete_recording(
    State(state): State<Arc<AppState>>,
    Path((id, filename)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let safe_name = sanitize_filename(&filename)
        .ok_or((StatusCode::BAD_REQUEST, "invalid filename".to_string()))?;
    let path = state.supervisor.recordings_dir(id).join(safe_name);
    tokio::fs::remove_file(&path)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Recording filenames are only ever generated by `splitmuxsink`
/// (`seg%05d.webm`), never taken verbatim from a user - but this endpoint
/// still builds a path from a URL segment, so it must reject anything
/// that could escape the camera's recordings directory.
fn sanitize_filename(name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return None;
    }
    Some(name.to_string())
}

fn bad_request<E: std::fmt::Display>(err: E) -> ApiError {
    (StatusCode::BAD_REQUEST, err.to_string())
}

fn internal_error<E: std::fmt::Display>(err: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
