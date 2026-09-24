#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo test --workspace
cargo clippy -p iptv-core -p fluxo --all-targets -- -D warnings
node --test tests/*.test.js
npm run check

if [[ "${1:-}" == "--bundle" ]]; then
  npm run tauri build -- --bundles app
  ditto target/release/bundle/macos/Fluxo.app Fluxo.app
fi
