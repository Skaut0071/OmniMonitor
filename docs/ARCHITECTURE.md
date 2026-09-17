# Architecture

## Goal

OmniMonitor is a self-hosted, open-source NVR (Network Video Recorder). The
pitch is two things combined:

1. A modern, Ubiquiti-Protect-style dashboard UI/UX.
2. First-class support for **USB cameras** (UVC webcams over V4L2), treated
   as full peers of network/RTSP cameras rather than a bolted-on afterthought
   - the same live-preview, and eventually recording, pipeline handles both.

v0.1 targets Linux only, HTTP only (no TLS - see "HTTPS" below), single-box
deployments. Default ports: **8090** for the web UI/API, **5544** for RTSP
(reserved for a future milestone, not implemented yet).

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
- **V4L2 (`v4l` crate) for device discovery**, **GStreamer for capture +
  encode**, **webrtc-rs for the browser transport.** See the capture
  pipeline section below for why these three specifically, and why they're
  not redundant with each other.
- **Svelte + TypeScript (Vite) frontend**, with a small **Rust -> WASM**
  module (`omni-wasm`, built via `wasm-pack`) for logic that must be
  identical on both sides of the wire. Browser-native APIs
  (`RTCPeerConnection`, `<video>`, WebSocket) are used directly from
  TypeScript - there is no value in fighting those through a Rust wasm
  binding layer. What *is* valuable: **validation rules that must match the
  server exactly** (e.g. "camera name must be 1-64 chars"). Those rules
  live once, in `omni-core::validate`, compiled twice - natively into
  `omni-server`, and to wasm into the admin UI - so the two can never drift.
  This is intentionally a small, targeted use of wasm rather than "the
  whole frontend is Rust": that would fight the browser's own APIs for no
  benefit at this stage.

## Workspace layout

```
crates/
  omni-core     - shared types (Camera, CameraKind, AppConfig) + validation.
                  No tokio/gstreamer/etc. deps - must compile to wasm32.
  omni-db       - SQLite storage for camera config (sqlx).
  omni-capture  - V4L2 device discovery (v4l) + GStreamer capture/encode
                  pipeline (v4l2src -> decodebin -> vp8enc -> appsink).
  omni-webrtc   - Turns an SDP offer + a running capture session into a
                  live WebRTC connection (webrtc-rs). Owns RTP/ICE/DTLS;
                  knows nothing about V4L2 or pixels.
  omni-wasm     - wasm-bindgen exports of omni-core::validate, built with
                  wasm-pack into frontend/src/lib/wasm (gitignored, built
                  artifact - see scripts/build-wasm.sh).
  omni-server   - axum HTTP/WebSocket server (the binary). REST API for
                  camera CRUD, WebSocket signaling for WebRTC, serves the
                  built frontend as static files.
frontend/       - Svelte + TypeScript + Vite. Dark, Ubiquiti-Protect-style
                  dashboard: camera grid, per-camera live WebRTC tile,
                  add/remove cameras.
```

## The USB-camera-as-network-camera capture pipeline

This is the core technical bet of the project, so it's worth spelling out
end to end for `/dev/video0`, a UVC webcam, arriving in a browser tab:

```
V4L2 device            GStreamer pipeline                  WebRTC          Browser
/dev/video0    ---->   v4l2src -> decodebin -> videoconvert -> videoscale
                        -> videorate -> vp8enc -> appsink
                                              |
                                    EncodedFrame { data, keyframe, duration }
                                    (tokio mpsc channel, omni-capture)
                                              |
                                              v
                                    TrackLocalStaticSample.write_sample()
                                    (webrtc-rs, omni-webrtc)
                                              |
                                    RTP/SRTP over ICE/DTLS  ------------>  <video>
```

Why three different libraries instead of one:

- **V4L2 (`v4l` crate)** is used *only* for device discovery/capability
  querying (`omni_capture::discover`) - "is `/dev/videoN` a real capture
  device, and what's its name". It's a thin, dependency-free way to answer
  that without spinning up a GStreamer pipeline just to enumerate cameras.
