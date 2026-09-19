# OmniMonitor

[![CI](https://github.com/Skaut0071/OmniMonitor/actions/workflows/ci.yml/badge.svg)](https://github.com/Skaut0071/OmniMonitor/actions/workflows/ci.yml)

An open-source, self-hosted NVR (Network Video Recorder) for Linux, aiming
for a Ubiquiti-Protect-style dashboard UI/UX with **USB webcams treated as
first-class cameras** - plug in a UVC camera over USB and it gets the same
live-preview and recording pipeline a network/RTSP camera would.

Status: **Alpha (v0.8.1)**. Live preview over WebRTC (trickle ICE),
continuous or motion-triggered segmented recording with retention,
motion detection with webhook alerts, single-account login with
brute-force lockout, an authenticated RTSP server that re-serves every
camera (USB included) to third-party NVR/VMS/player software, and ONVIF
network-camera discovery all work end-to-end for USB *and* RTSP cameras
(most WiFi/PoE IP cameras speak RTSP - that's the protocol this targets
for network cameras). Installable as a systemd service (see below). See
[`docs/ROADMAP.md`](docs/ROADMAP.md) for what's next and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how it's built and
why.

## Stack

- **Backend:** Rust (axum, tokio, GStreamer via `gstreamer-rs`, `webrtc-rs`,
  SQLite via `sqlx`, V4L2 device discovery via the `v4l` crate).
- **Frontend:** Svelte + TypeScript (Vite), with a small Rust crate
  (`omni-wasm`) compiled to WebAssembly for validation logic shared
  verbatim with the server.
- **Ports:** web UI/API on **8090** (HTTP, no TLS by design - see below);
  every camera also reachable at `rtsp://<host>:5544/<camera-id>` for
  third-party RTSP clients (VLC, `ffprobe`, other NVR/VMS software).

## Quickstart (Debian/Ubuntu)

```bash
git clone https://github.com/Skaut0071/OmniMonitor.git
cd OmniMonitor
./scripts/bootstrap.sh
```

One command, on a clean machine: installs the system packages (GStreamer
dev libs, build tools), Rust + `wasm-pack` + Node.js if you don't already
have them, builds everything, and installs + enables OmniMonitor as a
systemd service (see "Running as a service" below for what that sets
up). It asks for `sudo` only for the steps that need it (apt, the
service install) - run it as your normal user, not as root.

Not an actual `apt install omnimonitor` yet - that needs a hosted,
signed package repository to maintain, which is a bigger commitment than
this project has taken on so far (tracked in `docs/ROADMAP.md`). This
script is the practical equivalent until/unless that happens: one
command, idempotent (safe to re-run after a `git pull` to rebuild and
update an existing install).

Then start it and open `http://<host>:8090/` in a browser:

```bash
sudo systemctl start omnimonitor
sudo journalctl -u omnimonitor -f   # watch for the first-boot passwords
```

### Manual build (no system service, e.g. for development)

If you'd rather build and run it directly without installing a service -
system dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl git libssl-dev \
    libv4l-dev clang cmake \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev libgstrtspserver-1.0-dev \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
    gstreamer1.0-libav gstreamer1.0-tools
```

Also needed: [Rust](https://rustup.rs) (stable), [wasm-pack](https://rustwasm.github.io/wasm-pack/)
(`cargo install wasm-pack`), the `wasm32-unknown-unknown` target
(`rustup target add wasm32-unknown-unknown`), and Node.js 18+. Then:

```bash
./scripts/build-all.sh
OMNI_FRONTEND_DIST=$(pwd)/frontend/dist ./target/release/omni-server
```

Then open `http://<host>:8090/` in a browser. On first boot, check the
server's log output for a line like:

```
No admin account existed - created one. Username: admin  Password: <random>
```

That's printed exactly once - log in with it and change it from the
sidebar ("Change password"), or set `OMNI_ADMIN_PASSWORD` in the
environment before the very first run to pick your own instead.

The RTSP server (below) needs its own credential, bootstrapped the same
way - check the log for:

```
No RTSP credential existed - created one. Username: rtsp  Password: <random>
```

View or copy it any time from the sidebar ("RTSP credentials"), or set
`OMNI_RTSP_PASSWORD` before the very first run to pick your own.

Any USB camera visible to the OS (e.g. `/dev/video0`) is auto-discovered
on startup - if you plug one in after starting the server, click "Rescan
USB cameras" in the sidebar (or
`POST /api/cameras/discover`). Add a network camera with "+ Add camera"
and its RTSP URL (e.g. `rtsp://192.168.1.50:554/stream1` - check your
camera's manual for the exact path; most WiFi/PoE IP cameras speak
RTSP), or click "Scan for network cameras" there first if it supports
ONVIF - it'll list any camera that answers on the LAN by IP address so
you don't have to go find that in your router's DHCP client list.
Click the gear icon on a camera tile to turn on recording (continuous, or
only while motion is detected) and set a retention limit (max age and/or
max total size), and to turn on motion detection/webhook alerts
independently of recording. Click the record icon to browse/play back
recordings and view the motion event log. Every camera is also available
to any RTSP client at `rtsp://<host>:5544/<camera-id>` (its id from
`GET /api/cameras`), authenticated with the RTSP credential above - try
`ffplay rtsp://rtsp:<password>@<host>:5544/<camera-id>` or add it in VLC.

### Running as a service, or updating an existing install

`./scripts/bootstrap.sh` above already does this (dedicated system user,
runs under `/opt/omnimonitor` + `/var/lib/omnimonitor`, restarts on
failure) as its last step. If you already have the toolchain installed
and just want to (re)build and (re)install - e.g. after `git pull` to
update - skip straight to:

```bash
./scripts/build-all.sh
sudo ./scripts/install.sh
```

`install.sh` is safe to re-run: if the service is already active it
rebuilds/reinstalls the files in place and restarts it into the new
build; if it's not running yet, it enables the unit without starting it
so you can start it (and see the first-boot passwords) yourself:

```bash
sudo systemctl start omnimonitor
sudo journalctl -u omnimonitor -f
```

See `packaging/omnimonitor.service` for the unit file (also where
`OMNI_ADMIN_PASSWORD`/`OMNI_RTSP_PASSWORD` would go if you want to set
them instead of using the printed-once random ones) and re-run
`sudo systemctl daemon-reload && sudo systemctl restart omnimonitor`
after editing it.

### Development loop

```bash
# Terminal 1: backend, auto-rebuilds are just `cargo run` again
cargo run --bin omni-server

# Terminal 2: frontend with hot reload, proxies /api to :8090
./scripts/build-wasm.sh   # whenever omni-core/omni-wasm change
cd frontend && npm install && npm run dev
```

## REST API

| Method | Path                                 | Description                              |
|--------|--------------------------------------|-------------------------------------------|
| GET    | `/api/config`                        | Server config (ports, data dir).          |
| GET    | `/api/cameras`                       | List cameras.                             |
| POST   | `/api/cameras`                       | Add an RTSP camera: `{name, url}`.        |
| PATCH  | `/api/cameras/:id`                   | Update name/url/resolution/recording settings; restarts the camera's pipeline if it's running. |
| POST   | `/api/cameras/discover`              | Re-scan for USB cameras.                  |
| POST   | `/api/onvif/discover`                | Scan the LAN for ONVIF cameras (~3s), returns `[{address, xaddrs}]`. |
| DELETE | `/api/cameras/:id`                   | Remove a camera.                          |
| WS     | `/api/stream/:camera_id`             | WebRTC signaling for that camera's live preview. |
| GET    | `/api/cameras/:id/recordings`        | List a camera's recorded segments.        |
| GET    | `/api/recordings/:id/:filename`      | Download/stream a segment (Range-request/seekable). |
| DELETE | `/api/recordings/:id/:filename`      | Delete a segment.                         |
| GET    | `/api/cameras/:id/motion`            | `{"active": bool}` - is motion currently detected. |
| GET    | `/api/cameras/:id/events`            | List recent motion events (start/end time). |
| POST   | `/api/auth/login`                    | `{username, password}` - the only endpoint reachable without a session. |
| POST   | `/api/auth/logout`                   | Clear the current session.                |
| GET    | `/api/auth/me`                       | `{"username": ...}` if logged in, 401 otherwise. |
| POST   | `/api/auth/change-password`          | `{current_password, new_password}`.       |
| GET    | `/api/rtsp-credentials`              | `{username, password, port}` for the RTSP server. |

Every endpoint above `/api/auth/login` requires a valid session cookie
(set by logging in) - see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#authentication).

## HTTPS

Deliberate choice for early versions: the server speaks plain HTTP on
8090 - no TLS built in. If you need TLS, put a reverse proxy in front of
it: see `packaging/Caddyfile.example` (gets you a free auto-renewing
Let's Encrypt certificate just from a real DNS name) or
`packaging/nginx.conf.example` (bring your own certificate). Login
attempts are now rate-limited (exponential backoff per source IP after 3
failures, see `docs/ARCHITECTURE.md#authentication`), but it's still a
single admin account and the RTSP credential (port 5544) is a single
shared secret with no rate limiting of its own - don't expose either
past a reverse proxy (or a VPN/port-forward you trust) without
understanding that.

Only the web UI (port 8090) can go through an HTTP(S) reverse proxy this
way - the RTSP server (port 5544) isn't HTTP, so it needs a TLS-capable
TCP proxy (e.g. `stunnel`) or a VPN if you need it reachable outside your
LAN at all.

If you do put a reverse proxy in front, set `OMNI_COOKIE_SECURE=1` (see
`packaging/omnimonitor.service`) so the session cookie is only ever sent
over the proxy's encrypted hop.

## Security

v0.8.1 addressed a round of real findings from an external code review -
see "Security review fixes (v0.8.1)" in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full list and
reasoning (a GStreamer pipeline injection via RTSP camera URLs, SSRF
mitigation on the motion webhook, cross-site WebSocket hijacking
protection, hashed session tokens, and more). This is still an early
Alpha with a single admin account and no independent security audit -
treat it accordingly, especially before exposing it past a trusted LAN.

## License

[MIT](LICENSE).
