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
- [x] `scripts/bootstrap.sh`: one command from a clean Debian/Ubuntu
      machine to a running service - installs system packages, Rust/
      wasm-pack/Node.js if missing (checking actual versions present,
      not just presence, since distro-packaged Node is sometimes too
      old for Vite), builds everything, and hands off to
      `scripts/install.sh`. Not a real `apt install omnimonitor` (see
      "Later" below for what that would actually take) but closes the
      practical gap until/unless that happens. `scripts/install.sh`
      itself made idempotent - re-running it after a rebuild now
      restarts an already-running service into the new build instead of
      just enabling the unit again.

## v0.8.1 - security review fixes - done

An external code review of the v0.8 tree (reading the source only, not
running it) found several real issues - see "Security review fixes
(v0.8.1)" in `docs/ARCHITECTURE.md` for the full writeup of each. In
order of what the review rated most severe:

- [x] GStreamer pipeline injection via RTSP camera URL - fixed by
      setting `location` as a typed property after parsing instead of
      interpolating the URL into the launch string. Verified against a
      real RTSP server: normal URLs still work, an injection attempt no
      longer has any effect.
- [x] SSRF via the motion webhook - mitigated by rejecting loopback/
      link-local/unspecified resolved addresses and disabling redirect-
      following at send time (LAN addresses deliberately still allowed -
      notifying local home-automation is the point of the feature).
- [x] Cross-site WebSocket hijacking on `/api/stream/:camera_id` -
      mitigated with an `Origin`-vs-`Host` check on the upgrade request.
      Verified live: mismatched origin gets `403`, matching origin still
      completes the `101` upgrade.
- [x] Removed the unused, needlessly permissive `CorsLayer::permissive()`
      - the frontend is always same-origin, nothing needed it.
- [x] Session tokens now stored hashed (SHA-256) rather than in
      plaintext, so a leaked DB file doesn't hand out ready-to-use
      sessions.
- [x] Changing the admin password now revokes every other session
      immediately (kept only the one making the change) - previously a
      leaked-but-not-yet-noticed session survived a password change for
      its full 30-day life.
- [x] Argon2 hashing/verification moved off the async runtime via
      `spawn_blocking`, so a burst of login attempts can't tie up a
      tokio worker thread.
- [x] `internal_error` no longer echoes DB/IO error text (paths, driver
      detail) to the client - logs it server-side, returns a generic
      message instead.
- [x] Optional `Secure` cookie flag (`OMNI_COOKIE_SECURE=1`) for anyone
      running behind the documented TLS reverse proxy.
- [x] Resolution validation upper bound (8K) - previously only rejected
      zero.
- [x] `cargo audit` in CI, which caught two real things while being set
      up: `sqlx` was pulling in its unused MySQL/Postgres drivers via
      default features (now `default-features = false`), and `sqlx`
      0.7.4 had a real fixed advisory unrelated to SQLite usage (bumped
      to 0.8, re-verified full camera CRUD + login against the existing
      database afterward). One advisory with no available fix (`rsa`,
      reachable only via an optional driver this project never compiles)
      is explicitly `ignore`d in CI with the reasoning inline.
- [x] Extra systemd sandboxing flags in `packaging/omnimonitor.service`
      (not verified against a live systemd instance - see
      `docs/ARCHITECTURE.md`).

Reviewed and deliberately left as-is (reasoning in `docs/ARCHITECTURE.md`):
login rate limiting being keyed by TCP peer address (already a documented
tradeoff of running behind a reverse proxy), RTSP camera URLs/credentials
being visible via `GET /api/cameras` to an already-authenticated admin
(same trust boundary as other credential-revealing endpoints), and
`curl | sh` in `scripts/bootstrap.sh` (each tool's own official install
method).

## v0.9 - dashboard UX: reordering, rotation, zoom, USB device management - done

Real-world usage feedback after installing v0.8.1 as an actual systemd
service, in order of what shipped:

