mod auth;
mod discovery;
mod motion;
mod onvif_discovery;
mod reachability;
mod schedule;
mod retention;
mod routes;
mod rtsp;
mod state;
mod supervisor;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use omni_core::AppConfig;
use omni_db::Db;

use discovery::auto_discover_usb_cameras;
use rtsp::RtspServer;
use state::AppState;
use supervisor::Supervisor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    omni_capture::init()?;

    let config = AppConfig::from_env();
    std::fs::create_dir_all(config.recordings_dir())?;
    let db = Db::connect(&config.db_path()).await?;
    auth::bootstrap_admin(&db).await?;
    let (rtsp_username, rtsp_password) = auth::bootstrap_rtsp_credentials(&db).await?;

    auto_discover_usb_cameras(&db).await;

    let supervisor = Arc::new(Supervisor::new(PathBuf::from(&config.data_dir), db.clone()));

    // Cameras with recording and/or standalone motion detection enabled
    // get a persistent pipeline running from boot, independent of
    // whether anyone is watching live.
    for camera in db.list_cameras().await.unwrap_or_default() {
        if camera.recording.enabled || camera.motion.enabled {
            if let Err(err) = supervisor.ensure_running(&camera).await {
                tracing::error!(camera = %camera.id, %err, "failed to start pipeline at boot");
            } else {
                tracing::info!(camera = %camera.id, name = %camera.name, "pipeline started at boot");
            }
        }
    }

    tokio::spawn(retention::run(db.clone(), PathBuf::from(&config.data_dir)));
    tokio::spawn(auth::run_session_sweeper(db.clone()));
    tokio::spawn(schedule::run(Arc::clone(&supervisor), db.clone()));

    let login_rate_limiter = Arc::new(auth::LoginRateLimiter::new());
    tokio::spawn(auth::run_login_rate_limiter_sweeper(Arc::clone(
        &login_rate_limiter,
    )));

    let rtsp_server = RtspServer::start(config.rtsp_port, &rtsp_username, &rtsp_password);

    let state = Arc::new(AppState {
        db,
        config: config.clone(),
        supervisor,
        rtsp_server,
        login_rate_limiter,
    });

    // Every camera gets an RTSP mount point at /<camera-id>, regardless
    // of recording/motion settings - unlike the capture pipeline itself,
    // registering a mount point is cheap (it only starts a pipeline once
    // an RTSP client actually connects, via `Supervisor::acquire_viewer`
    // same as any other viewer).
    for camera in state.db.list_cameras().await.unwrap_or_default() {
        state.rtsp_server.add_camera(Arc::clone(&state), camera);
    }

    let frontend_dist = PathBuf::from(
        std::env::var("OMNI_FRONTEND_DIST").unwrap_or_else(|_| "frontend/dist".to_string()),
    );
    let index_html = frontend_dist.join("index.html");
    let static_service =
        ServeDir::new(&frontend_dist).not_found_service(ServeFile::new(index_html));

    // No CORS layer: the frontend is always served by this same process
    // (or, in dev, proxied to it by Vite - see frontend/vite.config.ts),
    // so every legitimate request is same-origin already. `CorsLayer::
    // permissive()` used to sit here doing nothing useful for that case
    // while needlessly telling *other* origins' browsers it's fine to
    // read responses from this cookie-authenticated API.
    let app = Router::new()
        .merge(routes::api_routes(Arc::clone(&state)))
        .fallback_service(static_service)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    tracing::info!(%addr, "OmniMonitor listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
