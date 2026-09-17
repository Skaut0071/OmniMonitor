# Architecture

## Goal

OmniMonitor is a self-hosted, open-source NVR (Network Video Recorder). The
pitch is two things combined:

1. A modern, Ubiquiti-Protect-style dashboard UI/UX.
2. First-class support for **USB cameras** (UVC webcams over V4L2), treated
   as full peers of network/RTSP cameras - the same live-preview and
   recording pipeline handles both.

v0.2 targets Linux only, HTTP only (no TLS - see "HTTPS" below), single-box
deployments. Default ports: **8090** for the web UI/API, **5544** for RTSP
*server* (reserved for a future milestone - re-serving OmniMonitor's own
streams over RTSP - not implemented yet; consuming a camera's RTSP stream
as a *client*, which is most of what "RTSP support" means day to day, is
implemented).

## Why these technology choices

- **Rust backend.** Memory safety for something that runs unattended and
  touches untrusted network input (camera RTSP streams, browser WebRTC
  offers), plus genuinely good async I/O (tokio) for handling many camera
  streams concurrently.
- **SQLite via `sqlx`** for camera configuration and (later) event metadata.
  This is a single-box NVR: zero ops, file-based (backs up alongside
  recordings), and far more throughput than camera-config-change or
  event-insert workloads need. If OmniMonitor ever grows a multi-node
  deployment mode, that's a reason to add a Postgres backend *alongside*
  this, not a reason SQLite was wrong for v0.1.
- **V4L2 (`v4l` crate) for device discovery**, **GStreamer for capture,
  encode and recording**, **webrtc-rs for the browser transport.** See the
  capture pipeline section below for why these three specifically, and why
  they're not redundant with each other.
