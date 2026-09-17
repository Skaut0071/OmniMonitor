# Roadmap

Rough, in priority order. Nothing here is a commitment/date - it's the
order the project intends to tackle things.

## v0.1 (this milestone) - done

- [x] Rust workspace: `omni-core`, `omni-db`, `omni-capture`, `omni-webrtc`,
      `omni-wasm`, `omni-server`.
- [x] USB camera discovery over V4L2, auto-registered on startup and via
      `POST /api/cameras/discover`.
- [x] Capture + encode pipeline (GStreamer: `v4l2src -> decodebin ->
      vp8enc -> appsink`), verified against real hardware.
- [x] Live browser preview over WebRTC (webrtc-rs), verified end-to-end
      with an automated RTP-level test client.
- [x] Async GStreamer pipeline failures (e.g. device busy) surfaced to the
      browser instead of failing silently.
- [x] REST API: list/create(RTSP)/delete cameras, discover, config.
- [x] Svelte + TypeScript dashboard: dark, Ubiquiti-Protect-style camera
      grid, per-camera live tile with connect/retry states.
- [x] Shared validation logic compiled to WASM (`omni-wasm`) and used by
      the "add camera" form, so client and server enforce identical rules.
- [x] HTTP on port 8090 by default, no TLS (by design for now).

## v0.2 - recording

- [ ] Segmented recording to disk (e.g. fixed-length `.webm`/`.mp4` chunks
      per camera) alongside the live pipeline, not instead of it.
- [ ] Recording retention policy (max age / max disk usage, whichever
      hits first) with a background reaper.
- [ ] Timeline/playback UI for a camera's recorded segments.
- [ ] Event metadata table in SQLite (start/end time, camera, trigger
      reason) so the timeline has something to query against.

## v0.3 - RTSP network cameras

- [ ] Actually implement RTSP capture (an `rtspsrc`-based GStreamer
      pipeline feeding the same `EncodedFrame` channel `omni-capture`
      already exposes for USB) so RTSP cameras added via the API become
      real, not just metadata rows.
- [ ] ONVF/mDNS discovery for network cameras (stretch).

## v0.4 - motion detection & alerting

- [ ] Frame-diff or lightweight ML-based motion detection on the decoded
      stream.
- [ ] Motion events feed the event metadata table from v0.2 and drive
      "record only on motion" mode.
- [ ] Basic notification hook (webhook out) on motion events.

## v0.5 - multi-user & auth

- [ ] Authentication (sessions or tokens) - currently there is none.
- [ ] Per-camera permissions.
- [ ] Still HTTP-first, but document a recommended reverse-proxy TLS setup.

## Later / unscheduled

- [ ] RTSP *server* on port 5544, so OmniMonitor's own streams (including
      USB cameras!) can be re-consumed by third-party NVR/VMS software -
      this is the other half of "USB cameras act like network cameras."
- [ ] Trickle ICE (current signaling waits for full gathering before
      sending offer/answer - simpler, marginally higher latency).
- [ ] Hardware-accelerated encode (VA-API/NVENC) as an alternative to the
      software `vp8enc` path, for higher camera counts on modest hardware.
- [ ] Postgres backend option for multi-node deployments, if that ever
      becomes a real use case (SQLite remains the default - see
      `docs/ARCHITECTURE.md`).
