#!/usr/bin/env bash
# Builds renderer.wasm next to the manifest and checks it with SUPER DESKTOP.
# Needs: rustup target add wasm32-unknown-unknown
set -euo pipefail
cd "$(dirname "$0")"
cargo test --quiet
cargo build --quiet --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/center_magnify.wasm renderer.wasm
sha256sum renderer.wasm
# SUPER_DESKTOP picks another build, e.g. target/debug/super-desktop of a checkout.
SD="${SUPER_DESKTOP:-super-desktop}"
if command -v "$SD" >/dev/null; then
    "$SD" plugin validate .
    "$SD" plugin test .
fi