- **Svelte + TypeScript (Vite) frontend**, with a small **Rust -> WASM**
  module (`omni-wasm`, built via `wasm-pack`) for logic that must be
  identical on both sides of the wire. Browser-native APIs
  (`RTCPeerConnection`, `<video>`, WebSocket) are used directly from
  TypeScript - there is no value in fighting those through a Rust wasm
  binding layer. What *is* valuable: **validation rules that must match the
  server exactly** (e.g. "camera name must be 1-64 chars", "a recording
  policy needs at least one retention limit"). Those rules live once, in
  `omni-core::validate`, compiled twice - natively into `omni-server`, and
  to wasm into the admin UI - so the two can never drift. This is
  intentionally a small, targeted use of wasm rather than "the whole
  frontend is Rust": that would fight the browser's own APIs for no
  benefit at this stage.

## Workspace layout

```
crates/
  omni-core     - shared types (Camera, CameraKind, RecordingSettings,
                  AppConfig) + validation. No tokio/gstreamer/etc. deps -
                  must compile to wasm32.
  omni-db       - SQLite storage for camera config (sqlx).
  omni-capture  - V4L2 device discovery (v4l) + GStreamer capture/encode/
                  record pipeline ({v4l2src|rtspsrc} -> decodebin -> vp8enc
                  -> tee -> {appsink, splitmuxsink}).
  omni-webrtc   - Turns an SDP offer + a live-frame feed into a WebRTC
                  connection (webrtc-rs) for one viewer. Owns RTP/ICE/DTLS;
                  knows nothing about V4L2, RTSP, pixels, or recording.
  omni-wasm     - wasm-bindgen exports of omni-core::validate, built with
                  wasm-pack into frontend/src/lib/wasm (gitignored, built
                  artifact - see scripts/build-wasm.sh).
  omni-server   - axum HTTP/WebSocket server (the binary):
                  - routes.rs / ws.rs: REST API + WebRTC signaling.
                  - supervisor.rs: owns the one running capture pipeline
                    per camera, shared across every live viewer and
                    recording (see below).
                  - retention.rs: background reaper enforcing each
                    camera's recording retention policy.
                  Serves the built frontend as static files.
frontend/       - Svelte + TypeScript + Vite. Dark, Ubiquiti-Protect-style
                  dashboard: camera grid, per-camera live WebRTC tile,
                  add/remove cameras, recording settings, recordings
                  browser/player.
```

## The USB-camera-as-network-camera capture pipeline

This is the core technical bet of the project, so it's worth spelling out
end to end for a camera - USB or RTSP - arriving in a browser tab, plus,
if recording is enabled, on disk:

```
Source                 GStreamer pipeline
/dev/video0    ---->    {v4l2src|rtspsrc} -> decodebin -> videoconvert
  or rtsp://...          -> videoscale -> videorate -> vp8enc -> tee
                                              |
                    +-------------------------+-------------------------+
                    |                                                   |
              appsink (omni-capture)                       splitmuxsink (if recording)
                    |                                                   |
        EncodedFrame broadcast channel                    <camera-id>/<ts>-%05d.webm
        (one running pipeline, many                        on disk, forever, until the
         subscribers - see Supervisor)                      retention reaper deletes it
                    |
                    v  (per viewer)
        TrackLocalStaticSample.write_sample()
        (webrtc-rs, omni-webrtc)
                    |
        RTP/SRTP over ICE/DTLS  ------------>  <video> in the browser
```

Why three different libraries instead of one:

- **V4L2 (`v4l` crate)** is used *only* for device discovery/capability
  querying (`omni_capture::discover`) - "is `/dev/videoN` a real capture
  device, and what's its name". It's a thin, dependency-free way to answer
  that without spinning up a GStreamer pipeline just to enumerate cameras.
  RTSP cameras have no equivalent discovery step - they're added by URL.
- **GStreamer** owns the actual capture, encode, and (optionally) record
  pipeline (`omni_capture::pipeline`). `v4l2src`/`rtspsrc` handle the
  V4L2 ioctl dance or the RTSP SETUP/PLAY session respectively;
  `decodebin` auto-inserts whatever's needed - `jpegdec` for an MJPG UVC
  camera, an RTP depayloader + H.264 decoder for an RTSP camera, raw YUYV
  passed straight through - so *one* pipeline shape works unmodified
  across USB and RTSP sources and wildly different camera hardware;
  `vp8enc` produces browser-native video with no licensing concerns
  (unlike H.264); `splitmuxsink` writes the same encoded stream to
  segmented `.webm` files on the recording branch, so there's no second
  encode pass for recording.
- **webrtc-rs** owns *only* RTP packetization, ICE/STUN, DTLS/SRTP, and the
  SDP offer/answer state machine, for exactly one viewer at a time. It has
  no idea a V4L2 device, an RTSP session, or a recording branch exists -
  it just receives `EncodedFrame`s over a channel and calls
  `TrackLocalStaticSample::write_sample`.

A GStreamer pipeline failure (e.g. `v4l2src` finding the device already
open elsewhere, or `rtspsrc` failing to connect) does **not** surface
synchronously from `set_state()` - GStreamer reports it asynchronously on
the pipeline's bus. `omni-capture` runs a dedicated bus-watch thread per
session specifically to catch this and propagate it (via a
`tokio::sync::watch` channel) up through the supervisor to every viewer's
`omni-webrtc` session, closing their peer connections so the browser sees
a real failure instead of a silently-connected, silently-empty video tile.
This was found and fixed by testing against a real camera device that was
already held open by another process on the host - see "Known
limitations".

## One shared pipeline per camera (`omni-server::supervisor`)

A USB device only allows one process to hold it open at a time (verified
directly: a second `v4l2src` against an already-open `/dev/videoN` fails
with "device busy"). Recording and live-viewing need to run
*simultaneously* against the same camera, and more than one person might
want to view the same camera at once - so there can only be one
`CaptureSession` per camera, ever, and everything else shares it:

- Every live viewer calls `Supervisor::acquire_viewer`, which starts the
  pipeline if it isn't already running, or subscribes to the existing one
  (a `tokio::sync::broadcast` channel - each viewer gets their own
  `EncodedFrame` receiver over the *same* GStreamer pipeline) if it is.
  Dropping a viewer's `ViewerGuard` (their WebSocket closing) releases
  their slot; the pipeline stops once the last viewer is gone, unless -
- recording is enabled for that camera, in which case its pipeline is
  persistent: started at server boot and kept running regardless of
  viewers, with a `splitmuxsink` branch added to the pipeline.
- Changing a camera's settings (recording on/off, resolution, retention,
  ...) **restarts its pipeline immediately** via
  `Supervisor::restart_if_running`, so the change actually takes effect.
  Active viewers are deliberately disconnected (with a clear
  "camera settings changed; please reconnect" error) rather than left
  silently frozen - the frontend already has a retry path for this.
  Applying settings changes without dropping active viewers is future
  work (see `docs/ROADMAP.md`); it would need dynamic `tee` pad add/remove
  instead of a full pipeline restart.

Two real bugs were found by testing this against actual hardware rather
than reasoning about it in the abstract, and both are fixed in the current
code:

