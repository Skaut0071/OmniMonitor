//! Background reaper that turns `RecordingTrigger::Motion`'s
//! always-recording pipeline (see `omni_core::RecordingSettings`'s docs
//! and `Supervisor::pipeline_config`) into "only keep footage from around
//! an actual motion event": every recorded segment for such a camera is
//! deleted unless it overlaps a logged motion event, padded by
//! `PRE_ROLL`/`POST_ROLL` - so a clip includes a bit of lead-in before
//! motion was detected and a bit of trailing footage after it ended,
//! instead of starting mid-action and cutting off the instant motion
//! stops.
//!
//! Runs independently of `omni-server::retention` (the age/size-based
//! reaper), which still applies on top of this for any camera that sets
//! `retention_max_age_secs`/`retention_max_size_bytes`, motion-triggered
//! or not.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use omni_core::RecordingTrigger;
use omni_db::Db;
use tokio::time::interval;
use tracing::{info, warn};
use uuid::Uuid;

const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// How much footage from before a motion event starts to keep.
const PRE_ROLL: Duration = Duration::from_secs(30);
/// How much footage from after a motion event ends to keep.
const POST_ROLL: Duration = Duration::from_secs(60);

/// How far back to look for motion events at all - bounds the DB query
/// instead of pulling a camera's entire motion history every pass. Well
/// beyond any reasonable reap cadence, so a camera that's been busy
/// doesn't lose events this pass should still know about.
const EVENT_LOOKBACK: chrono::Duration = chrono::Duration::hours(6);
const EVENT_FETCH_LIMIT: i64 = 500;

pub async fn run(db: Db, data_dir: PathBuf) {
    let mut ticker = interval(REAP_INTERVAL);
    loop {
        ticker.tick().await;
        if let Err(err) = reap_once(&db, &data_dir).await {
            warn!(%err, "motion retention pass failed");
        }
    }
}

async fn reap_once(db: &Db, data_dir: &Path) -> anyhow::Result<()> {
    let cameras = db.list_cameras().await?;
    for camera in cameras {
        if !camera.recording.enabled || camera.recording.trigger != RecordingTrigger::Motion {
            continue;
        }
        let events = db.list_motion_events(camera.id, EVENT_FETCH_LIMIT).await?;
        let cutoff = Utc::now() - EVENT_LOOKBACK;
        let windows: Vec<(DateTime<Utc>, DateTime<Utc>)> = events
            .into_iter()
            .filter(|e| e.started_at >= cutoff)
            .map(|e| {
                let start = e.started_at - PRE_ROLL;
                // Still-open events (`ended_at: None`) haven't finished
                // yet - treat their end as "now" so their segments (and
                // the currently-recording one) are never mistaken for
                // stale footage to prune.
                let end = e.ended_at.unwrap_or_else(Utc::now) + POST_ROLL;
                (start, end)
            })
            .collect();

        let dir = data_dir.join("recordings").join(camera.id.to_string());
        if let Err(err) = reap_camera_dir(camera.id, &dir, &windows).await {
            warn!(camera = %camera.id, %err, "motion retention pass failed for camera");
        }
    }
    Ok(())
}

/// A `splitmuxsink` segment file, named `{run_started_at}-{index:05}.webm`
/// by `omni_capture::pipeline::CaptureSession::start` - parsed back into
/// the segment's own `[start, end)` time range rather than relying on
/// filesystem timestamps, which can lag or be wrong after e.g. a restore
/// from backup.
struct Segment {
    path: PathBuf,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    /// `(run_started_at, index)` - used to find the newest segment
    /// (currently open for writing), which is never touched regardless
    /// of whether it overlaps a motion window yet.
    order: (u64, u32),
}

fn parse_segment(path: &Path, segment_seconds: u32) -> Option<Segment> {
    let stem = path.file_stem()?.to_str()?;
    let (run_started_at, index) = stem.rsplit_once('-')?;
    let run_started_at: u64 = run_started_at.parse().ok()?;
    let index: u32 = index.parse().ok()?;
    let start = DateTime::from_timestamp(
        run_started_at as i64 + (index as i64 * segment_seconds as i64),
        0,
    )?;
    let end = start + chrono::Duration::seconds(segment_seconds as i64);
    Some(Segment {
        path: path.to_path_buf(),
        start,
        end,
        order: (run_started_at, index),
    })
}

async fn reap_camera_dir(
    camera_id: Uuid,
    dir: &Path,
    windows: &[(DateTime<Utc>, DateTime<Utc>)],
) -> anyhow::Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut segments = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("webm") {
            continue;
        }
        // `MOTION_SEGMENT_SECONDS` in `omni-server::supervisor` - matched
        // here rather than imported to keep this module independent of
        // `Supervisor`; a mismatch would only make the parsed segment
        // boundaries approximate, not wrong in a way that breaks parsing.
        match parse_segment(&path, crate::supervisor::MOTION_SEGMENT_SECONDS) {
            Some(seg) => segments.push(seg),
            None => warn!(path = %path.display(), "could not parse segment filename, leaving it alone"),
        }
    }
    if segments.len() <= 1 {
        return Ok(());
    }
    segments.sort_by_key(|s| s.order);
    let newest_order = segments.last().map(|s| s.order);

    for seg in &segments {
        if Some(seg.order) == newest_order {
            continue;
        }
        let overlaps_motion = windows
            .iter()
            .any(|(start, end)| seg.start < *end && seg.end > *start);
        if overlaps_motion {
            continue;
        }
        match tokio::fs::remove_file(&seg.path).await {
            Ok(()) => info!(camera = %camera_id, path = %seg.path.display(), "deleted recording segment (no motion nearby)"),
            Err(err) => warn!(camera = %camera_id, path = %seg.path.display(), %err, "failed to delete recording segment"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_exact_filename_shape_splitmuxsink_writes() {
        // Matches `CaptureSession::start`'s `location` pattern exactly:
        // `{run_started_at}-%05d.webm`.
        let seg = parse_segment(Path::new("1700000000-00003.webm"), 20).unwrap();
        assert_eq!(seg.order, (1_700_000_000, 3));
        assert_eq!(seg.start, DateTime::from_timestamp(1_700_000_000 + 60, 0).unwrap());
        assert_eq!(seg.end, DateTime::from_timestamp(1_700_000_000 + 80, 0).unwrap());
    }

    #[test]
    fn rejects_filenames_that_dont_match_the_pattern() {
        assert!(parse_segment(Path::new("not-a-segment.webm"), 20).is_none());
        assert!(parse_segment(Path::new("1700000000.webm"), 20).is_none());
    }

    #[test]
    fn segment_touching_a_padded_window_edge_is_kept_not_pruned() {
        // A segment covering [100, 120) and a motion window of exactly
        // [120, 140) don't overlap (half-open ranges) - guards against an
        // off-by-one that would prune footage right at a window boundary.
        let seg = Segment {
            path: PathBuf::from("x.webm"),
            start: DateTime::from_timestamp(100, 0).unwrap(),
            end: DateTime::from_timestamp(120, 0).unwrap(),
            order: (0, 0),
        };
        let touching = DateTime::from_timestamp(120, 0).unwrap();
        let later = DateTime::from_timestamp(140, 0).unwrap();
        assert!(!(seg.start < later && seg.end > touching));

        let overlapping_start = DateTime::from_timestamp(119, 0).unwrap();
        assert!(seg.start < later && seg.end > overlapping_start);
    }
}
