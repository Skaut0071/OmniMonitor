use std::sync::Arc;

use omni_core::AppConfig;
use omni_db::Db;

use crate::supervisor::Supervisor;

pub struct AppState {
    pub db: Db,
    pub config: AppConfig,
    pub supervisor: Arc<Supervisor>,
}
