use std::fs;

use v4l::capability::Flags;
use v4l::Device;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredDevice {
    pub path: String,
    pub name: String,
}

/// Enumerates `/dev/video*` nodes and keeps only the ones that actually
/// support `VIDEO_CAPTURE`. A single UVC camera usually exposes several
/// `/dev/videoN` nodes (metadata, second stream, ...); this filters those
/// out so we don't offer them as separate cameras.
pub fn list_capture_devices() -> anyhow::Result<Vec<DiscoveredDevice>> {
    let mut entries: Vec<_> = fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("video"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut out = Vec::new();
    for entry in entries {
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();

        let dev = match Device::with_path(&path) {
            Ok(d) => d,
            Err(err) => {
                tracing::debug!(%path_str, %err, "skipping video device: could not open");
                continue;
            }
        };
        let caps = match dev.query_caps() {
            Ok(c) => c,
            Err(err) => {
                tracing::debug!(%path_str, %err, "skipping video device: query_caps failed");
                continue;
            }
        };
        if !caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
            continue;
        }

        out.push(DiscoveredDevice {
            path: path_str,
            name: caps.card,
        });
    }
    Ok(out)
}
