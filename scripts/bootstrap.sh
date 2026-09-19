#!/usr/bin/env bash
# One-command setup for a clean Debian/Ubuntu machine: installs system
# packages, the Rust/wasm toolchain and Node.js if missing, builds
# everything, and installs + enables the systemd service.
#
#   git clone https://github.com/Skaut0071/OmniMonitor.git
#   cd OmniMonitor
#   ./scripts/bootstrap.sh
#
# Run as your normal user, *not* as root/with sudo - it calls `sudo`
# itself for the steps that actually need root (apt, the systemd
# install) and otherwise installs Rust/wasm-pack into your own home
# directory the normal rustup way. There's no hosted apt repository for
# `apt install omnimonitor` yet (that needs a signed package repo to
# host and maintain, a bigger commitment than this project has taken on
# so far - see docs/ROADMAP.md) - this script is the "one command"
# equivalent until/unless that happens.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user (it calls sudo itself where needed), not as root." >&2
    exit 1
fi
if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo not found - install it, or run the steps in README.md's Quickstart by hand." >&2
    exit 1
fi

echo "==> Installing system packages..."
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl git libssl-dev \
    libv4l-dev clang cmake \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev libgstrtspserver-1.0-dev \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
    gstreamer1.0-libav gstreamer1.0-tools

if command -v cargo >/dev/null 2>&1; then
    echo "==> Rust already installed, skipping rustup."
else
    echo "==> Installing Rust (rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck source=/dev/null
source "$HOME/.cargo/env"

rustup target add wasm32-unknown-unknown

if command -v wasm-pack >/dev/null 2>&1; then
    echo "==> wasm-pack already installed, skipping."
else
    echo "==> Installing wasm-pack..."
    curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
fi

# Vite (the frontend build tool) needs Node 18+; Debian/Ubuntu's own
# `apt` package is sometimes older than that depending on release, so
# check the version actually present rather than just checking Node
# exists, and fall back to NodeSource's install script if it's missing
# or too old.
node_major_or_zero() {
    command -v node >/dev/null 2>&1 && node -e 'console.log(process.versions.node.split(".")[0])' || echo 0
}
if [[ "$(node_major_or_zero)" -ge 18 ]]; then
    echo "==> Node.js $(node --version) already installed, skipping."
else
    echo "==> Installing Node.js 20.x (NodeSource)..."
    curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
    sudo apt-get install -y nodejs
fi

echo "==> Building OmniMonitor (wasm module, frontend, release binary)..."
"$ROOT/scripts/build-all.sh"

echo "==> Installing the systemd service..."
sudo "$ROOT/scripts/install.sh"
