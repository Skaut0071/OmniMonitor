//! Turns a camera's raw motion-active signal (from `omni-capture`) into
//! logged events and an optional webhook call. Purely reactive: doesn't
//! decide *whether* motion detection runs (that's `Supervisor`, based on
//! `Camera::motion`/`Camera::recording`) - just watches the resulting
//! `watch::Receiver<bool>` for transitions.

use std::net::IpAddr;

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

/// `omni_core::validate_webhook_url` only checks for an `http(s)://`
/// prefix - deliberately, since it also runs in the browser (shared with
/// `omni-wasm`) where a real SSRF check (resolving the host) isn't
/// meaningful. The actual guard against this webhook being used to reach
/// things it shouldn't lives here instead, right before the request is
/// sent:
///
/// - loopback/link-local/unspecified destinations are rejected (this
///   would otherwise let an authenticated admin - the only one who can
///   set this URL - point the server at itself or, on a cloud VM, at the
///   `169.254.169.254` instance-metadata endpoint). Ordinary private LAN
///   addresses (192.168.x.x, 10.x.x.x, ...) are deliberately still
///   allowed: notifying a home-automation box on the same LAN as the
///   camera is the actual, documented point of this feature, so a
///   blanket "no private IPs" rule would break it.
/// - redirects aren't followed, so a webhook endpoint that starts out
///   pointing somewhere allowed can't 302 the request somewhere that
///   isn't.
///
/// This check happens at resolve/send time, not when the URL is saved -
/// still not airtight against a host that resolves differently a moment
/// later (DNS rebinding), but closes the straightforward case without
/// needing to intercept the TCP connect itself.
async fn send_webhook(url: &str, camera_id: uuid::Uuid, camera_name: &str) {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        tracing::warn!(%url, "motion webhook URL failed to parse, not sending");
        return;
    };
    let Some(host) = parsed.host_str() else {
        tracing::warn!(%url, "motion webhook URL has no host, not sending");
        return;
    };
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
    let resolved = match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => addrs.map(|a| a.ip()).collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!(%url, %err, "motion webhook host failed to resolve, not sending");
            return;
        }
    };
    if resolved.is_empty() || resolved.iter().any(|ip| is_disallowed_webhook_target(*ip)) {
        tracing::warn!(%url, ?resolved, "motion webhook resolves to a disallowed address, not sending");
        return;
    }

    let body = serde_json::json!({
        "event": "motion_started",
        "camera_id": camera_id,
        "camera_name": camera_name,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        tracing::warn!("failed to build webhook HTTP client");
        return;
    };
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

fn is_disallowed_webhook_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fe80::/10 (link-local) - std has no stable is_link_local
                // for Ipv6Addr, so check the prefix directly.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}
