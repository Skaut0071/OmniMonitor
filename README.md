# OmniMonitor

An open-source, self-hosted NVR (Network Video Recorder) for Linux, aiming
for a Ubiquiti-Protect-style dashboard UI/UX with **USB webcams treated as
first-class cameras** - plug in a UVC camera over USB and it gets the same
live-preview and recording pipeline a network/RTSP camera would.

Status: **early (v0.2)**. Live preview over WebRTC and continuous
segmented recording with retention both work end-to-end for USB *and*
RTSP cameras (most WiFi/PoE IP cameras speak RTSP - that's the protocol
this targets for network cameras). No authentication yet - see
[`docs/ROADMAP.md`](docs/ROADMAP.md) for what's next and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how it's built and why.

## Stack

- **Backend:** Rust (axum, tokio, GStreamer via `gstreamer-rs`, `webrtc-rs`,
  SQLite via `sqlx`, V4L2 device discovery via the `v4l` crate).
- **Frontend:** Svelte + TypeScript (Vite), with a small Rust crate
  (`omni-wasm`) compiled to WebAssembly for validation logic shared
  verbatim with the server.
- **Ports:** web UI/API on **8090** (HTTP, no TLS by design - see below),
  RTSP reserved on **5544** for a future milestone.

## Quickstart (Linux)

### 1. System dependencies

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl git libssl-dev \
    libv4l-dev clang cmake \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-tools
```

Also needed: [Rust](https://rustup.rs) (stable), [wasm-pack](https://rustwasm.github.io/wasm-pack/)
(`cargo install wasm-pack`), the `wasm32-unknown-unknown` target
(`rustup target add wasm32-unknown-unknown`), and Node.js 18+.

### 2. Build

```bash
./scripts/build-all.sh
```

This builds the `omni-wasm` module, the Svelte frontend (`frontend/dist`),
and the release backend binary.

### 3. Run

```bash
OMNI_FRONTEND_DIST=$(pwd)/frontend/dist ./target/release/omni-server
```

Then open `http://<host>:8090/` in a browser. Any USB camera visible to
the OS (e.g. `/dev/video0`) is auto-discovered on startup - if you plug one
in after starting the server, click "Rescan USB cameras" in the sidebar (or
`POST /api/cameras/discover`). Add a network camera with "+ Add camera"
and its RTSP URL (e.g. `rtsp://192.168.1.50:554/stream1` - check your
camera's manual for the exact path; most WiFi/PoE IP cameras speak RTSP).
Click the gear icon on a camera tile to turn on continuous recording and
set a retention limit (max age and/or max total size); click the record
icon to browse and play back its recordings.

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
| DELETE | `/api/cameras/:id`                   | Remove a camera.                          |
| WS     | `/api/stream/:camera_id`             | WebRTC signaling for that camera's live preview. |
| GET    | `/api/cameras/:id/recordings`        | List a camera's recorded segments.        |
| GET    | `/api/recordings/:id/:filename`      | Download/stream a segment (Range-request/seekable). |
| DELETE | `/api/recordings/:id/:filename`      | Delete a segment.                         |

## Why no HTTPS by default?

Deliberate choice for early versions: the server speaks plain HTTP on
8090. If you need TLS, put a reverse proxy in front of it or use your own
port-forwarding/tunnel setup. There's also no authentication yet (see
roadmap) - don't expose this to the open internet as-is.

## License

[MIT](LICENSE).
