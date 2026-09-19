# Architecture

## Goal

OmniMonitor is a self-hosted, open-source NVR (Network Video Recorder). The
pitch is two things combined:

1. A modern, Ubiquiti-Protect-style dashboard UI/UX.
2. First-class support for **USB cameras** (UVC webcams over V4L2), treated
   as full peers of network/RTSP cameras - the same live-preview and
   recording pipeline handles both.

v0.5 targets Linux only, HTTP only (no TLS - see "HTTPS" below), single-box
deployments. Default ports: **8090** for the web UI/API, **5544** for the
RTSP *server* - every camera, including USB ones, is also reachable at
`rtsp://<host>:5544/<camera-id>` for third-party NVR/VMS/player software
(see "RTSP server" below). Consuming a camera's RTSP stream as a *client*
(most of what "RTSP support" means day to day) has existed since v0.2.

## Why these technology choices

- **Rust backend.** Memory safety for something that runs unattended and
  touches untrusted network input (camera RTSP streams, browser WebRTC
  offers), plus genuinely good async I/O (tokio) for handling many camera
  streams concurrently.
- **SQLite via `sqlx`** for camera configuration and motion-event metadata.
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
                  MotionSettings, MotionEvent, AppConfig) + validation.
                  No tokio/gstreamer/etc. deps - must compile to wasm32.
  omni-db       - SQLite storage for camera config + motion events (sqlx).
  omni-capture  - V4L2 device discovery (v4l) + GStreamer capture/encode/
                  record/motion-detect pipeline ({v4l2src|rtspsrc} ->
                  decodebin -> tee(raw) -> vp8enc -> tee(encoded) ->
                  {appsink, splitmuxsink}, with the raw tee optionally
                  feeding a low-res motion-detection appsink).
  omni-webrtc   - Turns an SDP offer + a live-frame feed into a WebRTC
                  connection (webrtc-rs) for one viewer. Owns RTP/ICE/DTLS;
                  knows nothing about V4L2, RTSP, pixels, or recording.
  omni-wasm     - wasm-bindgen exports of omni-core::validate, built with
                  wasm-pack into frontend/src/lib/wasm (gitignored, built
                  artifact - see scripts/build-wasm.sh).
  omni-server   - axum HTTP/WebSocket server (the binary):
                  - routes.rs / ws.rs: REST API + WebRTC signaling.
                  - auth.rs: single-admin-account login, argon2 password
                    hashing, session-cookie middleware.
                  - supervisor.rs: owns the one running capture pipeline
                    per camera, shared across every live viewer and
                    recording, and rebuilds it on motion transitions for
                    `RecordingTrigger::Motion` (see below).
                  - motion.rs: turns a camera's raw motion-active signal
                    into logged events (SQLite) and an optional webhook
                    call.
                  - retention.rs: background reaper enforcing each
                    camera's recording retention policy.
                  - rtsp.rs: the RTSP *server* on port 5544 - every camera
                    is also a `Supervisor::acquire_viewer` caller, same as
                    a WebRTC viewer (see "RTSP server" below).
                  Serves the built frontend as static files.
frontend/       - Svelte + TypeScript + Vite. Dark, Ubiquiti-Protect-style
                  dashboard: camera grid, per-camera live WebRTC tile,
                  add/remove cameras, recording + motion-detection
                  settings, recordings browser/player, motion event log.
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
- A camera with `RecordingTrigger::Motion` gets the *same* full-pipeline
  rebuild on every motion start/stop, via
  `Supervisor::replace_pipeline`/`spawn_motion_recording_watcher` - see
  "Motion detection" below for why a live-toggled element didn't work out.

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

## Motion detection

When a camera needs motion detection - `motion.enabled`, or
`recording.trigger == Motion` (which implies it, regardless of
`motion.enabled`) - its pipeline gains a third tee branch off the *raw*
(pre-encode) video: downscaled to 160x90 grayscale at 5fps and pulled into
Rust via an `appsink`. `omni_capture::pipeline` does simple consecutive-
frame differencing there (count pixels that changed by more than a fixed
per-pixel threshold; compare the fraction against a sensitivity-derived
threshold) - no OpenCV, no ML, just enough to be genuinely useful for
"something moved in frame" at negligible CPU cost (14,400 pixels, 5
times a second). A short hold window (5s) after the last qualifying frame
keeps the reported state from flapping on brief pauses. `sensitivity`
(1-100, API/UI-facing) maps to a required "fraction of frame changed"
between 0.06 (1, needs a big change) and 0.002 (100, a tiny change
triggers it) - see `motion_required_fraction`.

The result is a `watch::Receiver<bool>` (`CaptureHandle::motion`) that two
independent consumers watch:

