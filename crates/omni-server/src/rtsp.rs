//! RTSP *server*: re-serves each camera's already-encoded live stream at
//! `rtsp://<host>:5544/<camera-id>`, so third-party NVR/VMS/player
//! software can pull from OmniMonitor the normal way - this is the other
//! half of "USB cameras act like network cameras" (the first half,
//! consuming an RTSP camera as a *client*, has existed since v0.2).
//!
//! Reuses the exact same shared-pipeline mechanism as WebRTC viewers:
//! an RTSP client is just another `Supervisor::acquire_viewer` caller.
//! Frames already encoded to VP8 by the one running `CaptureSession` per
//! camera are pushed into a per-session `appsrc` via `rtpvp8pay`, no
//! second encode pass and no second device open for USB cameras.
//!
//! GStreamer's RTSP server runs its own GLib main loop, which needs a
//! dedicated OS thread (it blocks). Its `media-configure` callback fires
//! on that thread, not on a tokio worker, so it hands off to the tokio
//! runtime via `Handle::spawn` rather than doing any async work itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_rtsp_server::prelude::*;
use gstreamer_rtsp_server::RTSPServer;
use omni_core::Camera;
use uuid::Uuid;

use crate::state::AppState;

pub struct RtspServer {
    mount_points: gstreamer_rtsp_server::RTSPMountPoints,
    /// Factories are only removable by the exact object that was added,
    /// per the C API this wraps - keep them around so `remove_camera` can
    /// look the right one up instead of trying to reconstruct it.
    factories: Mutex<HashMap<Uuid, gstreamer_rtsp_server::RTSPMediaFactory>>,
    rt_handle: tokio::runtime::Handle,
    /// Role-grant structure applied to every camera's factory, built once
    /// in `start`. Verified via a standalone Python GI test that
    /// `add_role_from_structure` (the only role-granting API the Rust
    /// bindings expose) needs a structure named after the role, with
    /// `media.factory.access`/`media.factory.construct` set to plain
    /// booleans - a `glib::Variant`-wrapped bool silently produces a 404
    /// on access, not the expected success or 401.
    role_structure: gst::Structure,
}

impl RtspServer {
    /// Starts the RTSP server on `port` on a dedicated OS thread (GLib's
    /// main loop is blocking) and returns a handle for registering
    /// per-camera mount points. Every mount point requires HTTP Basic
    /// auth with `rtsp_username`/`rtsp_password` - unauthenticated
    /// requests get a bare 401, per a deny-by-default empty
    /// `RTSPToken` set as the server's default token.
    pub fn start(port: u16, rtsp_username: &str, rtsp_password: &str) -> Arc<Self> {
        let server = RTSPServer::default();
        server.set_service(&port.to_string());
        let mount_points = server
            .mount_points()
            .expect("a freshly constructed RTSPServer always has mount points");

        let auth = gstreamer_rtsp_server::RTSPAuth::new();
        let credential =
            glib::base64_encode(format!("{rtsp_username}:{rtsp_password}").as_bytes());
        let token = gstreamer_rtsp_server::RTSPToken::builder()
            .field("media.factory.role", "user")
            .build();
        auth.add_basic(credential.as_str(), &token);
        let mut default_token = gstreamer_rtsp_server::RTSPToken::builder().build();
        auth.set_default_token(Some(&mut default_token));
        server.set_auth(Some(&auth));

        let role_structure = gst::Structure::builder("user")
            .field("media.factory.access", true)
            .field("media.factory.construct", true)
            .build();

        let this = Arc::new(Self {
            mount_points,
            factories: Mutex::new(HashMap::new()),
            rt_handle: tokio::runtime::Handle::current(),
            role_structure,
        });

        std::thread::spawn(move || {
            let main_context = glib::MainContext::new();
            let _guard = main_context.acquire();
            if let Err(err) = server.attach(Some(&main_context)) {
                tracing::error!(%err, "failed to start RTSP server");
                return;
            }
            tracing::info!(port, "RTSP server listening");
            let main_loop = glib::MainLoop::new(Some(&main_context), false);
            main_loop.run();
        });

        this
    }

