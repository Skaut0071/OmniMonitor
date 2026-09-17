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

## v0.3 - motion detection & alerting - done

- [x] Frame-diff motion detection on a downscaled raw-video branch (no
      OpenCV/ML dependency - cheap enough to run continuously per camera).
- [x] Motion-event metadata table in SQLite (start/end time, camera),
      independent of raw recording segments, queryable via
      `GET /api/cameras/:id/events`.
- [x] "Record only on motion" mode (`recording.trigger = "motion"`).
      Implemented as a full pipeline rebuild on each motion transition,
      not a live-toggled element - see `docs/ARCHITECTURE.md` for why a
      GStreamer `valve` was tried first and didn't work out after testing
      it end-to-end (three distinct failure modes, none visible from
      inspecting the pipeline string).
- [x] Webhook notification (JSON POST) on motion start, with sensitivity
      and the webhook URL configurable per camera.
- [x] Frontend: motion sensitivity/webhook settings, a live "MOTION" tile
      badge (polled), and an events tab alongside the recordings browser.

## v0.4 - auth - done

- [x] Authentication: single admin account, argon2-hashed password,
      server-side session tokens in an `HttpOnly` cookie
      (`omni-server::auth`). Bootstrapped on first boot via
      `OMNI_ADMIN_PASSWORD` or a randomly generated password printed to
      the log once. Change-password endpoint + UI.
- [ ] Still HTTP-first (see "HTTPS" in `docs/ARCHITECTURE.md`) - a
      recommended reverse-proxy TLS setup is documented as future work,
      not written up yet.

## v0.5 - RTSP server - done

- [x] RTSP *server* on port 5544: every camera, including USB ones, is
      reachable at `rtsp://<host>:5544/<camera-id>` for third-party
      NVR/VMS/player software - the other half of "USB cameras act like
      network cameras" (`omni-server::rtsp`, `gstreamer-rtsp-server`).
      An RTSP client is just another `Supervisor::acquire_viewer` caller,
      so it shares the same capture pipeline as WebRTC viewers and
      recording, with no second device open or encode pass.
- [x] Verified against real, independent RTSP clients - not just
      `gst-launch`: `ffprobe` establishing its own RTSP session and
      correctly reporting codec/resolution, and a USB camera served to an
      RTSP client and a WebRTC viewer at the same time with neither
      affecting the other.

## Later / unscheduled

- [ ] Apply camera settings changes (recording toggle, resolution, motion
      transitions, ...) without disconnecting active viewers - needs
      dynamic GStreamer `tee` pad add/remove instead of a full pipeline
      restart. Would also fix the motion-events logging fidelity issue
      for `RecordingTrigger::Motion` cameras noted in
      `docs/ARCHITECTURE.md` (event tracking needs to live above the
      per-pipeline-instance watcher, keyed by camera).
- [ ] Multi-user / per-camera permissions - still single-account only.
- [ ] RTSP server authentication (basic/digest) - port 5544 is currently
      wide open to anyone who can reach it.
- [ ] ONVIF/mDNS discovery for RTSP cameras, instead of adding by URL.
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
