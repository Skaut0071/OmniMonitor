//! Cheap, non-invasive "is this camera there at all" checks for a camera
//! with no capture pipeline currently running - most ephemeral cameras
//! (no recording/motion enabled, nobody currently watching) spend most of
//! their time in exactly that state, and the status overview
//! (`GET /api/cameras/status`) needs *some* answer for them without
//! actually opening the device or connecting a real RTSP session just to
//! populate a status page.

use std::net::ToSocketAddrs;
use std::time::Duration;

use omni_core::{Camera, CameraKind};

const RTSP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// For a USB camera: does the device node still exist (plugged in, driver
/// bound)? For an RTSP camera: does a plain TCP connect to its host:port
/// succeed within a short timeout? Neither actually confirms the camera
/// will *stream* successfully (a USB device node can exist but be wedged,
/// per the "device busy" issue seen during v0.9's testing; an RTSP
/// TCP-reachable host might still reject the actual RTSP handshake) -
/// this is deliberately just a presence/reachability check, cheap enough
/// to run for every idle camera on every status-page load.
pub async fn probe_reachable(camera: &Camera) -> bool {
    match &camera.kind {
        CameraKind::Usb { device_path } => tokio::fs::metadata(device_path).await.is_ok(),
        CameraKind::Rtsp { url } => probe_rtsp_reachable(url).await,
    }
}

async fn probe_rtsp_reachable(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let port = parsed.port().unwrap_or(554);
    let host = host.to_string();

    // `ToSocketAddrs::to_socket_addrs` does blocking DNS resolution -
    // fine here since this whole probe already runs inside
    // `spawn_blocking` isn't needed for the connect itself (that's
    // async), but resolution needs a blocking thread so it doesn't stall
    // the runtime on a slow/hung DNS lookup.
    let addr = match tokio::task::spawn_blocking(move || {
        (host.as_str(), port).to_socket_addrs().ok()?.next()
    })
    .await
    {
        Ok(Some(addr)) => addr,
        _ => return false,
    };

    tokio::time::timeout(RTSP_PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .is_ok_and(|res| res.is_ok())
}