- **GStreamer** owns the actual capture + encode pipeline
  (`omni_capture::pipeline`). `v4l2src` handles the V4L2 ioctl dance;
  `decodebin` auto-inserts `jpegdec` when the camera's native format is
  MJPG (very common on UVC webcams above ~720p) and passes raw YUYV through
  untouched otherwise - so the same pipeline string works unmodified across
  wildly different camera hardware; `vp8enc` produces browser-native video
  with no licensing concerns (unlike H.264).
- **webrtc-rs** owns *only* RTP packetization, ICE/STUN, DTLS/SRTP, and the
  SDP offer/answer state machine. It has no idea a V4L2 device exists - it
  just receives `EncodedFrame`s over a channel and calls
  `TrackLocalStaticSample::write_sample`. This split is what makes an RTSP
  camera a drop-in future replacement for a USB one: same
  `EncodedFrame` channel, same `omni-webrtc` crate, different producer.

A GStreamer pipeline failure (e.g. `v4l2src` finding the device already
open elsewhere) does **not** surface synchronously from `set_state()` -
GStreamer reports it asynchronously on the pipeline's bus. `omni-capture`
runs a dedicated bus-watch thread per session specifically to catch this and
propagate it (via a `tokio::sync::watch` channel) to `omni-webrtc`, which
then closes the peer connection so the browser sees a real failure instead
of a silently-connected, silently-empty video tile. This was found and
fixed by testing against a real camera device that was already held open by
another process on the host - see the "Known limitations" section.

## Signaling protocol (v0.1, non-trickle ICE)

`GET/WS /api/stream/:camera_id`:

1. Browser opens the WebSocket, sends `{"type":"offer","sdp":"..."}` once
   its own ICE gathering completes (so the offer already contains all local
   candidates).
2. Server looks up the camera, starts a `CaptureSession` for it, creates an
   `RTCPeerConnection` + answer, waits for its own ICE gathering to
   complete, and replies `{"type":"answer","sdp":"..."}`.
3. Both the capture pipeline and the peer connection live exactly as long
   as the WebSocket stays open. Closing the tab / the WS tears both down.
4. `{"type":"error","message":"..."}` is sent (and the socket closed) if
   the camera doesn't exist, is an RTSP camera (not implemented yet), or
   the capture pipeline fails to start or dies later.

Trickle ICE isn't implemented yet - full-gathering-before-send adds a
little latency (typically well under a second on a LAN) but is much
simpler. Worth revisiting if cross-NAT latency becomes a real complaint.

## HTTPS

Deliberately out of scope for early versions, per project decision: the
server is plain HTTP on port 8090. If you need TLS, put a reverse proxy
(Caddy, nginx, Tailscale, etc.) in front of it, or port-forward with your
own certificate termination. This keeps local/LAN setup friction-free,
which matters more than TLS for a device that in v0.1 has no
authentication either (see Roadmap).

## Known limitations / honest gaps in v0.1

- **No authentication.** Anyone who can reach port 8090 can view and
  reconfigure cameras. Do not expose this to the open internet as-is.
- **No recording yet.** Only live preview. See `docs/ROADMAP.md`.
- **RTSP camera capture isn't implemented.** You can add an RTSP camera's
  metadata via the API/UI, but requesting its stream returns
  "not implemented yet" - only USB/V4L2 capture is wired up so far.
- **Frame-drop-under-backpressure can transiently corrupt VP8 decode.**
  `omni-capture`'s appsink channel drops individual frames (not
  necessarily on a keyframe boundary) if the WebRTC consumer is slow; the
  stream self-heals at the next keyframe (currently every ~2s at 30fps).
  Noted as a `TODO` in `crates/omni-capture/src/pipeline.rs`.
- **A camera device already held open by another process** (verified
  during testing: this dev box has a container with `/dev/video0` passed
  through) will show as discoverable, but starting its stream fails with a
  clear "Device is busy" error rather than silently doing nothing - that
  failure mode is exactly what the bus-watch fix above addresses.
