use serde::{Deserialize, Serialize};

fn default_http_port() -> u16 {
    8090
}

fn default_rtsp_port() -> u16 {
    5544
}

fn default_data_dir() -> String {
    "./data".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_rtsp_port")]
    pub rtsp_port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            http_port: default_http_port(),
            rtsp_port: default_rtsp_port(),
            data_dir: default_data_dir(),
        }
    }
}

impl AppConfig {
    /// Same defaults as `Default`, overridable via `OMNI_HTTP_PORT`,
    /// `OMNI_RTSP_PORT`, `OMNI_DATA_DIR` - needed for a systemd
    /// deployment (see `packaging/`), where the service doesn't get to
    /// pick its own working directory the way `./data` (the relative
    /// default) assumes a development checkout does.
    pub fn from_env() -> Self {
        let http_port = std::env::var("OMNI_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_http_port);
        let rtsp_port = std::env::var("OMNI_RTSP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_rtsp_port);
        let data_dir = std::env::var("OMNI_DATA_DIR").unwrap_or_else(|_| default_data_dir());
        Self {
            http_port,
            rtsp_port,
            data_dir,
        }
    }

    pub fn db_path(&self) -> String {
        format!("{}/db/omnimonitor.sqlite", self.data_dir)
    }

    pub fn recordings_dir(&self) -> String {
        format!("{}/recordings", self.data_dir)
    }
}
