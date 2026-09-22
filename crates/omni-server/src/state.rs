use std::sync::Arc;

use omni_core::AppConfig;
use omni_db::Db;
use omni_webrtc::IceServersConfig;

use crate::auth::LoginRateLimiter;
use crate::rtsp::RtspServer;
use crate::supervisor::Supervisor;

pub struct AppState {
    pub db: Db,
    pub config: AppConfig,
    pub supervisor: Arc<Supervisor>,
    pub rtsp_server: Arc<RtspServer>,
    pub login_rate_limiter: Arc<LoginRateLimiter>,
    pub ice_servers: IceServersConfig,
}
