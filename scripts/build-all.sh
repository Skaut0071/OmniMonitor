#!/usr/bin/env bash
# Full production build: wasm module -> frontend static bundle -> release
# binary. Run this, then `./target/release/omni-server` (with cwd at the
# repo root, or OMNI_FRONTEND_DIST pointed at frontend/dist) to run
# OmniMonitor as it'd actually be deployed.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

"$ROOT/scripts/build-wasm.sh"

cd "$ROOT/frontend"
npm install
npm run build

cd "$ROOT"
cargo build --release --bin omni-server

echo
echo "Build complete."
echo "Run with:  OMNI_FRONTEND_DIST=$ROOT/frontend/dist $ROOT/target/release/omni-server"
