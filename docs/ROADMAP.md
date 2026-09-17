# Roadmap

Rough, in priority order. Nothing here is a commitment/date - it's the
order the project intends to tackle things.

## v0.1 - USB live preview - done

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
- [x] Svelte + TypeScript dashboard: dark, Ubiquiti-Protect-style camera
      grid, per-camera live tile with connect/retry states.
- [x] Shared validation logic compiled to WASM (`omni-wasm`).
- [x] HTTP on port 8090 by default, no TLS (by design for now).

## v0.2 - RTSP cameras + recording - done

- [x] Real RTSP capture (`rtspsrc -> decodebin -> ...`, same downstream as
      USB), verified against a local RTSP test session (SETUP/PLAY) and
      protocols forced to TCP for NAT/firewall friendliness.
- [x] One shared capture pipeline per camera (`omni-server::supervisor`),
      replacing "one pipeline per browser connection" - required so
      recording and live-viewing (and multiple simultaneous viewers) can
      run against the same USB device at once. Verified with two
      simultaneous live viewers plus an active recording on one USB
      camera at the same time.
- [x] Continuous segmented recording (GStreamer `splitmuxsink`, `.webm`)
      via a `tee` off the same encoded stream used for live view - no
      second encode pass.
- [x] Retention reaper: delete oldest segments once a max age and/or max
      total size is exceeded, enforced continuously (`omni-server::retention`).
      Enabling recording without at least one limit is rejected.
- [x] Recordings browser/player + download in the UI, range-request
      (seekable) playback.
- [x] `PATCH /api/cameras/:id` for updating recording/resolution/etc
      settings, restarting the camera's pipeline immediately so changes
      take effect.
- [x] Two real bugs found and fixed via testing on real hardware:
      recording-filename collisions across pipeline restarts, and a
      "device busy" race from restarting the replacement pipeline before
      the old one released the device. See `docs/ARCHITECTURE.md`.

## v0.3 - motion detection & alerting

- [ ] Frame-diff or lightweight ML-based motion detection on the decoded
      stream.
- [ ] Event metadata table in SQLite (start/end time, camera, trigger
      reason), independent of raw recording segments, so the future
      timeline UI has something richer to query than just file listings.
- [ ] "Record only on motion" mode, building on the event table.
- [ ] Basic notification hook (webhook out) on motion events.

## v0.4 - multi-user & auth

- [ ] Authentication (sessions or tokens) - currently there is none.
- [ ] Per-camera permissions.
- [ ] Still HTTP-first, but document a recommended reverse-proxy TLS setup.

## Later / unscheduled

- [ ] Apply camera settings changes (recording toggle, resolution, ...)
      without disconnecting active viewers - needs dynamic GStreamer
      `tee` pad add/remove instead of a full pipeline restart.
- [ ] ONVIF/mDNS discovery for RTSP cameras, instead of adding by URL.
- [ ] RTSP *server* on port 5544, so OmniMonitor's own streams (including
      USB cameras!) can be re-consumed by third-party NVR/VMS software -
      this is the other half of "USB cameras act like network cameras."
- [ ] Trickle ICE (current signaling waits for full gathering before
      sending offer/answer - simpler, marginally higher latency).
- [ ] Hardware-accelerated encode (VA-API/NVENC) as an alternative to the
      software `vp8enc` path, for higher camera counts on modest hardware.
- [ ] H.264 passthrough for RTSP cameras that already send it, instead of
      always decoding and re-encoding to VP8 - saves real CPU on
      multi-camera setups, at the cost of a more complex WebRTC codec
      negotiation path.
- [ ] Postgres backend option for multi-node deployments, if that ever
      becomes a real use case (SQLite remains the default - see
      `docs/ARCHITECTURE.md`).