    /// Adds (or replaces) a `/<camera-id>` mount point streaming that
    /// camera's live VP8 feed. Safe to call again for the same camera
    /// (e.g. after a settings change) - replaces the old factory.
    pub fn add_camera(self: &Arc<Self>, state: Arc<AppState>, camera: Camera) {
        let factory = gstreamer_rtsp_server::RTSPMediaFactory::new();
        // `is-live=true`/`do-timestamp=true`: this appsrc has no fixed
        // rate of its own - it just forwards whatever the shared capture
        // pipeline produces, whenever it produces it.
        factory.set_launch(
            "( appsrc name=src is-live=true format=time do-timestamp=true \
               caps=video/x-vp8 ! rtpvp8pay name=pay0 pt=96 )",
        );
        // One shared appsrc per *session*, not per server: two RTSP
        // clients on the same camera each get their own
        // `Supervisor::acquire_viewer` slot, same as two WebRTC viewers
        // would.
        factory.set_shared(false);
        factory.add_role_from_structure(&self.role_structure);

        let this = Arc::clone(self);
        let camera_for_cb = camera.clone();
        let state_for_cb = Arc::clone(&state);
        factory.connect_media_configure(move |_factory, media| {
            configure_media(&this.rt_handle, Arc::clone(&state_for_cb), camera_for_cb.clone(), media);
        });

        let path = format!("/{}", camera.id);
        self.mount_points.add_factory(&path, factory.clone());
        self.factories.lock().unwrap().insert(camera.id, factory);
        tracing::info!(camera = %camera.id, %path, "RTSP mount point registered");
    }

    pub fn remove_camera(&self, camera_id: Uuid) {
        if self.factories.lock().unwrap().remove(&camera_id).is_some() {
            self.mount_points.remove_factory(&format!("/{camera_id}"));
        }
    }
}

/// Runs on the RTSP server's GLib thread whenever a client requests a
/// camera's stream. Finds the session's `appsrc` and hands off to the
/// tokio runtime to acquire a viewer slot and start forwarding frames -
/// this function itself must stay synchronous and fast, it's on the same
/// thread that services every other RTSP client's protocol messages too.
fn configure_media(
    rt_handle: &tokio::runtime::Handle,
    state: Arc<AppState>,
    camera: Camera,
    media: &gstreamer_rtsp_server::RTSPMedia,
) {
    let Some(element) = media.element().dynamic_cast::<gst::Bin>().ok() else {
        tracing::error!(camera = %camera.id, "RTSP media element is not a Bin");
        return;
    };
    let Some(appsrc) = element
        .by_name("src")
        .and_then(|e| e.dynamic_cast::<gst_app::AppSrc>().ok())
    else {
        tracing::error!(camera = %camera.id, "RTSP media has no 'src' appsrc");
        return;
    };

    rt_handle.spawn(async move {
        let viewer = match state.supervisor.acquire_viewer(&camera).await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(camera = %camera.id, %err, "RTSP client: failed to start capture");
                let _ = appsrc.end_of_stream();
                return;
            }
        };
        let mut frames = viewer.frames;
        loop {
            let frame = match frames.recv().await {
                Ok(f) => f,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let mut buffer = gst::Buffer::from_slice(frame.data);
            {
                let buffer_mut = buffer.get_mut().expect("buffer has one owner here");
                if !frame.duration.is_zero() {
                    buffer_mut.set_duration(gst::ClockTime::from_nseconds(
                        frame.duration.as_nanos() as u64,
                    ));
                }
            }
            if appsrc.push_buffer(buffer).is_err() {
                // The RTSP session tore down its pipeline (client
                // disconnected) - stop forwarding, `viewer` drops below
                // and releases this session's slot on the shared
                // capture pipeline.
                break;
            }
        }
    });
}