1. **Recording filename collisions across restarts.** `splitmuxsink`
   numbers segments from 0 within one pipeline instance. A naive
   `seg%05d.webm` pattern meant a settings-change restart's fresh instance
   would silently overwrite `seg00000.webm` from the *previous* instance -
   real recorded footage, gone. Fixed by prefixing the pattern with the
   pipeline's own start time (`<unix-seconds>-%05d.webm`), so every run
   gets a distinct, still-chronologically-sortable namespace.
2. **"Device busy" on settings-change restart.** The first version of
   `restart_if_running` started the *replacement* pipeline before stopping
   the old one, so for a USB camera the new `v4l2src` raced the still-open
   old one for the same `/dev/videoN` and reliably failed. Fixed by
   stopping the old pipeline (`CaptureSession::stop`, synchronous
   `set_state(Null)`, independent of how many `Arc` references to it still
   exist elsewhere) and giving the driver a brief moment to settle *before*
   starting the replacement.

## Recording and retention

When a camera's `recording.enabled` is set, its pipeline gains a
`splitmuxsink` branch writing fixed-length (`recording.segment_seconds`,
default 300s) `.webm` segments to
`data/recordings/<camera-id>/<run-start>-%05d.webm` - continuously,
forever. `omni-server::retention` is a background task (one pass a minute)
that, per camera with recording enabled, deletes the oldest segments once
`retention_max_age_secs` and/or `retention_max_size_bytes` is exceeded -
whichever is set (both can be; either being hit triggers a delete). It
never deletes a camera's only segment or its newest one (`splitmuxsink`
almost certainly still has that one open for writing). Enabling recording
without setting at least one retention limit is rejected at the API level
(`omni_core::validate::validate_retention`) specifically so this can't
quietly fill the disk forever.

Segment boundaries snap to the nearest VP8 keyframe (`vp8enc
keyframe-max-dist=60`, i.e. every ~2s at 30fps), so actual segment length
is typically a couple of seconds short of the configured value - fine at
the multi-minute segment lengths this is meant for.

## Signaling protocol (non-trickle ICE)

`GET/WS /api/stream/:camera_id`:

1. Browser opens the WebSocket, sends `{"type":"offer","sdp":"..."}` once
   its own ICE gathering completes (so the offer already contains all local
   candidates).
2. Server looks up the camera, acquires a viewer slot on its (possibly
   freshly-started) shared pipeline via the supervisor, creates an
   `RTCPeerConnection` + answer, waits for its own ICE gathering to
   complete, and replies `{"type":"answer","sdp":"..."}`.
3. The viewer slot and the peer connection both live exactly as long as
   the WebSocket stays open. If the underlying pipeline fails (bus error)
   or is superseded (a settings-change restart), the server sends
   `{"type":"error","message":"..."}` and closes the socket itself -
   the browser doesn't have to notice a dangling peer connection on its
   own.

Trickle ICE isn't implemented yet - full-gathering-before-send adds a
little latency (typically well under a second on a LAN) but is much
simpler. Worth revisiting if cross-NAT latency becomes a real complaint.

## HTTPS

Deliberately out of scope for early versions, per project decision: the
server is plain HTTP on port 8090. If you need TLS, put a reverse proxy
(Caddy, nginx, Tailscale, etc.) in front of it, or port-forward with your
own certificate termination. This keeps local/LAN setup friction-free,
which matters more than TLS for a device that currently has no
authentication either (see Roadmap).

## Known limitations / honest gaps in v0.2

- **No authentication.** Anyone who can reach port 8090 can view,
  reconfigure, and delete recordings for any camera. Do not expose this to
  the open internet as-is.
- **A settings change disconnects active viewers of that camera** (see
  "One shared pipeline per camera" above) rather than applying live.
- **Frame-drop-under-backpressure can transiently corrupt VP8 decode for
  live viewers.** The broadcast channel a lagging viewer falls behind on
  just skips ahead (`RecvError::Lagged`), which can show as a brief glitch
  until the next keyframe. The recording branch is unaffected - its queue
  is unbounded specifically so it never drops frames the way a live
  viewer's feed might.
- **A camera device already held open by another process** (verified
  during testing: this dev box has a container with `/dev/video0` passed
  through) will show as discoverable, but starting its stream fails with a
  clear "Device is busy" error rather than silently doing nothing.
- **No ONVIF/mDNS discovery for RTSP cameras** - they're added by URL by
  hand. No motion detection yet either. See `docs/ROADMAP.md`.
