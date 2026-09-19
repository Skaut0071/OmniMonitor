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

## v0.5.1 - RTSP server authentication - done

- [x] RTSP Basic auth (`GstRTSPAuth`) on every mount point - a client
      with no or wrong credentials gets a `401` on `DESCRIBE`. Single
      shared credential (`rtsp` / random password, or `OMNI_RTSP_PASSWORD`),
      bootstrapped the same way as the HTTP admin account and stored in
      `rtsp_credentials` (plaintext - required by `GstRTSPAuth`'s
      mechanism, see `docs/ARCHITECTURE.md`).
- [x] `GET /api/rtsp-credentials` (session-gated) plus an "RTSP
      credentials" panel in the UI sidebar to view/copy them.
- [x] Verified against real clients: wrong password, no password, and
      correct password all tested with `ffprobe` against a live USB
      camera's mount point.

## v0.6 - trickle ICE - done

- [x] Both the browser and server now send their SDP as soon as
      `setLocalDescription` resolves and trickle ICE candidates
      individually as they're discovered, instead of waiting for full
      gathering before exchanging either side's SDP - removes up to
      ~1s of avoidable connection-setup latency, more on cross-NAT
      links. `omni_webrtc::StreamSession::start` now returns a candidate
      channel; `omni-server::ws` and `CameraTile.svelte` forward
      candidates over the existing signaling WebSocket in both
      directions.

## v0.7 - ONVIF network camera discovery - done

- [x] "Scan for network cameras" in "+ Add camera": sends a WS-Discovery
      multicast probe (`omni-server::onvif_discovery`, no new
      dependencies - raw UDP via `tokio::net::UdpSocket`) and lists
      whatever ONVIF device answers within 3 seconds by IP address.
      Stops short of resolving an actual RTSP stream URI (that needs a
      further authenticated per-vendor ONVIF call) - picking a result
      just prefills the host in the RTSP URL field.
- [x] `POST /api/onvif/discover` (session-gated), returning address +
      XAddrs for each responder.
- [x] Verified against the real network (multicast join/send/receive
      loop runs end-to-end, correctly returns empty after the timeout
      when nothing answers - no ONVIF camera happened to be reachable
      from this dev environment) plus unit tests for the XAddrs-parsing
      logic against real-world WS-Discovery reply shapes (varying
      namespace prefixes, multiple space-separated XAddrs).

## v0.8 - Alpha: packaging, HTTPS docs, CI, login rate limiting - done

- [x] Systemd packaging: `packaging/omnimonitor.service` (dedicated
      system user, sandboxed with `ProtectSystem=strict` etc.) plus
      `scripts/install.sh` to set it up end-to-end (creates the system
      user, adds it to `video` for USB camera access, installs to
      `/opt/omnimonitor` + `/var/lib/omnimonitor`, enables the unit).
- [x] `AppConfig::from_env()` (`OMNI_HTTP_PORT`/`OMNI_RTSP_PORT`/
      `OMNI_DATA_DIR`), needed so the service isn't tied to a relative
      `./data` path that only makes sense from a dev checkout's working
      directory.
- [x] Reverse-proxy HTTPS recipes: `packaging/Caddyfile.example`
      (automatic Let's Encrypt) and `packaging/nginx.conf.example`
      (bring your own certificate, includes the WebSocket upgrade
      headers `/api/stream/:camera_id` needs).
- [x] CI (`.github/workflows/ci.yml`): backend build + clippy
      (`-D warnings`) + test, and frontend wasm build + `svelte-check` +
      `vite build`, on every push/PR. Caught and fixed a real
      pre-existing type error in `RtspCredentialsModal.svelte` that
      `npm run build` alone hadn't (nullable `creds` used directly in
      markup instead of narrowed local variables).
- [x] Login rate limiting: exponential backoff per source IP on
      `/api/auth/login` after 3 free failures, capped at a 5-minute
      lockout, reset on success (`auth::LoginRateLimiter`). Verified
      against a live server end-to-end (see `docs/ARCHITECTURE.md`).

## Later / unscheduled

- [ ] Apply camera settings changes (recording toggle, resolution, motion
      transitions, ...) without disconnecting active viewers - needs
      dynamic GStreamer `tee` pad add/remove instead of a full pipeline
      restart. Would also fix the motion-events logging fidelity issue
      for `RecordingTrigger::Motion` cameras noted in
      `docs/ARCHITECTURE.md` (event tracking needs to live above the
      per-pipeline-instance watcher, keyed by camera).
- [ ] Multi-user / per-camera permissions - still single-account only,
      and the RTSP credential (above) is a single shared secret too, not
      per-camera.
- [ ] Hardware-accelerated encode (VA-API/NVENC) as an alternative to the
      software `vp8enc` path, for higher camera counts on modest hardware.
- [ ] H.264 passthrough for RTSP cameras that already send it, instead of
      always decoding and re-encoding to VP8 - saves real CPU on
      multi-camera setups, at the cost of a more complex WebRTC codec
      negotiation path.
- [ ] Postgres backend option for multi-node deployments, if that ever
      becomes a real use case (SQLite remains the default - see
      `docs/ARCHITECTURE.md`).