- `omni-server::motion::spawn_watcher` - logs each start/end to the
  `motion_events` table and, if `motion.webhook_url` is set, POSTs a
  `{"event":"motion_started",...}` JSON body on start (fire-and-forget,
  5s timeout, no retry queue - a slow/broken webhook endpoint just logs a
  warning, it doesn't block detection).
- `omni-server::supervisor::spawn_motion_recording_watcher` - only for
  `RecordingTrigger::Motion` cameras: rebuilds the pipeline (recording
  branch present or absent) on every transition, per below.

### Why motion-gated recording rebuilds the whole pipeline

The first implementation used a GStreamer `valve` between the encoded tee
and `splitmuxsink`, toggled live from the motion-detection callback -
"gate a branch that's always there" is the obvious design, and it's what
the recording-trigger UI language ("only record while motion is
detected") suggests happens under the hood. It doesn't, and getting there
took three rounds of testing against a real running pipeline (not just
inspecting the pipeline string), each surfacing a different failure that
reasoning about GStreamer's docs alone didn't predict:

1. A valve starting **closed** (`drop=true`) never lets its first buffer
   through to `splitmuxsink`, which can then never complete preroll -
   which blocks the *entire* pipeline's transition to PLAYING, not just
   the recording branch. Confirmed with a plain buffer-count probe on
   *unrelated* branches (live view, motion detection itself): zero
   buffers reached any of them, indefinitely, with no bus error and no
   indication from `set_state`'s return value that anything was wrong.
2. Starting the valve **open** and closing it immediately after
   `set_state(Playing)` returns avoids that deadlock but reintroduces a
   milder version of it: `set_state` returning doesn't mean real data has
   started flowing (RTSP negotiation/decoding takes real wall-clock time),
   so closing "immediately" typically closes it before any buffer got
   through anyway.
3. Giving the valve a real settle window (open, sleep ~500ms, then close)
   fixed *that* - but reopening it later, when motion resumed, reliably
   left `splitmuxsink` stuck with zero bytes written forever, most likely
   a keyframe/timestamp discontinuity the muxer doesn't recover from
   after a stop-start on the same element instance.

None of these are visible from reading the pipeline string or the
`valve`/`splitmuxsink` documentation in isolation - each was found by
actually recording end-to-end and checking the resulting file, not by
checking for bus errors or successful state transitions, which all three
broken versions reported cleanly.

Given that, motion-gated recording now reuses the same full-pipeline
rebuild already proven for settings changes: `Supervisor` watches the
motion signal and calls `replace_pipeline` on every transition, which
tears down the old pipeline and starts a fresh one with the recording
branch present or absent to match. The fresh pipeline's *internal* motion
state has to be seeded from the state it was rebuilt for
(`MotionConfig::initial_active`) - without that, a freshly-rebuilt
"recording" pipeline starts assuming "no motion yet", immediately
re-detects the still-ongoing motion as a new transition, and asks for
*another* rebuild, in a tight loop (caught by testing, not reasoned out -
it was rebuilding several times a second).

**Known trade-offs of this approach**, both accepted rather than solved
for v0.3:

- Active viewers of a `RecordingTrigger::Motion` camera get disconnected
  on *every* motion transition, not just on an explicit settings change -
  it's the same restart machinery, so it has the same effect. The
  frontend's retry path covers it, but it's a rougher experience than
  gapless would be.
- The per-pipeline-instance event watcher means a motion event that's
  still open when a rebuild happens (which motion-triggered recording
  causes on essentially every "motion starts" transition) gets closed by
  the *old* pipeline's watcher tearing down and immediately reopened as a
  *new* event by the incoming pipeline - so the `motion_events` log for a
  `RecordingTrigger::Motion` camera can show a spurious near-zero-duration
  event around each transition instead of one continuous one. The
  recording itself (the file on disk) is unaffected; this is a logging
  fidelity issue only, specific to combining motion-gated recording with
  the events feature. Fixing it needs event-open/close tracking to live
  above the per-pipeline-instance watcher (keyed by camera, surviving
  rebuilds) - not done yet, see `docs/ROADMAP.md`.

Applying motion transitions (and settings changes generally) without
rebuilding the whole pipeline is the same "dynamic `tee` pad add/remove"
future work mentioned above for plain settings changes.

## RTSP server

Every camera - including USB ones - is reachable at
`rtsp://<host>:5544/<camera-id>` for any RTSP-capable third-party
software (VLC, other NVR/VMS tools, `ffprobe`/`ffmpeg`). This is the
other half of "USB cameras act like network cameras": the first half
(consuming an RTSP camera as a client) existed since v0.2; this is
OmniMonitor acting as the *server* side for all of its cameras, USB
included.

`omni-server::rtsp` wraps `gstreamer-rtsp-server`, which needs its own
GLib main loop - blocking, so it runs on a dedicated OS thread, separate
from the tokio runtime the rest of the server uses. Each camera gets a
`GstRTSPMediaFactory` whose launch string is just
`appsrc name=src ! rtpvp8pay name=pay0 pt=96`: when an RTSP client
connects, the factory's `media-configure` callback (firing on the GLib
thread) finds that session's `appsrc` and hands off to the tokio runtime
via `Handle::spawn` - the RTSP client becomes just another
`Supervisor::acquire_viewer` caller, indistinguishable at that layer from
a WebRTC viewer. Frames already VP8-encoded by the one running
`CaptureSession` for that camera are pushed into the `appsrc` as they
arrive; no second encode pass, and for a USB camera, no second device
open (which would just fail with "device busy" against the same
`/dev/videoN` the primary pipeline already holds).

Verified against real RTSP clients, not just "no errors reported":
`gst-launch-1.0 rtspsrc` decoding VP8 back out, `ffprobe` independently
reporting the correct codec/resolution over an RTSP session it initiated
itself, and a USB camera served to an RTSP client and a WebRTC viewer at
the same time with neither affecting the other.

Mount points are registered/removed as cameras are added/deleted (and
re-registered idempotently on `/api/cameras/discover`) - not tied to
recording or motion settings, since registering one is cheap: it doesn't
start a pipeline until an RTSP client actually connects.

### RTSP server authentication (v0.5.1)

Every mount point requires HTTP Basic auth via `GstRTSPAuth` - a client
with no or wrong credentials gets a bare `401 Unauthorized` on
`DESCRIBE`, verified against real `ffprobe`/`gst-launch-1.0 rtspsrc`
sessions (wrong password, no password, and correct password all tested).

The single RTSP credential (username `rtsp`, random password unless
`OMNI_RTSP_PASSWORD` is set - same bootstrap pattern as the HTTP admin
account, see "Authentication" below) is stored **in plaintext** in
`rtsp_credentials` (`omni-db`), unlike the HTTP admin account's one-way
argon2 hash. This is a deliberate, unavoidable difference: `GstRTSPAuth`'s
Basic-auth mechanism needs the plaintext credential at server-start time
to build its base64 `add_basic()` token - there's no server-side
"verify against a hash" hook to use instead. It's viewable/reset from the
UI ("RTSP credentials" in the sidebar) or `GET /api/rtsp-credentials`
(session-gated, same as every other `/api/*` route).

Getting the Rust API right took an empirical detour: the Rust bindings
only expose `RTSPMediaFactoryExtManual::add_role_from_structure` (not the
C API's `add_role`/`RTSPPermissions` convenience wrappers), and a
`gst::Structure` field set via a `glib::Variant`-wrapped bool silently
produces a `404` on access instead of the expected `401`/success - it
needs a plain Rust `bool` passed straight to `.field(...)`. This was
confirmed with a standalone Python GI script before writing the Rust
version, following the same "verify the exact mechanism outside Rust
first" discipline established during the motion-detection valve
debugging (see above) - swapping the Variant-wrapped bool for a plain
one was the one-line fix that made it work.

## Signaling protocol (trickle ICE, since v0.6)

`GET/WS /api/stream/:camera_id`:

1. Browser opens the WebSocket, creates an offer, and sends
   `{"type":"offer","sdp":"..."}` immediately after `setLocalDescription`
   - it does not wait for its own ICE gathering to finish.
2. Server looks up the camera, acquires a viewer slot on its (possibly
   freshly-started) shared pipeline via the supervisor, creates an
   `RTCPeerConnection` + answer, and replies
   `{"type":"answer","sdp":"..."}` as soon as the local description is
   set - likewise without waiting for gathering.
3. Both sides trickle `{"type":"ice_candidate","candidate":{...}}`
   messages as candidates are discovered, for the life of the socket.
   `omni_webrtc::StreamSession::start` returns an
   `mpsc::UnboundedReceiver<RTCIceCandidateInit>` fed by
   `on_ice_candidate`, which `omni-server::ws` forwards to the browser;
   incoming candidates from the browser go straight to
   `StreamSession::add_ice_candidate`. A browser candidate that arrives
   before the server's session exists yet (a race that's possible since
   nothing stops the browser from trickling immediately after sending the
   offer) is queued and applied once the session starts.
4. The viewer slot and the peer connection both live exactly as long as
   the WebSocket stays open. If the underlying pipeline fails (bus error)
   or is superseded (a settings-change restart), the server sends
   `{"type":"error","message":"..."}` and closes the socket itself -
   the browser doesn't have to notice a dangling peer connection on its
   own.

This replaces the earlier "wait for full ICE gathering before sending
either side's SDP" approach (simpler, but added up to a second of latency
before the connection could even start negotiating) - worth it now that
the mechanism is proven end-to-end elsewhere in the project (RTSP,
motion detection) via the same "verify real behavior, not just
successful compiles" discipline.

