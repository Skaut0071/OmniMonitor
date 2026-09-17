//! SQLite-backed storage for camera configuration and motion events.
//!
//! SQLite is intentionally chosen over a server database for a
//! single-box NVR: zero ops, file-based (easy to back up alongside
//! recordings), and more than fast enough for the write volume here
//! (camera config changes and event metadata, not frame data).

use anyhow::Result;
use omni_core::{
    Camera, CameraKind, MotionEvent, MotionSettings, RecordingSettings, RecordingTrigger,
    StreamCodec,
};
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
    recording_trigger: String,
    segment_seconds: i64,
    retention_max_age_secs: Option<i64>,
    retention_max_size_bytes: Option<i64>,
    motion_enabled: bool,
    motion_sensitivity: i64,
    motion_webhook_url: Option<String>,
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
        let trigger = match row.recording_trigger.as_str() {
            "continuous" => RecordingTrigger::Continuous,
            "motion" => RecordingTrigger::Motion,
            other => anyhow::bail!("unknown recording trigger in db: {other}"),
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
                trigger,
                segment_seconds: row.segment_seconds as u32,
                retention_max_age_secs: row.retention_max_age_secs.map(|v| v as u64),
                retention_max_size_bytes: row.retention_max_size_bytes.map(|v| v as u64),
            },
            motion: MotionSettings {
                enabled: row.motion_enabled,
                sensitivity: row.motion_sensitivity as u8,
                webhook_url: row.motion_webhook_url,
            },
            status: None,
        })
    }
}

