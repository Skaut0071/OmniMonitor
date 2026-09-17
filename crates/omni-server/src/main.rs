mod discovery;
mod retention;
mod routes;
mod state;
mod supervisor;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use omni_core::AppConfig;
use omni_db::Db;

use discovery::auto_discover_usb_cameras;
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

    let config = AppConfig::default();
    std::fs::create_dir_all(config.recordings_dir())?;
    let db = Db::connect(&config.db_path()).await?;

    auto_discover_usb_cameras(&db).await;

    let supervisor = Arc::new(Supervisor::new(PathBuf::from(&config.data_dir)));

    // Cameras with recording enabled get a persistent pipeline running
    // from boot, independent of whether anyone is watching live.
    for camera in db.list_cameras().await.unwrap_or_default() {
        if camera.recording.enabled {
            if let Err(err) = supervisor.ensure_running(&camera).await {
                tracing::error!(camera = %camera.id, %err, "failed to start recording pipeline at boot");
            } else {
                tracing::info!(camera = %camera.id, name = %camera.name, "recording pipeline started");
            }
        }
    }

    tokio::spawn(retention::run(db.clone(), PathBuf::from(&config.data_dir)));

    let state = Arc::new(AppState {
        db,
        config: config.clone(),
        supervisor,
    });

    let frontend_dist = PathBuf::from(
        std::env::var("OMNI_FRONTEND_DIST").unwrap_or_else(|_| "frontend/dist".to_string()),
    );
    let index_html = frontend_dist.join("index.html");
    let static_service =
        ServeDir::new(&frontend_dist).not_found_service(ServeFile::new(index_html));

    let app = Router::new()
        .merge(routes::api_routes())
        .fallback_service(static_service)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    tracing::info!(%addr, "OmniMonitor listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
