//! Runs the shell commands configured via `omni_core::LedControl` to turn
//! a camera's LED ring on/off - see `Supervisor`'s module docs for when
//! these fire. Deliberately just `sh -c` on whatever string the admin
//! configured: this is the same trust boundary as the rest of this
//! server's admin-only settings (there's exactly one admin account, and
//! it already has full control over which cameras exist and where their
//! recordings go), and there's no generic cross-vendor "turn off this
//! USB device's indicator LED" API to call instead - see `LedControl`'s
//! docs for why.

use std::time::Duration;

use uuid::Uuid;

/// Runs `command` in the background and logs the outcome; never blocks
/// the caller (the supervisor's viewer-count bookkeeping shouldn't wait
/// on an external command that might hang or fail). Bounded by a timeout
/// so a misconfigured command (e.g. one that never exits) can't leak
/// tasks indefinitely.
pub fn spawn_run(camera_id: Uuid, label: &'static str, command: String) {
    tokio::spawn(async move {
        let attempt = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .output(),
        )
        .await;
        match attempt {
            Ok(Ok(output)) if output.status.success() => {
                tracing::debug!(camera = %camera_id, label, "led command succeeded");
            }
            Ok(Ok(output)) => {
                tracing::warn!(
                    camera = %camera_id,
                    label,
                    status = %output.status,
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "led command exited with a non-zero status"
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(camera = %camera_id, label, %err, "failed to run led command");
            }
            Err(_) => {
                tracing::warn!(camera = %camera_id, label, "led command timed out after 5s");
            }
        }
    });
}
