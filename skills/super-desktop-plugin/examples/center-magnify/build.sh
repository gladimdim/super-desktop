#!/usr/bin/env bash
# Builds renderer.wasm next to the manifest. Needs: rustup target add wasm32-unknown-unknown
set -euo pipefail
cd "$(dirname "$0")"
cargo test --quiet
cargo build --quiet --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/center_magnify.wasm renderer.wasm
sha256sum renderer.wasm
