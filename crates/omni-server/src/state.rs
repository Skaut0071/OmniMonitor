use omni_core::AppConfig;
use omni_db::Db;

pub struct AppState {
    pub db: Db,
    pub config: AppConfig,
}
