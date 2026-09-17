//! SQLite-backed storage for camera configuration.
//!
//! SQLite is intentionally chosen over a server database for a
//! single-box NVR: zero ops, file-based (easy to back up alongside
//! recordings), and more than fast enough for the write volume here
//! (camera config changes and event metadata, not frame data).

use anyhow::Result;
use omni_core::{Camera, CameraKind, RecordingSettings, StreamCodec};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::{FromRow, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

#[derive(FromRow)]
struct CameraRow {
    id: String,
    name: String,
    kind: String,
    device_path: Option<String>,
    rtsp_url: Option<String>,
    enabled: bool,
    width: i64,
    height: i64,
    framerate: i64,
    codec: String,
    recording_enabled: bool,
    segment_seconds: i64,
    retention_max_age_secs: Option<i64>,
    retention_max_size_bytes: Option<i64>,
}

impl TryFrom<CameraRow> for Camera {
    type Error = anyhow::Error;

    fn try_from(row: CameraRow) -> Result<Self> {
        let kind = match row.kind.as_str() {
            "usb" => CameraKind::Usb {
                device_path: row
                    .device_path
                    .ok_or_else(|| anyhow::anyhow!("usb camera row missing device_path"))?,
            },
            "rtsp" => CameraKind::Rtsp {
                url: row
                    .rtsp_url
                    .ok_or_else(|| anyhow::anyhow!("rtsp camera row missing rtsp_url"))?,
            },
            other => anyhow::bail!("unknown camera kind in db: {other}"),
        };
        let codec = match row.codec.as_str() {
            "vp8" => StreamCodec::Vp8,
            "h264" => StreamCodec::H264,
            other => anyhow::bail!("unknown codec in db: {other}"),
        };
        Ok(Camera {
            id: Uuid::parse_str(&row.id)?,
            name: row.name,
            kind,
            enabled: row.enabled,
            width: row.width as u32,
            height: row.height as u32,
            framerate: row.framerate as u32,
            codec,
            recording: RecordingSettings {
                enabled: row.recording_enabled,
                segment_seconds: row.segment_seconds as u32,
                retention_max_age_secs: row.retention_max_age_secs.map(|v| v as u64),
                retention_max_size_bytes: row.retention_max_size_bytes.map(|v| v as u64),
            },
            status: None,
        })
    }
}

impl Db {
    pub async fn connect(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("sqlite://{path}?mode=rwc");
        let pool = SqlitePoolOptions::new().max_connections(5).connect(&url).await?;
        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cameras (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                kind         TEXT NOT NULL,
                device_path  TEXT,
                rtsp_url     TEXT,
                enabled      INTEGER NOT NULL DEFAULT 1,
                width        INTEGER NOT NULL,
                height       INTEGER NOT NULL,
                framerate    INTEGER NOT NULL,
                codec        TEXT NOT NULL,
                created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Added in v0.2 (recording support). SQLite has no "ADD COLUMN IF
        // NOT EXISTS", so on a v0.1 database each ALTER TABLE below fails
        // with "duplicate column name" the first time it's re-run against
        // an already-migrated v0.2+ database - that specific error is
        // expected and ignored; anything else is a real failure.
        for stmt in [
            "ALTER TABLE cameras ADD COLUMN recording_enabled INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE cameras ADD COLUMN segment_seconds INTEGER NOT NULL DEFAULT 300",
            "ALTER TABLE cameras ADD COLUMN retention_max_age_secs INTEGER",
            "ALTER TABLE cameras ADD COLUMN retention_max_size_bytes INTEGER",
        ] {
            if let Err(err) = sqlx::query(stmt).execute(&self.pool).await {
                let msg = err.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(err.into());
                }
            }
        }

        Ok(())
    }

    pub async fn list_cameras(&self) -> Result<Vec<Camera>> {
        let rows = sqlx::query_as::<_, CameraRow>("SELECT * FROM cameras ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Camera::try_from).collect()
    }

    pub async fn get_camera(&self, id: Uuid) -> Result<Option<Camera>> {
        let row = sqlx::query_as::<_, CameraRow>("SELECT * FROM cameras WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(Camera::try_from).transpose()
    }

    pub async fn upsert_camera(&self, camera: &Camera) -> Result<()> {
        let (kind, device_path, rtsp_url) = match &camera.kind {
            CameraKind::Usb { device_path } => ("usb", Some(device_path.clone()), None),
            CameraKind::Rtsp { url } => ("rtsp", None, Some(url.clone())),
        };
        let codec = match camera.codec {
            StreamCodec::Vp8 => "vp8",
            StreamCodec::H264 => "h264",
        };
        sqlx::query(
            r#"
            INSERT INTO cameras (
                id, name, kind, device_path, rtsp_url, enabled, width, height, framerate, codec,
                recording_enabled, segment_seconds, retention_max_age_secs, retention_max_size_bytes
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                kind = excluded.kind,
                device_path = excluded.device_path,
                rtsp_url = excluded.rtsp_url,
                enabled = excluded.enabled,
                width = excluded.width,
                height = excluded.height,
                framerate = excluded.framerate,
                codec = excluded.codec,
                recording_enabled = excluded.recording_enabled,
                segment_seconds = excluded.segment_seconds,
                retention_max_age_secs = excluded.retention_max_age_secs,
                retention_max_size_bytes = excluded.retention_max_size_bytes
            "#,
        )
        .bind(camera.id.to_string())
        .bind(&camera.name)
        .bind(kind)
        .bind(device_path)
        .bind(rtsp_url)
        .bind(camera.enabled)
        .bind(camera.width as i64)
        .bind(camera.height as i64)
        .bind(camera.framerate as i64)
        .bind(codec)
        .bind(camera.recording.enabled)
        .bind(camera.recording.segment_seconds as i64)
        .bind(camera.recording.retention_max_age_secs.map(|v| v as i64))
        .bind(camera.recording.retention_max_size_bytes.map(|v| v as i64))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_camera(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM cameras WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// True if a usb camera row already exists for this device path -
    /// used by startup auto-discovery to avoid re-inserting duplicates
    /// on every restart.
    pub async fn usb_camera_exists(&self, device_path: &str) -> Result<bool> {
        let row = sqlx::query("SELECT COUNT(*) as c FROM cameras WHERE device_path = ?")
            .bind(device_path)
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.try_get("c")?;
        Ok(count > 0)
    }
}
