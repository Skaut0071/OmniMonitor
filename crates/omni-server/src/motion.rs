//! Turns a camera's raw motion-active signal (from `omni-capture`) into
//! logged events and an optional webhook call. Purely reactive: doesn't
//! decide *whether* motion detection runs (that's `Supervisor`, based on
//! `Camera::motion`/`Camera::recording`) - just watches the resulting
//! `watch::Receiver<bool>` for transitions.

use omni_core::Camera;
use tokio::sync::watch;

/// Spawns a task that logs each motion start/end to `db` and, if
/// `camera.motion.webhook_url` is set, POSTs a JSON notification on
/// motion start. Runs until `motion` closes (the pipeline it belongs to
/// was stopped or replaced) - if motion was still active at that point,
/// closes the open event first rather than leaving it dangling forever
/// with `ended_at: null` (caught by testing a settings-change restart
/// while motion was active: the pipeline swap drops the old watch
/// channel with no final `false`, so without this the event would never
/// close).
pub fn spawn_watcher(db: omni_db::Db, camera: &Camera, mut motion: watch::Receiver<bool>) {
    let camera_id = camera.id;
    let camera_name = camera.name.clone();
    let webhook_url = camera.motion.webhook_url.clone();

    tokio::spawn(async move {
        let mut open_event: Option<uuid::Uuid> = None;
        // The channel starts at `false`; only react to actual changes.
        loop {
            if motion.changed().await.is_err() {
                if let Some(event_id) = open_event {
                    if let Err(err) = db.close_motion_event(event_id).await {
                        tracing::warn!(camera = %camera_id, %err, "failed to close motion event on pipeline teardown");
                    }
                }
                return;
            }
            let active = *motion.borrow();
            if active {
                match db.open_motion_event(camera_id).await {
                    Ok(event) => {
                        tracing::info!(camera = %camera_id, %event.id, "motion started");
                        open_event = Some(event.id);
                    }
                    Err(err) => tracing::warn!(camera = %camera_id, %err, "failed to log motion start"),
                }
                if let Some(url) = webhook_url.clone() {
                    let camera_name = camera_name.clone();
                    tokio::spawn(async move {
                        send_webhook(&url, camera_id, &camera_name).await;
                    });
                }
            } else if let Some(event_id) = open_event.take() {
                if let Err(err) = db.close_motion_event(event_id).await {
                    tracing::warn!(camera = %camera_id, %err, "failed to log motion end");
                } else {
                    tracing::info!(camera = %camera_id, "motion ended");
                }
            }
        }
    });
}

async fn send_webhook(url: &str, camera_id: uuid::Uuid, camera_name: &str) {
    let body = serde_json::json!({
        "event": "motion_started",
        "camera_id": camera_id,
        "camera_name": camera_name,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    let client = reqwest::Client::new();
    let result = client
        .post(url)
        .timeout(std::time::Duration::from_secs(5))
        .json(&body)
        .send()
        .await;
    match result {
        Ok(resp) if !resp.status().is_success() => {
            tracing::warn!(%url, status = %resp.status(), "motion webhook returned non-success status")
        }
        Ok(_) => tracing::debug!(%url, "motion webhook delivered"),
        Err(err) => tracing::warn!(%url, %err, "motion webhook request failed"),
    }
}
