//! Periodically re-evaluates each camera's recording schedule
//! (`omni_core::RecordingSchedule`) and applies the change (via
//! `Supervisor::apply_settings`) when it crosses a scheduled start/end
//! boundary.
//!
//! Every other reason a pipeline gets its settings applied - an actual
//! settings change, a schedule change - is triggered by something
//! happening (`omni-server::routes::update_camera`). A schedule boundary
//! is different: nothing "happens" at 22:00, time just passes, so
//! something has to actually poll for it.
//!
//! As of v0.14, `Supervisor::keeps_pipeline_alive` *is* schedule-aware:
//! a schedule-gated recording camera's pipeline actually stops (device
//! closed) once the window ends, not just its recording branch, and
//! restarts (via `ensure_running` below) when the window opens again -
//! so the camera isn't sitting open/powered for no reason outside its
//! scheduled hours. `apply_settings` recomputes `keeps_pipeline_alive`
//! and tears the pipeline down itself when it goes false and no viewer
//! is currently watching (see its docs) - this module just needs to call
//! it on each boundary crossing and start a fresh pipeline if the window
//! just opened and nothing was running. This deliberately accepts a real
//! "device busy" risk at each boundary (confirmed against this project's
//! actual USB hardware: rapid open/close cycles on the same `/dev/videoN`
//! aren't perfectly reliable even with `replace_pipeline`'s existing
//! 200ms settle sleep) as the tradeoff for the camera not staying
//! open/powered outside its scheduled hours - a deliberate choice, not
//! an oversight.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use omni_db::Db;
use uuid::Uuid;

use crate::supervisor::{schedule_is_active_now, Supervisor};

const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub async fn run(supervisor: Arc<Supervisor>, db: Db) {
    // Remembers each schedule-gated camera's last-seen active/inactive
    // state, so a rebuild only happens exactly when that flips - not on
    // every poll, which would mean disconnecting every viewer of a
    // scheduled camera every 30 seconds for no reason.
    let mut last_state: HashMap<Uuid, bool> = HashMap::new();
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        let cameras = match db.list_cameras().await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(%err, "recording schedule check: failed to list cameras");
                continue;
            }
        };

        let mut seen = std::collections::HashSet::new();
        for camera in cameras {
            if !camera.recording.enabled || !camera.recording.schedule.enabled {
                continue;
            }
            seen.insert(camera.id);
            let active = schedule_is_active_now(&camera.recording.schedule);
            if last_state.get(&camera.id) == Some(&active) {
                continue;
            }
            last_state.insert(camera.id, active);
            tracing::info!(
                camera = %camera.id,
                active,
                "recording schedule boundary crossed, rebuilding pipeline"
            );
            // `apply_settings` only touches a pipeline that's already
            // running, and - since a schedule boundary never changes
            // resolution/rotation/motion-detection-presence, only
            // whether recording is currently active - takes the dynamic
            // add/remove-the-recording-branch path rather than a full
            // rebuild, so this doesn't disconnect anyone currently
            // watching the camera live.
            if let Err(err) = supervisor.apply_settings(&camera).await {
                tracing::warn!(camera = %camera.id, %err, "failed to apply schedule change to pipeline");
                continue;
            }
            // A camera whose pipeline wasn't running yet (no viewers,
            // motion detection not otherwise needed) needs an explicit
            // start now that it's due to record - `apply_settings` is a
            // no-op for a camera with nothing running.
            if active {
                if let Err(err) = supervisor.ensure_running(&camera).await {
                    tracing::warn!(camera = %camera.id, %err, "failed to start pipeline for scheduled recording");
                }
            }
        }

        // Forget cameras that no longer have an enabled schedule, so a
        // schedule re-enabled later starts fresh instead of comparing
        // against a stale state from before it was turned off.
        last_state.retain(|id, _| seen.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Datelike, TimeZone, Timelike};

    // `schedule_is_active_now` itself is exercised via
    // `omni_core::camera::tests` (the pure predicate) - this module's
    // own logic (poll, detect a flip, rebuild) needs a running
    // Supervisor/GStreamer to test meaningfully, which is exactly what
    // was verified live instead (see docs/ARCHITECTURE.md).
    #[test]
    fn weekday_and_minute_use_chrono_correctly() {
        // Sanity check on the Datelike/Timelike calls
        // `schedule_is_active_now` relies on - Monday should be day 0,
        // and minute-of-day math should match a known wall-clock time.
        let dt = chrono::Local.with_ymd_and_hms(2026, 1, 5, 14, 30, 0).unwrap(); // a Monday
        assert_eq!(dt.weekday().num_days_from_monday(), 0);
        assert_eq!(dt.hour() * 60 + dt.minute(), 14 * 60 + 30);
    }
}
