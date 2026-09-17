#!/usr/bin/env bash
# Builds crates/omni-wasm to frontend/src/lib/wasm so the Svelte app can
# `import` it. Must be run (or re-run after editing omni-wasm/omni-core)
# before `npm run dev` / `npm run build` in frontend/, since the wasm
# output is a gitignored build artifact, not checked in.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "wasm-pack not found. Install it with: cargo install wasm-pack" >&2
    exit 1
fi

cd "$ROOT/crates/omni-wasm"
wasm-pack build --target web --out-dir ../../frontend/src/lib/wasm --out-name omni_wasm