#[derive(FromRow)]
struct EventRow {
    id: String,
    camera_id: String,
    started_at: chrono::DateTime<chrono::Utc>,
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TryFrom<EventRow> for MotionEvent {
    type Error = anyhow::Error;

    fn try_from(row: EventRow) -> Result<Self> {
        Ok(MotionEvent {
            id: Uuid::parse_str(&row.id)?,
            camera_id: Uuid::parse_str(&row.camera_id)?,
            started_at: row.started_at,
            ended_at: row.ended_at,
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

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS motion_events (
                id          TEXT PRIMARY KEY,
                camera_id   TEXT NOT NULL,
                started_at  TEXT NOT NULL,
                ended_at    TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS motion_events_camera_idx ON motion_events(camera_id, started_at)")
            .execute(&self.pool)
            .await?;

        // Singleton row (id always 1) - see `Db::admin_user`. There is
        // exactly one account for v0.4; see docs/ROADMAP.md for
        // multi-user as future work.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS admin_user (
                id             INTEGER PRIMARY KEY CHECK (id = 1),
                username       TEXT NOT NULL,
                password_hash  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                token       TEXT PRIMARY KEY,
                created_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                expires_at  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // SQLite has no "ADD COLUMN IF NOT EXISTS", so each ALTER TABLE
        // below fails with "duplicate column name" once it's already been
        // applied to a given database file - that specific error is
        // expected and ignored on every run after the first; anything
        // else is a real failure.
        for stmt in [
            // v0.2: recording support.
            "ALTER TABLE cameras ADD COLUMN recording_enabled INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE cameras ADD COLUMN segment_seconds INTEGER NOT NULL DEFAULT 300",
            "ALTER TABLE cameras ADD COLUMN retention_max_age_secs INTEGER",
            "ALTER TABLE cameras ADD COLUMN retention_max_size_bytes INTEGER",
            // v0.3: motion detection + motion-triggered recording.
            "ALTER TABLE cameras ADD COLUMN recording_trigger TEXT NOT NULL DEFAULT 'continuous'",
            "ALTER TABLE cameras ADD COLUMN motion_enabled INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE cameras ADD COLUMN motion_sensitivity INTEGER NOT NULL DEFAULT 50",
            "ALTER TABLE cameras ADD COLUMN motion_webhook_url TEXT",
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
        let trigger = match camera.recording.trigger {
            RecordingTrigger::Continuous => "continuous",
            RecordingTrigger::Motion => "motion",
        };
        sqlx::query(
            r#"
            INSERT INTO cameras (
                id, name, kind, device_path, rtsp_url, enabled, width, height, framerate, codec,
                recording_enabled, recording_trigger, segment_seconds,
                retention_max_age_secs, retention_max_size_bytes,
                motion_enabled, motion_sensitivity, motion_webhook_url
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                recording_trigger = excluded.recording_trigger,
                segment_seconds = excluded.segment_seconds,
                retention_max_age_secs = excluded.retention_max_age_secs,
                retention_max_size_bytes = excluded.retention_max_size_bytes,
                motion_enabled = excluded.motion_enabled,
                motion_sensitivity = excluded.motion_sensitivity,
                motion_webhook_url = excluded.motion_webhook_url
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
        .bind(trigger)
        .bind(camera.recording.segment_seconds as i64)
        .bind(camera.recording.retention_max_age_secs.map(|v| v as i64))
        .bind(camera.recording.retention_max_size_bytes.map(|v| v as i64))
        .bind(camera.motion.enabled)
        .bind(camera.motion.sensitivity as i64)
        .bind(&camera.motion.webhook_url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_camera(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM cameras WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM motion_events WHERE camera_id = ?")
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

    /// Opens a new motion event for a camera (motion just started).
    pub async fn open_motion_event(&self, camera_id: Uuid) -> Result<MotionEvent> {
        let event = MotionEvent {
            id: Uuid::new_v4(),
            camera_id,
            started_at: chrono::Utc::now(),
            ended_at: None,
        };
        sqlx::query("INSERT INTO motion_events (id, camera_id, started_at) VALUES (?, ?, ?)")
            .bind(event.id.to_string())
            .bind(event.camera_id.to_string())
            .bind(event.started_at)
            .execute(&self.pool)
            .await?;
        Ok(event)
    }

    /// Closes a specific motion event by id (motion just ended). Callers
    /// must track the id `open_motion_event` gave them and close that
    /// exact event - not "whatever's latest for this camera": a settings
    /// change can start a *second* pipeline (and motion watcher) for the
    /// same camera before the old one's shutdown code runs, and a
    /// latest-open-event lookup would then race and close the new
    /// pipeline's event instead of the old, truly orphaned one. Caught by
    /// testing a settings-change restart while motion was active.
    pub async fn close_motion_event(&self, event_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE motion_events SET ended_at = ? WHERE id = ? AND ended_at IS NULL")
            .bind(chrono::Utc::now())
            .bind(event_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_motion_events(&self, camera_id: Uuid, limit: i64) -> Result<Vec<MotionEvent>> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT * FROM motion_events WHERE camera_id = ? ORDER BY started_at DESC LIMIT ?",
        )
        .bind(camera_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(MotionEvent::try_from).collect()
    }

    /// The single admin account, if one has been bootstrapped yet.
    pub async fn admin_user(&self) -> Result<Option<(String, String)>> {
        let row = sqlx::query("SELECT username, password_hash FROM admin_user WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(row) => Some((row.try_get("username")?, row.try_get("password_hash")?)),
            None => None,
        })
    }

    /// Creates or overwrites the single admin account.
    pub async fn set_admin_user(&self, username: &str, password_hash: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO admin_user (id, username, password_hash) VALUES (1, ?, ?)
            ON CONFLICT(id) DO UPDATE SET username = excluded.username, password_hash = excluded.password_hash
            "#,
        )
        .bind(username)
        .bind(password_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_session(&self, token: &str, expires_at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        sqlx::query("INSERT INTO sessions (token, expires_at) VALUES (?, ?)")
            .bind(token)
            .bind(expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// True if `token` names a session that hasn't expired.
    pub async fn session_valid(&self, token: &str) -> Result<bool> {
        let row = sqlx::query("SELECT expires_at FROM sessions WHERE token = ?")
            .bind(token)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(false) };
        let expires_at: chrono::DateTime<chrono::Utc> = row.try_get("expires_at")?;
        Ok(expires_at > chrono::Utc::now())
    }

    pub async fn delete_session(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sweeps expired sessions - called periodically, not on every
    /// request, since an expired-but-not-yet-swept token is already
    /// correctly rejected by `session_valid`.
    pub async fn delete_expired_sessions(&self) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(chrono::Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
