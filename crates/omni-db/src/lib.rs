//! SQLite-backed storage for camera configuration and motion events.
//!
//! SQLite is intentionally chosen over a server database for a
//! single-box NVR: zero ops, file-based (easy to back up alongside
//! recordings), and more than fast enough for the write volume here
//! (camera config changes and event metadata, not frame data).

use anyhow::Result;
use omni_core::{
    Camera, CameraKind, MotionEvent, MotionSettings, RecordingSettings, RecordingTrigger,
    Rotation, StreamCodec,
};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::{FromRow, Row};
use uuid::Uuid;

/// Session tokens are stored hashed (SHA-256, hex), never in plaintext -
/// they're bearer credentials good for 30 days (`SESSION_LIFETIME` in
/// `omni-server::auth`), so a leaked DB file (backup, misconfigured
/// permissions, ...) shouldn't hand out ready-to-use sessions the way a
/// plaintext copy would. A fast, unsalted hash is fine here (unlike a
/// password): the input is already a 48-character cryptographically
/// random token (`auth::generate_token`), not a human-memorable secret,
/// so there's no offline dictionary/rainbow-table attack to defend
/// against - the only thing this protects against is a bulk DB leak
/// directly yielding usable session cookies.
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

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
    rotation: String,
    sort_order: i64,
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
        let rotation = match row.rotation.as_str() {
            "none" => Rotation::None,
            "clockwise90" => Rotation::Clockwise90,
            "rotate180" => Rotation::Rotate180,
            "counter_clockwise90" => Rotation::CounterClockwise90,
            other => anyhow::bail!("unknown rotation in db: {other}"),
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
            rotation,
            sort_order: row.sort_order,
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

        // Singleton row, separate from `admin_user`. RTSP Basic auth
        // needs the plaintext password at server-start time (to build
        // the "user:pass" credential GStreamer's RTSPAuth checks
        // against) - unlike the HTTP admin password, there's no
        // one-way-hash option here, so this is intentionally a distinct,
        // narrower-blast-radius credential rather than reusing the admin
        // one. See docs/ARCHITECTURE.md.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS rtsp_credentials (
                id        INTEGER PRIMARY KEY CHECK (id = 1),
                username  TEXT NOT NULL,
                password  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Devices the user has explicitly deleted and doesn't want
        // auto-discovery to keep re-adding - see `ignore_usb_device`'s
        // docs for why this needs to exist at all (deleting a USB
        // camera's `cameras` row alone doesn't stick).
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS ignored_usb_devices (
                device_path  TEXT PRIMARY KEY
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
            // v0.9: rotation and manual dashboard ordering.
            "ALTER TABLE cameras ADD COLUMN rotation TEXT NOT NULL DEFAULT 'none'",
            "ALTER TABLE cameras ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0",
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
        let rows =
            sqlx::query_as::<_, CameraRow>("SELECT * FROM cameras ORDER BY sort_order, created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Camera::try_from).collect()
    }

    /// A `sort_order` that puts a newly added camera after every existing
    /// one, so it shows up at the end of the dashboard grid instead of
    /// wherever its default `0` would happen to land relative to cameras
    /// that have already been manually reordered.
    pub async fn next_sort_order(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(sort_order), -1) + 1 AS next FROM cameras")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("next")?)
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
        let rotation = match camera.rotation {
            Rotation::None => "none",
            Rotation::Clockwise90 => "clockwise90",
            Rotation::Rotate180 => "rotate180",
            Rotation::CounterClockwise90 => "counter_clockwise90",
        };
        sqlx::query(
            r#"
            INSERT INTO cameras (
                id, name, kind, device_path, rtsp_url, enabled, width, height, framerate, codec,
                recording_enabled, recording_trigger, segment_seconds,
                retention_max_age_secs, retention_max_size_bytes,
                motion_enabled, motion_sensitivity, motion_webhook_url,
                rotation, sort_order
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                motion_webhook_url = excluded.motion_webhook_url,
                rotation = excluded.rotation,
                sort_order = excluded.sort_order
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
        .bind(rotation)
        .bind(camera.sort_order)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Assigns sequential `sort_order` values (0, 1, 2, ...) to cameras in
    /// `ordered_ids`, matching the order the caller wants them displayed
    /// in - used by drag-and-drop reordering in the dashboard, which
    /// naturally produces "here's the full new order" rather than a
    /// single camera's new position.
    pub async fn reorder_cameras(&self, ordered_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (index, id) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE cameras SET sort_order = ? WHERE id = ?")
                .bind(index as i64)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
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

    /// Marks a USB device path as never to be auto-(re)discovered -
    /// without this, deleting a USB camera from the UI doesn't stick:
    /// `auto_discover_usb_cameras` runs on every server restart and via
    /// "Rescan USB cameras", and would just see the still-plugged-in
    /// device as unknown again and re-add it. Meant to be called
    /// alongside `delete_camera` for a USB camera - e.g. one that's
    /// actually used for something else on this machine and shouldn't be
    /// managed by OmniMonitor at all.
    pub async fn ignore_usb_device(&self, device_path: &str) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO ignored_usb_devices (device_path) VALUES (?)")
            .bind(device_path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Reverses `ignore_usb_device` - the device is eligible for
    /// auto-discovery again from the next rescan.
    pub async fn unignore_usb_device(&self, device_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM ignored_usb_devices WHERE device_path = ?")
            .bind(device_path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn is_usb_device_ignored(&self, device_path: &str) -> Result<bool> {
        let row = sqlx::query("SELECT COUNT(*) as c FROM ignored_usb_devices WHERE device_path = ?")
            .bind(device_path)
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.try_get("c")?;
        Ok(count > 0)
    }

    pub async fn list_ignored_usb_devices(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT device_path FROM ignored_usb_devices ORDER BY device_path")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| Ok(r.try_get("device_path")?)).collect()
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

    /// The RTSP server's Basic-auth credential, if bootstrapped yet.
    pub async fn rtsp_credentials(&self) -> Result<Option<(String, String)>> {
        let row = sqlx::query("SELECT username, password FROM rtsp_credentials WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(row) => Some((row.try_get("username")?, row.try_get("password")?)),
            None => None,
        })
    }

    pub async fn set_rtsp_credentials(&self, username: &str, password: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO rtsp_credentials (id, username, password) VALUES (1, ?, ?)
            ON CONFLICT(id) DO UPDATE SET username = excluded.username, password = excluded.password
            "#,
        )
        .bind(username)
        .bind(password)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_session(&self, token: &str, expires_at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        sqlx::query("INSERT INTO sessions (token, expires_at) VALUES (?, ?)")
            .bind(hash_token(token))
            .bind(expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// True if `token` names a session that hasn't expired.
    pub async fn session_valid(&self, token: &str) -> Result<bool> {
        let row = sqlx::query("SELECT expires_at FROM sessions WHERE token = ?")
            .bind(hash_token(token))
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(false) };
        let expires_at: chrono::DateTime<chrono::Utc> = row.try_get("expires_at")?;
        Ok(expires_at > chrono::Utc::now())
    }

    pub async fn delete_session(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(hash_token(token))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Deletes every session except `keep_token` - used when the admin
    /// password changes, so a session token that leaked before the
    /// change (the whole reason to change it) doesn't just keep working
    /// afterwards. The session making the change request itself is kept
    /// so the user isn't logged out by their own password change.
    pub async fn delete_sessions_except(&self, keep_token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token != ?")
            .bind(hash_token(keep_token))
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
