//! Background reaper enforcing each camera's recording retention policy:
//! delete the oldest `.webm` segments once the camera's recordings
//! directory is older than `retention_max_age_secs` and/or bigger than
//! `retention_max_size_bytes` - whichever limit is set (both can be, and
//! either being hit triggers a delete). This is what turns
//! `splitmuxsink`'s endless segment-writing into bounded "forever loop"
//! recording instead of eventually filling the disk.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use omni_db::Db;
use tokio::time::interval;
use tracing::{info, warn};

const REAP_INTERVAL: Duration = Duration::from_secs(60);

pub async fn run(db: Db, data_dir: PathBuf) {
    let mut ticker = interval(REAP_INTERVAL);
    loop {
        ticker.tick().await;
        if let Err(err) = reap_once(&db, &data_dir).await {
            warn!(%err, "retention reaper pass failed");
        }
    }
}

async fn reap_once(db: &Db, data_dir: &Path) -> anyhow::Result<()> {
    let cameras = db.list_cameras().await?;
    for camera in cameras {
        if !camera.recording.enabled {
            continue;
        }
        let max_age = camera.recording.retention_max_age_secs;
        let max_size = camera.recording.retention_max_size_bytes;
        if max_age.is_none() && max_size.is_none() {
            // Shouldn't happen - validated when recording is enabled via
            // the API - but never delete-nothing-forever silently either.
            continue;
        }
        let dir = data_dir.join("recordings").join(camera.id.to_string());
        if let Err(err) = reap_camera_dir(&dir, max_age, max_size).await {
            warn!(camera = %camera.id, %err, "retention pass failed for camera");
        }
    }
    Ok(())
}

async fn reap_camera_dir(
    dir: &Path,
    max_age_secs: Option<u64>,
    max_size_bytes: Option<u64>,
) -> anyhow::Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("webm") {
            continue;
        }
        let meta = entry.metadata().await?;
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((path, meta.len(), modified));
    }
    // Never touch the only segment - and never the newest one, which
    // `splitmuxsink` almost certainly still has open for writing.
    if files.len() <= 1 {
        return Ok(());
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    let newest_index = files.len() - 1;

    let now = SystemTime::now();
    let mut remaining_total: u64 = files.iter().map(|(_, size, _)| *size).sum();

    for (i, (path, size, modified)) in files.iter().enumerate() {
        if i == newest_index {
            continue;
        }
        let too_old = max_age_secs.is_some_and(|max| {
            now.duration_since(*modified)
                .map(|age| age.as_secs() > max)
                .unwrap_or(false)
        });
        let too_big = max_size_bytes.is_some_and(|max| remaining_total > max);
        if !too_old && !too_big {
            continue;
        }
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                info!(path = %path.display(), "deleted recording segment (retention)");
                remaining_total = remaining_total.saturating_sub(*size);
            }
            Err(err) => warn!(path = %path.display(), %err, "failed to delete recording segment"),
        }
    }
    Ok(())
}