## ONVIF network camera discovery (v0.7)

`omni-server::onvif_discovery` sends a WS-Discovery `Probe` (the
multicast SOAP-over-UDP protocol ONVIF devices use to announce
themselves) to `239.255.255.250:3702` and collects `ProbeMatch` replies
for a fixed 3-second window, deduplicated by source IP. No new
dependency was needed - `tokio::net::UdpSocket` (already available via
the `tokio "full"` feature set) handles the multicast join/send/receive
directly, and XAddrs are pulled out of the raw SOAP reply with a small
string scan rather than a full XML parser, matching on the unprefixed
local name (`XAddrs>`) since real cameras use inconsistent namespace
prefixes (`d:XAddrs`, `wsdd:XAddrs`, `a:XAddrs`, ...).

This deliberately stops at "here's an IP address and ONVIF device
service URL" - going further to ask the camera for its actual RTSP
stream URI needs an authenticated `GetStreamUri` SOAP call against that
device service, which needs per-camera credentials OmniMonitor doesn't
have at discovery time. So a discovered result just prefills the host in
"+ Add camera"'s RTSP URL field; the user still fills in the stream path
and any camera credentials themselves, the same as adding one by hand
today - this saves the "find the camera's IP" step (previously: check a
router's DHCP client list or run a separate network scanner), not the
whole flow.

Multicast doesn't cross routers, so this only ever finds cameras on the
same LAN segment as the server - expected and fine for the target
use case (a home/small-site NVR on one local network).

## Authentication

Single admin account, session cookie, deliberately no more than that for
now (see `docs/ROADMAP.md` for multi-user/per-camera permissions as
future work). `omni-server::auth`:

- On first boot, if no account exists, one is created - `admin` plus
  `OMNI_ADMIN_PASSWORD` if that env var is set, otherwise a random
  password printed to the log exactly once (the standard self-hosted-app
  pattern: there's no sensible default password to ship, and prompting
  interactively doesn't fit a service that's meant to just start).
- Passwords are hashed with argon2 (`argon2` crate, default parameters).
- A successful login gets an opaque random token (not a JWT - there's
  nothing to encode beyond "this token is valid", so a signed/stateless
  token would just be extra complexity) stored in a `sessions` table and
  set as an `HttpOnly`, `SameSite=Lax` cookie. No `Secure` flag - this
  server is HTTP-only by design (see below), and `Secure` would make the
  cookie silently stop being sent at all over plain HTTP.
- `axum::middleware::from_fn_with_state` (`auth::require_auth`) wraps
  every `/api/*` route except `/api/auth/login` - see `routes::api_routes`
  for how the router is split into a public and a protected half via
  `route_layer`, which (unlike `.layer`) only wraps routes registered
  before it in the same `Router`, not routes merged in afterward. The
  static frontend files (`GET /`, JS/CSS/wasm) are deliberately **not**
  gated, so the login screen itself can load; the API calls it makes are
  what's actually protected.
- Sessions last 30 days from creation (no sliding-window renewal) and are
  swept lazily - an expired-but-not-yet-deleted token is already rejected
  by the validity check on every request, the periodic sweep just
  reclaims the row.

### Login rate limiting (v0.8)

`auth::LoginRateLimiter` throttles `/api/auth/login` per source IP -
the only endpoint reachable without a session, so the only one worth
protecting this way. The first 3 failures from an IP are free (typos
happen); each failure after that locks that IP out for `2^n` seconds
(2, 4, 8, ... capped at 5 minutes), reset immediately on a successful
login. An in-memory `HashMap<IpAddr, _>` behind a `Mutex` - no
persistence needed, a restart clearing everyone's lockout state is an
acceptable tradeoff for a single-process app, and a background sweep
(hourly) forgets entries idle for over an hour so the map doesn't grow
unbounded against a scanner hitting many distinct source addresses.

The IP comes from `axum::extract::ConnectInfo`, wired in by starting the
server with `into_make_service_with_connect_info::<SocketAddr>()` -
which means it's the **TCP peer address**, not necessarily the real
client if OmniMonitor runs behind a reverse proxy (see "HTTPS" below):
in that setup every request looks like it comes from the proxy, so the
limiter's per-IP tracking degrades to "shared across everyone behind
it." Verified end-to-end against a live server: repeated wrong passwords
correctly return `429` with an increasing retry-after, and a correct
password is accepted immediately once the lockout window elapses.

## HTTPS

Deliberately out of scope for the server itself, per project decision:
it's plain HTTP on port 8090, no TLS built in. `packaging/` ships
ready-to-edit reverse-proxy configs (`Caddyfile.example`,
`nginx.conf.example`) instead - Caddy gets a Let's Encrypt certificate
automatically from a real DNS name, nginx expects you (or `certbot`) to
provide one. This keeps local/LAN setup friction-free, which matters
more than TLS for a device most people run on a trusted network; put a
reverse proxy in front of it (or a VPN/tunnel) the moment that stops
being true. Only the HTTP API/UI can go through an HTTP(S) reverse proxy
this way - the RTSP server (5544, see above) isn't HTTP, so exposing it
past a LAN needs a TLS-capable TCP proxy (`stunnel`) or a VPN instead.

## Security review fixes (v0.8.1)

An external code review of the v0.8 tree (reading the source, not
running it) surfaced several real issues, addressed here in order of
what it rated most severe:

- **GStreamer pipeline injection via RTSP URL, fixed.** The RTSP
  camera's URL used to be formatted directly into the `gst::parse::
  launch` description string (`rtspsrc location="{url}" ...`) -
  `validate_rtsp_url` only checked for an `rtsp://` prefix, not the
  absence of a `"`, so a URL containing one could break out of the
  quoted property and append arbitrary pipeline elements (e.g. a
  `filesink` writing files as the service user). Only an authenticated
  admin could reach this (camera URLs are only ever set via the
  session-gated `POST /api/cameras`), so it was a privilege-widening
  bug, not an anonymous remote one - but still the most serious finding.
  Fixed in `omni-capture::pipeline` by never interpolating the URL into
  the launch string at all: `CaptureSource::Rtsp` gets a named,
  property-less `rtspsrc` in the description, and `CaptureSession::start`
  sets `location` on it afterward via `Element::set_property` - a typed
  GObject property setter, not a string that gets parsed as gst-launch
  syntax. Verified against a real RTSP test server both that normal
  URLs still capture frames and that a URL crafted to break out
  (containing `" ! filesink location=...`) no longer creates the target
  file - `rtspsrc` just fails to connect to the literal (now-harmless)
  string instead.
- **SSRF via the motion webhook, mitigated.** `validate_webhook_url`
  (shared with the browser via `omni-wasm`, so it can only ever check
  the URL's *shape*, not resolve it) only checked for an `http(s)://`
  prefix, so a webhook could be pointed at `127.0.0.1`, a cloud
  metadata endpoint, or anywhere else the server can reach. Since this
  requires an authenticated admin to configure, and the actual point of
  the feature is notifying a home-automation box that's typically *on
  the same LAN* as the cameras, a blanket "no private IPs" rule would
  break the intended use case. `omni-server::motion::send_webhook`
  instead resolves the URL's host right before sending and rejects
  loopback/link-local/unspecified destinations specifically (which
  covers the cloud-metadata and "reach the server itself" cases) while
  still allowing ordinary RFC1918 LAN addresses through, and disables
  HTTP redirect-following so a webhook that starts out pointing
  somewhere allowed can't 302 the request elsewhere. Not airtight
  against DNS rebinding between the resolve-time check and the actual
  connect (that would need a custom `reqwest` resolver/connector
  hooking the TCP connect itself), but closes the straightforward case.
- **No CORS layer.** `CorsLayer::permissive()` was applied globally but
  did nothing useful: the frontend is always served by this same
  process (or proxied to it by Vite in dev - see
  `frontend/vite.config.ts`), so every legitimate request is
  same-origin already. Removed entirely rather than tuned, since
  nothing needs it.
- **Cross-site WebSocket hijacking, mitigated.** The session-cookie
  check (`require_auth`) wrapping `/api/stream/:camera_id` isn't enough
  on its own: unlike a `fetch()`, a browser's WebSocket handshake
  attaches cookies regardless of which site's JavaScript opened it, so
  a malicious page could open a WS connection here and ride the
  victim's session. `omni-server::ws::stream_ws_handler` now checks the
  handshake's `Origin` header against its own `Host` and rejects the
  upgrade with `403` on a mismatch (no `Origin` header at all - not
  possible for a real cross-site browser request - is let through, same
  as a same-origin request). Verified live: a mismatched-`Origin`
  handshake gets `403`, a matching one still completes the normal `101
  Switching Protocols` upgrade.
- **Session tokens hashed at rest.** `sessions.token` used to store the
  bearer token verbatim; a leaked DB file (backup, misconfigured
  permissions) would hand out immediately-usable 30-day sessions.
  `omni-db` now stores/looks up `SHA-256(token)` instead - fine here
  even unsalted, since the input is already a 48-character
  cryptographically random token (`auth::generate_token`), not a
  human-chosen secret, so there's no dictionary attack to defend
  against; this only closes the "bulk DB leak yields live sessions"
  case. One consequence: upgrading past this version invalidates every
  session that existed before it (old plaintext rows can't match a
  hash lookup) - a one-time forced re-login, not a bug.
- **Password change now revokes other sessions.** Previously, changing
  the admin password (the standard response to "this password might be
  compromised") left every other already-issued session valid for the
  rest of its normal 30-day life. `auth_change_password` now calls
  `Db::delete_sessions_except` with the session making the request, so
  every *other* session is invalidated immediately while the user isn't
  logged out by their own change. Verified live with two concurrent
  sessions: changing the password via one leaves it valid and gets the
  other a `401` on its next request.
- **Argon2 moved off the async runtime.** `verify_password`/
  `hash_password` are deliberately slow (that's the point of a password
  hash) and were being called inline inside `auth_login`/
  `auth_change_password`'s async handlers, tying up a tokio worker
  thread for the duration. `auth::verify_password_async`/
  `hash_password_async` now run them via `tokio::task::spawn_blocking`
  instead, so a burst of login attempts can't starve unrelated requests
  on a small worker pool.
- **`internal_error` no longer echoes internal detail to the client.**
  It used to return `err.to_string()` (DB error text, file paths, ...)
  as the response body for any unexpected server-side failure -
  `bad_request` (used for `ValidationError`s meant to be shown to the
  user) is unaffected. `internal_error` now logs the real error via
  `tracing::error!` and returns a fixed "internal server error" message.
- **`Secure` cookie support**, opt-in via `OMNI_COOKIE_SECURE=1` for
  anyone running behind a TLS-terminating reverse proxy (see "HTTPS"
  below) - left off by default since the server itself still only ever
  speaks plain HTTP, and `Secure` would make the browser silently stop
  sending the cookie at all in that default setup.
- **Resolution upper bound.** `validate_resolution` only rejected zero
  in either dimension; a malformed or malicious `PATCH /api/cameras/:id`
  could request an arbitrarily large resolution and have the pipeline
  try to allocate buffers for it. Capped at 7680x4320 (8K) - no real
  camera exceeds this.
- **`cargo audit` added to CI** (`.github/workflows/ci.yml`), checking
  `Cargo.lock` against the RustSec advisory database on every push/PR.
  Running it locally for the first time surfaced two real things to
  fix: `sqlx` was declared with its *default* features on top of the
  explicit `sqlite` one, silently pulling in the unused MySQL and
  Postgres drivers (and, via MySQL's auth plugin, the `rsa` crate) into
  the dependency graph and, for MySQL/Postgres, the actual compiled
  binary too, confirmed by checking `target/debug/deps` before and after
  - now `default-features = false` with only `sqlite`/`macros`/etc.
  explicitly listed. Separately, `sqlx` 0.7.4 has a real fixed advisory
  (RUSTSEC-2024-0363, a wire-protocol decoding bug in the MySQL/Postgres
  decoders this project doesn't even compile) - bumped to `0.8` anyway
  since a version with a real fix is simpler and more honest than
  arguing the unreachable code doesn't matter; verified compiling
  cleanly and re-tested the full camera CRUD path (create/list/update/
  delete) plus login/sessions against the existing SQLite database
  afterward. What's left, `RUSTSEC-2023-0071` (`rsa`, no fix available
  upstream, reachable only via the still-`Cargo.lock`-listed-but-never-
  compiled `sqlx-mysql`), is explicitly `ignore`d in the CI step with
  the reasoning inline - `cargo audit` scans Cargo.lock's full dependency
  graph, not per-feature reachability, so an unactivated optional
  dependency can still show up there with no way to make it disappear
  short of dropping `sqlx` entirely.
- **Extra systemd sandboxing** (`packaging/omnimonitor.service`):
  `RestrictAddressFamilies`, `RestrictNamespaces`, `LockPersonality`,
  `MemoryDenyWriteExecute`, `ProtectKernelTunables`/`Modules`/`Logs`,
  `ProtectControlGroups`, `ProtectClock`, and `SystemCallFilter=
  @system-service`. Deliberately does **not** add `PrivateDevices=true`
  (a common hardening suggestion) - that blocks all of `/dev`, including
  the `/dev/videoN` nodes USB capture depends on, which would silently
  break the core feature.
  - These flags were reasoned about from what the service needs, not
    verified against a live systemd instance when first written - and
    that gap turned up a real bug once someone actually ran it:
    `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6` (no `AF_NETLINK`)
    made every WebRTC live view hang at "connecting" forever. SDP
    negotiation succeeded, but `webrtc-rs`'s ICE agent enumerates local
    network interfaces via `getifaddrs()` to gather host candidates,
    which talks to the kernel over an `AF_NETLINK` socket under the hood
    on Linux - blocked, every such `socket()` call fails with
    `EAFNOSUPPORT` (errno 97), and ICE gathers zero local candidates.
    Diagnosed from `journalctl -u omnimonitor` showing "Address family
    not supported by protocol (os error 97)" during candidate gathering,
    with `RestrictAddressFamilies` the only thing in the unit that could
    produce that specific error for a syscall the unsandboxed dev binary
    made successfully every time. Fixed by adding `AF_NETLINK` to the
    allow-list. Run `systemd-analyze security omnimonitor` after
    installing to sanity-check the rest of these.

Reviewed and deliberately left as-is, with reasoning:

- **Login rate limiting is keyed by TCP peer address**, which is the
  reverse proxy's address for anyone following the "HTTPS" setup below -
  already documented in "Login rate limiting" above; there's no fix
  that doesn't either trust a spoofable `X-Forwarded-For` header or
  require configuring which proxy to trust, which is a bigger feature
  than this pass was scoped for.
- **RTSP camera URLs (which may embed credentials) are stored and
  returned as-is by `GET /api/cameras`.** Same trust boundary as the
  RTSP/admin credentials already exposed by other session-gated
  endpoints (`GET /api/rtsp-credentials`) - not a new exposure, just
  worth naming. Redacting it while still allowing the settings form to
  edit other fields without re-entering the URL is a real feature, not
  a one-line fix; left for later.
- **`curl | sh` in `scripts/bootstrap.sh`** for rustup/wasm-pack is each
  tool's own officially documented install method, not a shortcut this
  project invented.

## Dashboard UX: rotation, reordering, zoom, USB device management (v0.9)

- **Rotation** (`Camera::rotation`) is applied in the capture pipeline
  itself via GStreamer's `videoflip`, inserted right after `decodebin`
  and before the `videoscale` that fits the result to the camera's
  configured width/height - so the final output is always exactly that
  configured resolution with the rotated content inside it, and rotation
  affects the live view, recordings, and the RTSP re-serve identically
  (it's one shared pipeline, see "One shared pipeline per camera" above).
  `videoflip`'s `method` property (`identity`/`clockwise`/`rotate-180`/
  `counterclockwise`) maps directly from the four `Rotation` variants.

  Verifying this surfaced a real, separate, pre-existing bug: the RTSP
  server's `RTSPMediaFactory::connect_media_configure` callback closes
  over the `Camera` value passed to `RtspServer::add_camera` **by value**
  at registration time, and reuses that same snapshot for every future
  RTSP session on that mount - it never re-fetches from the database.
  `update_camera` (`PATCH /api/cameras/:id`) was updating the DB and the
  in-memory `Supervisor` state correctly, but never told the RTSP server
  about the change, so *any* settings change (not just rotation) was
  invisible to RTSP clients connecting after the change, until something
  else (e.g. `/api/cameras/discover`) happened to re-register every mount
  point anyway. Fixed by having `update_camera` call
  `state.rtsp_server.add_camera(...)` again after every update, same as
  `create_camera`/`delete_camera`/`discover_cameras` already did.

- **Manual dashboard ordering** (`Camera::sort_order`) is a plain integer
  column, reassigned in one transaction by `Db::reorder_cameras` from the
  full new id order the frontend's drag-and-drop produces -
  `PUT /api/cameras/reorder` rejects (400) any request that isn't
  exactly the current set of camera ids, so a partial/stale list can't
  silently leave some cameras with a `sort_order` that no longer means
  what the caller thought it meant. New cameras get
  `Db::next_sort_order()` (max existing + 1) instead of defaulting to
  `0`, so they append at the end of the grid instead of jumping to the
  front of an already-reordered list.

- **Click-to-expand + live-view zoom**: `ExpandedCameraModal` opens a
  second, independent `Supervisor::acquire_viewer` slot for the same
  camera (the shared-pipeline design means this is cheap - no second
  device open or encode pass, same as two browser tabs already worked
  before this). Zoom/pan is a CSS `transform: scale()/translate()` on
  the `<video>` element, entirely browser-side - it doesn't touch the
  pipeline, so it has no effect on recordings or the RTSP output, only
  on what that one viewer happens to be looking at.

  The WebRTC/trickle-ICE connection logic (non-trivial - see "Signaling
  protocol" above) was extracted from `CameraTile` into
  `frontend/src/lib/webrtc-view.ts` so `ExpandedCameraModal` could reuse
  the exact same tested logic instead of a second copy that could drift.

- **Permanently ignoring a USB device**: deleting a USB camera previously
  didn't stick - `auto_discover_usb_cameras` runs on every restart and
  via "Rescan USB cameras", and would just see the still-plugged-in
  device as unknown again and re-add it, since `usb_camera_exists`
  only checks the *current* `cameras` table. A new `ignored_usb_devices`
  table (keyed by device path, same identity scheme USB cameras already
  use) is checked alongside it; `delete_camera` inserts into it
  automatically for a USB camera (an RTSP camera's deletion is already
  permanent, since it's never auto-discovered in the first place).
  Reversible from "Ignored USB devices" in the sidebar
  (`Db::unignore_usb_device`) - the device becomes eligible for
  discovery again but isn't re-added until the next actual rescan.

## Recording timeline, camera groups, status overview (v0.10)

- **Recording timeline placement is approximate, on purpose.**
  `splitmuxsink` never records each segment's exact start time anywhere
  (not in the filename - `<run-started-unix>-%05d.webm` only encodes
  when the *pipeline run* started, not each fragment - and not in any
  sidecar metadata). `RecordingInfo.started_at` is computed as
  `file_mtime - segment_seconds` in `list_recordings`. This is exact for
  a segment that's already finalized (its mtime stops advancing once
  `splitmuxsink` rolls over to the next file, sitting at the moment the
  last byte was written - subtracting the nominal segment length back
  out from that gives the real start), but is *not* exact for whichever
  segment is currently still being written (its mtime keeps advancing
  toward "now" as data streams in, rather than jumping to a stable
  "finished" value) - confirmed by checking a real in-progress segment's
  mtime during testing rather than assuming. `RecordingTimeline.svelte`
  places blocks using this value; clicking one seeks to
  `clicked_time - segment.started_at` seconds into that file. The exact
  fix (having the capture pipeline itself record each fragment's real
  start, e.g. via `splitmuxsink`'s `format-location-full` signal, into a
  `recording_segments` table) is real additional work - a new signal
  handler wired into `omni-capture::pipeline`, DB writes on every
  fragment boundary, and keeping the retention reaper's direct
  filesystem deletes in sync with that table - deferred until the
  approximation actually causes a problem in practice.

- **Camera groups are a plain string, not a normalized table.**
  `Camera::group: Option<String>` - nothing else in the schema needs to
  reference a group by id (no per-group settings, no group-level
  permissions), so a `groups` table would only add a join for no benefit.
  The frontend's group tabs are just `[...new Set(cameras.map(c =>
  c.group))]` - a group "exists" exactly when some camera currently has
  that string set, and stops existing the moment the last camera with it
  is moved out or deleted. Renaming a tab
  (`POST /api/camera-groups/rename`, `Db::rename_camera_group`) is a
  single `UPDATE cameras SET camera_group = ? WHERE camera_group = ?`,
  not a cascade - there's nothing else that could reference the old name.

- **Status overview trades exactness for cost, deliberately, for an idle
  camera.** A camera with a pipeline currently running gets an exact
  answer for free (`Supervisor::pipeline_status` just reads the same
  capture-error watch channel viewers already watch). A camera with no
  pipeline running - the common case for anything that isn't recording,
  motion-detecting, or currently being watched - would need the status
  page to actually open the device or connect to it to know anything for
  certain, which this deliberately doesn't do (spinning up a real capture
  pipeline just to populate a status table would be far more invasive
  than the question deserves, and for a USB camera would contend with
  whatever else might have it open - see the "device busy" issue noted
  in v0.9's testing). Instead `reachability::probe_reachable` does the
  cheapest presence check available per camera kind: a USB device's
  `stat()` (does `/dev/videoN` still exist), or a plain TCP connect with
  a 2-second timeout for an RTSP camera's host:port. Neither confirms the
  camera will actually *stream* successfully - a wedged UVC device or an
  RTSP host that accepts TCP connections but rejects the RTSP handshake
  both read as "online." Good enough to answer "is this camera even
  there," not a substitute for actually trying to view it.

## Known limitations / honest gaps in v0.10

- **Single admin account and single RTSP credential, not
  per-camera or per-user permissions** - every camera's RTSP mount point
  grants the same "user" role to the same Basic-auth credential. Good
  enough to keep it off a trusted LAN's uninvited guests; not
  fine-grained, and Basic auth over RTSP isn't encrypted (see "HTTPS"
  above) - don't expose port 5544 to the open internet as-is.
- **A settings change disconnects active viewers of that camera** (see
  "One shared pipeline per camera" above) rather than applying live -
  and for `RecordingTrigger::Motion` cameras, this now also happens on
  every motion start/stop, not just an explicit settings change (see
  "Motion detection" above).
- **The motion-events log can show spurious near-zero-duration entries
  around each transition for a `RecordingTrigger::Motion` camera**
  specifically (event-open/close tracking doesn't yet survive the
  pipeline rebuilds that trigger drives) - see "Motion detection" above.
  Standalone motion detection (`motion.enabled` without motion-gated
  recording) doesn't have this problem, since nothing rebuilds the
  pipeline on a plain motion transition there.
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
  hand. See `docs/ROADMAP.md`.
