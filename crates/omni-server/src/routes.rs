use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use omni_core::{
    validate, Camera, CameraKind, MotionEvent, MotionSettings, RecordingSettings,
    RecordingTrigger, Rotation, StreamCodec,
};

use crate::auth;
use crate::discovery::auto_discover_usb_cameras;
use crate::onvif_discovery::{self, DiscoveredDevice};
use crate::reachability;
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
        .route("/api/cameras/status", get(cameras_status))
        .route("/api/cameras/discover", post(discover_cameras))
        .route("/api/cameras/reorder", axum::routing::put(reorder_cameras))
        .route(
            "/api/camera-groups/rename",
            post(rename_camera_group),
        )
        .route("/api/onvif/discover", post(discover_onvif))
        .route("/api/ignored-usb-devices", get(list_ignored_usb_devices))
        .route(
            "/api/ignored-usb-devices/unignore",
            post(unignore_usb_device),
        )
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
        .route("/api/rtsp-credentials", get(get_rtsp_credentials))
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let ip = peer.ip();
    if let Err(retry_after_secs) = state.login_rate_limiter.check(ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("too many failed login attempts - try again in {retry_after_secs}s"),
        ));
    }

    let Some((username, hash)) = state.db.admin_user().await.map_err(internal_error)? else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "no admin account configured".to_string()));
    };
    if req.username != username || !auth::verify_password_async(req.password.clone(), hash).await {
        state.login_rate_limiter.record_failure(ip);
        return Err((StatusCode::UNAUTHORIZED, "invalid username or password".to_string()));
    }
    state.login_rate_limiter.record_success(ip);

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
    headers: HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    validate::validate_password(&req.new_password).map_err(bad_request)?;
    let (username, hash) = state
        .db
        .admin_user()
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no admin account configured".to_string()))?;
    if !auth::verify_password_async(req.current_password.clone(), hash).await {
        return Err((StatusCode::UNAUTHORIZED, "current password is incorrect".to_string()));
    }
    let new_hash = auth::hash_password_async(req.new_password.clone()).await;
    state
        .db
        .set_admin_user(&username, &new_hash)
        .await
        .map_err(internal_error)?;

    // A password change is usually prompted by "this password might be
    // compromised" - leaving every other already-issued session (good
    // for 30 days) valid afterwards would defeat the point. Keep only
    // the session making this request, so the user isn't logged out by
    // their own change.
    if let Some(current_token) = auth::session_token_from_headers(&headers) {
        if let Err(err) = state.db.delete_sessions_except(&current_token).await {
            tracing::warn!(%err, "failed to revoke other sessions after password change");
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

type ApiError = (StatusCode, String);

async fn get_config(State(state): State<Arc<AppState>>) -> Json<omni_core::AppConfig> {
    Json(state.config.clone())
}

#[derive(Serialize)]
struct RtspCredentialsResponse {
    username: String,
    password: String,
    port: u16,
}

/// Surfaces the RTSP Basic-auth credential the server bootstrapped (or
/// was given via `OMNI_RTSP_PASSWORD`) so it's discoverable from the UI
/// without digging through logs - see `auth::bootstrap_rtsp_credentials`.
/// Gated behind the same session auth as everything else in `protected`.
async fn get_rtsp_credentials(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RtspCredentialsResponse>, ApiError> {
    let (username, password) = state
        .db
        .rtsp_credentials()
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "no RTSP credential configured".to_string()))?;
    Ok(Json(RtspCredentialsResponse {
        username,
        password,
        port: state.config.rtsp_port,
    }))
}

async fn list_cameras(State(state): State<Arc<AppState>>) -> Json<Vec<Camera>> {
    Json(state.db.list_cameras().await.unwrap_or_default())
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum StatusOverview {
    /// A pipeline is running for this camera and producing frames.
    Streaming,
    /// A pipeline is running but hit an error.
    Error,
    /// No pipeline running, but the device/host answered a cheap
    /// presence check - see `reachability::probe_reachable`.
    Online,
    /// No pipeline running and the presence check failed (USB unplugged,
    /// RTSP host unreachable).
    Offline,
}

#[derive(Serialize)]
struct CameraStatusInfo {
    id: Uuid,
    name: String,
    kind: &'static str,
    status: StatusOverview,
}

/// A no-video overview of every camera's reachability - USB vs network
/// (`kind`) and online/offline/streaming/error (`status`) - without
/// opening a live view for each one. For a camera with an active
/// pipeline this is exact (`Supervisor::pipeline_status`); for an idle
/// one it's a best-effort presence check (`reachability::probe_reachable`)
/// run fresh on every call, so this endpoint is a little slower than a
/// plain camera list (a couple of seconds if several RTSP cameras are
/// unreachable and each has to time out) - acceptable for a status page
/// that's opened occasionally, not polled tightly.
async fn cameras_status(State(state): State<Arc<AppState>>) -> Json<Vec<CameraStatusInfo>> {
    let cameras = state.db.list_cameras().await.unwrap_or_default();
    let mut out = Vec::with_capacity(cameras.len());
    for camera in cameras {
        let kind = match camera.kind {
            CameraKind::Usb { .. } => "usb",
            CameraKind::Rtsp { .. } => "rtsp",
        };
        let status = match state.supervisor.pipeline_status(camera.id).await {
            Some(omni_core::CameraStatus::Error) => StatusOverview::Error,
            Some(_) => StatusOverview::Streaming,
            None => {
                if reachability::probe_reachable(&camera).await {
                    StatusOverview::Online
                } else {
                    StatusOverview::Offline
                }
            }
        };
        out.push(CameraStatusInfo {
            id: camera.id,
            name: camera.name,
            kind,
            status,
        });
    }
    Json(out)
}

async fn discover_cameras(State(state): State<Arc<AppState>>) -> Json<Vec<Camera>> {
    auto_discover_usb_cameras(&state.db).await;
    let cameras = state.db.list_cameras().await.unwrap_or_default();
    // Idempotent (replaces any existing mount point for a camera), so
    // just re-registering everyone here is simpler than tracking which
    // ones were actually new.
    for camera in &cameras {
        state.rtsp_server.add_camera(Arc::clone(&state), camera.clone());
    }
    Json(cameras)
}

#[derive(Deserialize)]
struct ReorderCamerasRequest {
    /// Every camera's id, in the new display order. Rejected (400) if it
    /// doesn't contain exactly the same set of ids as the ones that
    /// currently exist - a partial list would leave the left-out cameras
    /// with a stale `sort_order` relative to ones that did get reordered,
    /// silently corrupting the intended order rather than failing loudly.
    ids: Vec<Uuid>,
}

/// Drag-and-drop reordering in the dashboard: the frontend sends the
/// full new order after a drop, and every camera's `sort_order` is
/// reassigned to match it in one go (`Db::reorder_cameras`).
async fn reorder_cameras(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReorderCamerasRequest>,
) -> Result<StatusCode, ApiError> {
    let existing = state.db.list_cameras().await.map_err(internal_error)?;
    let mut existing_ids: Vec<Uuid> = existing.iter().map(|c| c.id).collect();
    let mut requested_ids = req.ids.clone();
    existing_ids.sort();
    requested_ids.sort();
    if existing_ids != requested_ids {
        return Err((
            StatusCode::BAD_REQUEST,
            "ids must be exactly the current set of camera ids".to_string(),
        ));
    }

    state
        .db
        .reorder_cameras(&req.ids)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RenameCameraGroupRequest {
    old_name: String,
    new_name: String,
}

/// Renames a group tab across every camera that has it - there's no
/// separate `groups` table with its own id/row to rename instead (see
/// `omni_core::Camera::group`'s docs), so this is a bulk find-and-replace.
async fn rename_camera_group(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenameCameraGroupRequest>,
) -> Result<StatusCode, ApiError> {
    let new_name = req.new_name.trim();
    validate::validate_group_name(new_name).map_err(bad_request)?;
    if new_name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "new group name must not be empty - clear a camera's group individually instead"
                .to_string(),
        ));
    }
    state
        .db
        .rename_camera_group(req.old_name.trim(), new_name)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Probes the LAN for ONVIF network cameras (WS-Discovery multicast) and
/// returns whatever answers within a few seconds - see
/// `onvif_discovery` for why this stops at "here's an IP and device
/// service URL" rather than resolving an actual RTSP URI.
async fn discover_onvif() -> Result<Json<Vec<DiscoveredDevice>>, ApiError> {
    let devices = onvif_discovery::discover(std::time::Duration::from_secs(3))
        .await
        .map_err(internal_error)?;
    Ok(Json(devices))
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

    let sort_order = state.db.next_sort_order().await.map_err(internal_error)?;
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
        rotation: Rotation::default(),
        sort_order,
        group: None,
        status: None,
    };

    state
        .db
        .upsert_camera(&camera)
        .await
        .map_err(internal_error)?;
    state
        .rtsp_server
        .add_camera(Arc::clone(&state), camera.clone());

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
    rotation: Option<Rotation>,
    /// `Some("")` (or whitespace-only) clears the group; `None` leaves it
    /// unchanged - same convention as `motion.webhook_url`.
    group: Option<String>,
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

    if let Some(rotation) = req.rotation {
        camera.rotation = rotation;
    }

    if let Some(group) = req.group {
        let trimmed = group.trim();
        validate::validate_group_name(trimmed).map_err(bad_request)?;
        camera.group = (!trimmed.is_empty()).then(|| trimmed.to_string());
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

    // The RTSP server's per-camera mount point captures its own snapshot
    // of the `Camera` at registration time (its `media-configure`
    // callback only builds a fresh pipeline from it on that mount's
    // *first* viewer since the last (re)registration - see
    // `RtspServer::add_camera`'s docs) - without re-registering here,
    // any settings change made through this endpoint would be invisible
    // to RTSP clients until the next full `/api/cameras/discover` happened
    // to re-register every mount point anyway.
    state.rtsp_server.add_camera(Arc::clone(&state), camera.clone());

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

/// Deleting a USB camera also permanently excludes its device path from
/// future auto-discovery (see `Db::ignore_usb_device`'s docs) - without
/// this, the still-plugged-in device would just get silently re-added on
/// the next rescan/restart, making "delete" not actually stick for USB
/// cameras specifically. Reversible from "Ignored USB devices" in the
/// sidebar. RTSP cameras don't need this: they're never auto-discovered
/// in the first place, so deleting one is already permanent.
async fn delete_camera(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let camera = state.db.get_camera(id).await.map_err(internal_error)?;
    state.supervisor.stop(id).await;
    state.rtsp_server.remove_camera(id);
    state.db.delete_camera(id).await.map_err(internal_error)?;
    if let Some(Camera {
        kind: CameraKind::Usb { device_path },
        ..
    }) = camera
    {
        state
            .db
            .ignore_usb_device(&device_path)
            .await
            .map_err(internal_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct IgnoredUsbDevice {
    device_path: String,
}

async fn list_ignored_usb_devices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<IgnoredUsbDevice>>, ApiError> {
    let devices = state
        .db
        .list_ignored_usb_devices()
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|device_path| IgnoredUsbDevice { device_path })
        .collect();
    Ok(Json(devices))
}

#[derive(Deserialize)]
struct UnignoreUsbDeviceRequest {
    device_path: String,
}

/// Reverses deleting a USB camera - the device becomes eligible for
/// auto-discovery again on the next rescan (it doesn't reappear
/// immediately on its own; call `POST /api/cameras/discover` afterward,
/// which the sidebar's "Rescan USB cameras" button already does).
async fn unignore_usb_device(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnignoreUsbDeviceRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .db
        .unignore_usb_device(&req.device_path)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct RecordingInfo {
    filename: String,
    size_bytes: u64,
    modified: String,
    /// Approximate - `splitmuxsink` doesn't record each segment's exact
    /// start time anywhere, only the file's own mtime (~when it finished
    /// being written, i.e. roughly `modified`). Computed as
    /// `modified - segment_seconds`, which is exactly right for a
    /// full-length segment and off by however much the *current*
    /// in-progress segment or a just-restarted pipeline's first segment
    /// falls short of the configured length. Good enough for placing
    /// segments on a timeline; see `docs/ARCHITECTURE.md` for why exact
    /// per-segment timestamps would need real pipeline instrumentation.
    started_at: String,
}

async fn list_recordings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RecordingInfo>>, ApiError> {
    let segment_seconds = state
        .db
        .get_camera(id)
        .await
        .map_err(internal_error)?
        .map(|c| c.recording.segment_seconds)
        .unwrap_or(300);

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
        let started_at = modified - chrono::Duration::seconds(segment_seconds as i64);
        out.push(RecordingInfo {
            filename: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            size_bytes: meta.len(),
            modified: modified.to_rfc3339(),
            started_at: started_at.to_rfc3339(),
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

/// Unlike `bad_request` (whose messages are `ValidationError`s meant to
/// be shown to the user), this wraps failures that are never the
/// client's fault - DB errors, IO errors - which can carry internal
/// detail (file paths, driver-specific error text) that has no business
/// being sent back over the API. Logs the real error server-side and
/// returns a generic message instead.
fn internal_error<E: std::fmt::Display>(err: E) -> ApiError {
    tracing::error!(%err, "internal error handling API request");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal server error".to_string(),
    )
}