- [x] `ufw` detection in `scripts/install.sh` - a default-deny firewall
      silently blocks the web UI/RTSP even on the LAN, not just the open
      internet, and nothing about installing OmniMonitor itself would
      have failed to reveal that. Offers to run `ufw allow` for both
      ports rather than doing it silently.
- [x] Camera rotation (`Camera::rotation`, none/90°/180°/270°) via
      GStreamer's `videoflip`, inserted right after `decodebin` before
      the scale to the configured resolution - affects the live view,
      recordings, and RTSP re-serve alike, not just the browser. Verified
      the generated pipeline string for all four values against a real
      camera.
  - Found and fixed a real, separate bug while verifying this: the RTSP
    server's per-camera mount point captures its `Camera` snapshot once
    at registration and never refreshes it, so `PATCH /api/cameras/:id`
    (rotation or *any* other setting) was invisible to RTSP clients
    until the next full rescan happened to re-register every mount
    point. `update_camera` now re-registers the mount after every
    change - see `docs/ARCHITECTURE.md`.
- [x] Manual dashboard ordering: drag-and-drop tiles, `sort_order` column
      + `PUT /api/cameras/reorder` (validates the request is exactly the
      current set of camera ids, rejecting a partial reorder that would
      silently corrupt the rest). New cameras append after existing ones
      rather than defaulting into position 0.
