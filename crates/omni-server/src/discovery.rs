use omni_db::Db;

/// Registers any USB camera the OS can see that isn't already in the DB.
/// Run at startup and available via `POST /api/cameras/discover` so newly
/// plugged-in cameras can be picked up without a restart.
pub async fn auto_discover_usb_cameras(db: &Db) {
    let devices = match omni_capture::list_capture_devices() {
        Ok(devices) => devices,
        Err(err) => {
            tracing::warn!(%err, "USB camera discovery failed");
            return;
        }
    };

    for dev in devices {
        match db.usb_camera_exists(&dev.path).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(%err, device = %dev.path, "failed checking existing camera");
                continue;
            }
        }

        let camera = omni_core::Camera::new_usb(dev.name.clone(), dev.path.clone());
        match db.upsert_camera(&camera).await {
            Ok(()) => tracing::info!(name = %dev.name, path = %dev.path, "registered USB camera"),
            Err(err) => {
                tracing::warn!(%err, device = %dev.path, "failed to register discovered camera")
            }
        }
    }
}
