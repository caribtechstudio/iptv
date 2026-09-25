#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo test --workspace
cargo clippy -p iptv-core -p fluxo --all-targets -- -D warnings
node --test tests/*.test.js
npm run check

if [[ "${1:-}" == "--bundle" ]]; then
  # FFmpeg and libmpv from Homebrew (brew install mpv) are copied into the app.
  python3 scripts/bundle_native.py
  npm run tauri build -- --bundles app --config src-tauri/tauri.native.conf.json
  rm -rf Fluxo.app
  ditto target/release/bundle/macos/Fluxo.app Fluxo.app
fi