- [x] Click a live tile to open it full-size (`ExpandedCameraModal`),
      with scroll-to-zoom/drag-to-pan/double-click-to-reset on the
      **live view only** - browser-side CSS transform, doesn't touch the
      pipeline, recordings, or RTSP output.
  - The WebRTC/trickle-ICE signaling logic (non-trivial, see
    `docs/ARCHITECTURE.md`'s "Signaling protocol" section) was extracted
    from `CameraTile` into a shared `webrtc-view.ts` module so the
    expanded view didn't need a second copy of easy-to-get-subtly-wrong
    connection logic.
- [x] Permanently ignoring a USB device: deleting a USB camera used to
      not stick - the next rescan (or server restart) just saw the
      still-plugged-in device as unknown again and re-added it. Deleting
      a USB camera now also records its device path in a new
      `ignored_usb_devices` table, checked by auto-discovery; reversible
      from "Ignored USB devices" in the sidebar. Verified live: delete →
      rescan doesn't bring it back → un-ignore → rescan does.

## v0.10 - recording timeline, camera groups, status overview - done

The three items deferred from v0.9 as "bigger, own milestone":

- [x] **Recording timeline**: `RecordingTimeline.svelte` renders a
      per-day scrubbable timeline (like Protect's) instead of only a flat
      segment list - clicking a point plays the containing segment from
      the right offset. Segment placement uses a new `RecordingInfo.
      started_at` (`modified - segment_seconds`, since `splitmuxsink`
      doesn't record each segment's exact start anywhere) - exact for a
      finalized segment, approximate for the one currently being written;
      see `docs/ARCHITECTURE.md` for the honest limitation. Motion events
      (which *do* have exact timestamps from the DB) are overlaid on the
      same timeline. The existing segment list/player stays as-is
      alongside it, not replaced.
- [x] **Camera groups**: `Camera::group`, a plain optional string rather
      than a normalized `groups` table (nothing else needs to reference
      a group by id) - shown as dashboard tabs, computed from whatever
      distinct values currently exist across cameras. Renaming a tab
      (`Db::rename_camera_group`) bulk-updates every camera that has the
      old name in one query, so the UX isn't "rename N cameras one at a
      time." Verified live: set on two cameras, rename, clear.
- [x] **Status overview tab**: `GET /api/cameras/status` - exact for a
      camera with a pipeline running (`Supervisor::pipeline_status`,
      reusing the existing capture-error watch channel), a cheap
      best-effort presence probe otherwise (`reachability::
      probe_reachable`: does the USB device node still exist, or does a
      plain TCP connect to the RTSP host succeed within 2s) rather than
      actually opening the device/stream just to answer a status page.
      Verified live against both a real USB camera (online) and a
      deliberately unreachable RTSP address, TEST-NET `192.0.2.55`
      (offline, correctly took ~2s to time out).

## v0.11 - recording schedule, Timeline tab - done

- [x] **Recording schedule**: `RecordingSchedule` (enabled, days-of-week,
      start/end time) added to `RecordingSettings`, editable from
      `CameraSettingsModal`. Gates `Supervisor::pipeline_config`'s
      `recording_now` the same way motion-gating already did, plus a new
      `omni-server::schedule` background task that rebuilds a camera's
      pipeline exactly when it crosses a scheduled boundary. Deliberately
      doesn't affect live view - the camera stays watchable any time, only
      recording is gated. Verified live with real recording segments
      appearing/disappearing at the configured window's edges.
- [x] **Timeline tab**: new `TimelineView.svelte` - a list of currently
      online cameras, click one to watch its live feed as the main
      content, with a vertical list of that camera's motion events on the
      side. The video area is rewindable in place: a scrubbable per-day
      timeline (reusing `RecordingTimeline.svelte` from the Recordings
      modal) sits under it, and dragging/clicking a point on it - or an
      event in the side list - switches straight to that recorded segment
      instead of requiring a trip to the Recordings modal; a "Go live"
      button returns to the live feed. Reuses the existing status/events/
      recordings endpoints and the same live-view connection code as the
      dashboard tiles - no new API routes. Verified with a headless
      browser against a running server, including scrubbing into real
      recorded video and back to live.

## v0.11.1 - scrub-to-preview, timestamp overlay - done

- [x] **Timeline: scroll-to-preview instead of segment playback**: the
      Timeline tab's scrub bar now responds to the mouse wheel (like a
      jog wheel), and both wheel and click show a frozen frame at that
      moment - no native player controls, no autoplay-through-the-segment
      - instead of opening a full `controls autoplay` player, so you can
      see what happened at a point in time without leaving the tab or
      stepping through segments by hand. A "Go live" button returns to
      the live feed. Verified live: scrubbing switches to the correct
      recorded frame, a second scrub on the same segment doesn't reload
      the video, and going live reconnects WebRTC.
- [x] **Camera setting: burned-in timestamp overlay**: a per-camera
      `overlay_timestamp` toggle burns the current date/time onto the
      video itself via GStreamer's `clockoverlay`, in the live view,
      recordings, and RTSP alike - not just a browser-side overlay, and
      not subject to the file-mtime-approximation caveat that
      `RecordingInfo.started_at` has elsewhere. Off by default. Verified
      against a real running camera: the burned-in timestamp is visible
      in a live WebRTC screenshot, present only on the camera with the
      setting on.
- [x] **Timeline: auto-resume, bigger bar, transport buttons** (follow-up
      feedback on the above): scrubbing and letting go for ~500ms now
      auto-plays forward from that point instead of staying frozen
      forever; `⏮ Previous segment` / `▶ Play` / `⏸ Pause` / `Next
      segment ⏭` buttons sit above the bar (not native video controls);
      the scrub bar itself is taller with bigger labels and hourly tick
      lines to make aiming at a specific time easier; the wheel now moves
      ~15s per notch instead of ~4s, since a full day's timeline at the
      old rate took thousands of notches to cross. Verified live.

## v0.12 - motion recording pre/post-roll, LED ring control - done

- [x] **Motion-triggered recording no longer disconnects live viewers**:
      `RecordingTrigger::Motion` used to rebuild the entire capture
      pipeline (including the live-view branch) on every single motion
      start/stop, which visibly disconnected/reconnected anyone watching
      that camera's live view every time something moved. The recording
      branch now stays present in the pipeline for as long as recording
      is enabled at all - exactly like `Continuous` - and
      `omni-server::motion_retention` (a new background reaper, same
      shape as `omni-server::retention`) deletes segments after the fact
      that don't fall near a logged motion event, keeping only footage
      from 30s before each motion event starts through 60s after it ends.
      This also gives motion clips a pre-roll for free (previously a clip
      only started once motion was already detected, with no lead-in).
      Verified live against a running server with real motion events:
      live view stays connected through a motion start/stop, and segments
      outside the padded window get pruned while ones inside it survive.
- [x] **Camera setting: LED ring on/off commands**: a per-camera
      `led_control` (`on_command`/`off_command`, arbitrary shell
      commands run server-side) for cameras with a software-controllable
      LED ring or other indicator - mainly USB cameras. Only applies to a
      camera with no recording and no motion detection enabled, since
      that's the only case where its pipeline isn't already running for
      its own reasons: the "on" command runs when the first live viewer
      connects, "off" when the last one disconnects, mirroring
      `Supervisor`'s existing ephemeral-pipeline lifecycle rather than
      adding a separate one.

## v0.13 - dynamic recording toggle, no viewer disconnect - done

- [x] **Recording on/off no longer restarts the pipeline**: toggling
      `recording.enabled` (or a segment-length edit, or the recording
      schedule crossing a start/end boundary) used to do a full
      `replace_pipeline` restart - same as any other settings change -
      which disconnected every active live viewer of that camera even
      though nothing about the actual video (resolution, rotation, ...)
      changed. `omni-server::supervisor::apply_settings` now compares a
      `StructuralConfig` snapshot (resolution, framerate, rotation,
      overlay, RTSP URL, and whether motion detection needs to run at
      all) against what the running pipeline was actually built with; if
      that's unchanged, only the recording branch is added or removed on
      the *live* pipeline via GStreamer `tee` request-pad add/remove
      (`CaptureSession::set_recording` in `omni-capture`) instead of a
      restart - the live-view branch is never touched, so active viewers
      stay connected through a recording toggle or a schedule boundary.
      Resolution, rotation, the overlay, an RTSP URL edit, or motion
      detection's presence turning on/off still fall back to a full
      restart (see "Later / unscheduled" below) - those change what's
      actually being decoded/encoded, which a branch add/remove can't
      express.
- [x] **Not the same mechanism as the abandoned `valve` attempt**: that
      approach reused one `splitmuxsink` instance across open/close
      cycles and got it stuck on reopen (see "Motion detection" in
      `docs/ARCHITECTURE.md`). `set_recording` instead creates a fresh
      `queue`+`splitmuxsink` pair on every attach and fully removes them
      - after finalizing the current segment with a real EOS, via a
      blocking pad probe plus `Element::call_async` for the actual
      element removal (structural pipeline changes can't happen
      synchronously from a probe callback) - on every detach, so there's
      no reused element to get into a stuck state.

## v0.14 - schedule-gated cameras actually power down outside their window - done

- [x] **A schedule-gated recording camera's pipeline now closes the
      device outside its scheduled window**, not just its recording
      branch - `Supervisor::keeps_pipeline_alive` is schedule-aware:
      `recording.enabled` alone only counts as a reason to keep the
      pipeline running when there's no schedule restricting it, or the
      schedule's window is active right now. Standalone motion detection
      (`motion.enabled`, independent of recording) is unaffected - it
      always keeps the pipeline alive, since there's no way to notice the
      next motion event on a camera that isn't open. Applies to
      `RecordingTrigger::Motion` too: within an active scheduled window
      the camera stays on continuously (so it can keep watching for the
      next motion event, unaffected by v0.12's fix), but outside the
      window it closes entirely, same as a continuous-trigger camera -
      motion isn't watched for outside hours the schedule was explicitly
      set to restrict.
- [x] `schedule.rs`'s boundary-crossing check already called
      `apply_settings` (v0.13) and `ensure_running` on activation - since
      `apply_settings` recomputes `keeps_pipeline_alive` and tears the
      pipeline down itself once it goes false with no active viewer
      (already-existing logic from v0.13's recording-toggle work), making
      `keeps_pipeline_alive` schedule-aware was enough to get the actual
      close-on-boundary behavior with no new teardown path needed. Boot
      startup (`main.rs`) and `routes::update_camera`'s post-save
      `ensure_running` check were updated to the same
      `Supervisor::keeps_pipeline_alive` logic, so a camera outside its
      window doesn't get an unwanted pipeline started at boot or
      immediately after a settings save either.
- [x] **A deliberate tradeoff, not an oversight**: opening/closing a USB
      device right at a schedule boundary carries a real "device busy"
      risk - confirmed against this project's actual USB hardware during
      v0.12/v0.13 testing, rapid open/close cycles on the same
      `/dev/videoN` aren't perfectly reliable even with
      `replace_pipeline`'s existing 200ms settle sleep. Chosen anyway,
      on request, in favor of the camera not sitting open/powered outside
      its scheduled hours.
- [x] Full workspace `cargo build`/`clippy`/`test` clean, including three
      new unit tests for `Supervisor::keeps_pipeline_alive`'s
      deterministic cases (nothing enabled, standalone motion, recording
      with no schedule restriction) - the schedule-boundary case itself
      needs a real/mocked clock to test deterministically and wasn't
      covered by a new automated test this round.

## Later / unscheduled

- [ ] Apply the *remaining* camera settings changes (resolution,
      framerate, rotation, the timestamp overlay, an RTSP URL edit, or
      motion detection turning on/off) without disconnecting active
      viewers. Recording on/off is fixed as of v0.13 - see above - but
      these all change what's actually being decoded/encoded, not just
      whether an already-unchanged pipeline has a branch attached, so
      they'd need genuine mid-stream reconfiguration (live caps
      renegotiation on `videoscale`/`videoflip`/`vp8enc`, or a second
      dynamic-branch mechanism for the motion-detection appsink) rather
      than the `tee` request-pad trick recording uses - meaningfully
      trickier and not yet attempted.
- [ ] Multi-user / per-camera permissions - still single-account only,
      and the RTSP credential (above) is a single shared secret too, not
      per-camera.
- [ ] Per-viewer adaptive bitrate for WebRTC live view (raised after a
      report of live view going black over a Tailscale connection -
      suspected cause: a fixed ~2Mbps VP8 target-bitrate with no
      congestion response, plus no TURN fallback if direct UDP doesn't
      establish - see the STUN/TURN revert noted above). Explicitly
      wanted **per-viewer**, not a single pipeline-wide bitrate: today
      recording, RTSP, and every live viewer all share one `vp8enc`
      encode off one tee, so reacting to one struggling viewer (e.g. a
      phone on Tailscale) would also degrade quality for every other
      viewer and the recording at that moment. Per-viewer quality needs
      simulcast (multiple simultaneous encodes at different
      bitrates/resolutions, viewer subscribes to whichever layer its
      connection can sustain) rather than adjusting the one shared
      encoder - meaningfully more CPU and pipeline complexity than a
      shared-bitrate version would be. The RTCP receiver reports needed
      to detect a struggling viewer (packet loss/jitter) are already
      being read per peer connection in `omni-webrtc` and currently just
      discarded (`crates/omni-webrtc/src/lib.rs`, "RTCP must be read even
      if we don't act on it") - that's the hook point once this is
      picked up.
- [ ] Hardware-accelerated encode (VA-API/NVENC) as an alternative to the
      software `vp8enc` path, for higher camera counts on modest hardware.
- [ ] H.264 passthrough for RTSP cameras that already send it, instead of
      always decoding and re-encoding to VP8 - saves real CPU on
      multi-camera setups, at the cost of a more complex WebRTC codec
      negotiation path.
- [ ] Postgres backend option for multi-node deployments, if that ever
      becomes a real use case (SQLite remains the default - see
      `docs/ARCHITECTURE.md`).
- [ ] Real `.deb`/apt-repo packaging (`apt install omnimonitor`) -
      `scripts/bootstrap.sh` (above) is the one-command equivalent for
      now. Actual apt distribution needs a hosted, signed package
      repository (GPG key management, a repo server, a release process
      that publishes to it) which is real ongoing infrastructure to
      maintain, not just a build step - worth it once there's an
      audience to justify maintaining it.
