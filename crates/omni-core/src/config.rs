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
    pub fn db_path(&self) -> String {
        format!("{}/db/omnimonitor.sqlite", self.data_dir)
    }

    pub fn recordings_dir(&self) -> String {
        format!("{}/recordings", self.data_dir)
    }
}
