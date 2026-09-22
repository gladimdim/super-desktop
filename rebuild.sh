#!/usr/bin/env bash
# SUPER DESKTOP - rebuild + reload into Omarchy / Hyprland
#
# Usage:
#   ./rebuild.sh              # release build, restart daemon, hyprctl reload
#   ./rebuild.sh --clean      # cargo clean first (full rebuild from scratch)
#   ./rebuild.sh --no-daemon  # build only, do not restart daemon / reload Hyprland
#   ./rebuild.sh --help       # this help
#
# What it does:
#   1. Rebuilds the native Rust binary (cargo build --release). The running
#      app is left alone, so a failed build never takes down your desktop.
#   2. Stops the running super-desktop daemon and its harness bridge (IPC kill
#      + pkill fallback, stale socket cleanup) so no old process remains.
#   3. Re-links ~/.local/bin/super-desktop -> the native control client and refreshes
#      toolbar assets in ~/.config/super-desktop/assets.
#   4. Reloads Hyprland (hyprctl reload) and validates (hyprctl configerrors).
#   5. Starts a fresh hidden daemon (super-desktop daemon) and verifies it
#      answers `super-desktop status`, then syncs the Omarchy theme. A
#      freshness check guards against a daemon cold-started (e.g. by the
#      toggle hotkey) from the old binary while no listener existed: a stale
#      holder is replaced once instead of silently kept.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_SRC="$SCRIPT_DIR/target/release/super-desktop-client"
BIN_DST="$HOME/.local/bin/super-desktop"
RUST_BIN="$SCRIPT_DIR/target/release/super-desktop"
CONFIG_DIR="$HOME/.config/super-desktop"
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/super-desktop.sock"

CLEAN=0
START_DAEMON=1

for arg in "$@"; do
  case "$arg" in
    --clean) CLEAN=1 ;;
    --no-daemon) START_DAEMON=0 ;;
    -h|--help)
      sed -n '2,20p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg (try --help)" >&2
      exit 1
      ;;
  esac
done

echo "=== SUPER DESKTOP rebuild ==="

# 1. Build first, while the old daemon keeps serving: a broken tree must not
# kill the running desktop (that failure mode used to require a second run).
if [[ "$CLEAN" -eq 1 ]]; then
  echo "--> cargo clean (full rebuild)..."
  cargo clean --manifest-path "$SCRIPT_DIR/Cargo.toml"
fi
echo "--> cargo build --release..."
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

if [[ ! -x "$RUST_BIN" ]]; then
  echo "ERROR: build produced no binary at $RUST_BIN" >&2
  exit 1
fi
echo "✓ Built $RUST_BIN ($(du -h "$RUST_BIN" | cut -f1))"
# Binary bytes are final from here: anything (re)started later runs this build.
BUILD_DONE=$(date +%s)

if [[ "$START_DAEMON" -eq 0 ]]; then
  echo "=== Build done (--no-daemon, running installation unchanged) ==="
  exit 0
fi

stop_desktop() {
  echo "--> Stopping old daemon..."
  if [[ -x "$BIN_DST" ]]; then
    "$BIN_DST" kill >/dev/null 2>&1 || true
  elif [[ -x "$BIN_SRC" ]]; then
    "$BIN_SRC" kill >/dev/null 2>&1 || true
  fi
  pkill -f "super-desktop.*daemon" >/dev/null 2>&1 || true
  # The bridge is a separate process. Leaving it running would keep serving
  # the previous binary after a successful daemon rebuild.
  pkill -f "super-desktop harness-bridge" >/dev/null 2>&1 || true
  # Give IPC `kill` a moment, then drop a stale socket file if the daemon is gone.
  sleep 0.5
  if [[ -S "$SOCK" ]] && ! pgrep -f "super-desktop.*daemon" >/dev/null 2>&1; then
    rm -f "$SOCK"
    echo "    removed stale socket $SOCK"
  fi
}

start_desktop() {
  echo "--> Starting fresh daemon (hidden)..."
  # Detached so it survives the terminal; daemon comes up hidden, overlay builds warm.
  nohup "$BIN_DST" daemon >/tmp/super-desktop-daemon.log 2>&1 & disown || true

  echo "--> Waiting for daemon IPC..."
  for _ in $(seq 1 50); do
    if "$BIN_DST" status >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

# True when every running daemon was born after $1 (epoch seconds). A
# `toggle`/`show` issued while no listener exists cold-starts a daemon from
# whatever binary is on disk at that moment, and the singleton guard then
# keeps that process while our starter exits quietly — without this check the
# script would report success while old code is still running.
all_daemons_fresh() {
  local mark="$1"
  local now oldest=0
  now=$(date +%s)
  local pids pid age
  pids=$(pgrep -f "super-desktop.*daemon" || true)
  if [[ -z "$pids" ]]; then
    return 1
  fi
  for pid in $pids; do
    age=$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d '[:space:]')
    if [[ ! "$age" =~ ^[0-9]+$ ]]; then
      return 1
    fi
    if (( age > oldest )); then
      oldest=$age
    fi
  done
  if (( oldest <= now - mark + 10 )); then
    return 0
  fi
  return 1
}

# 2. Stop old desktop processes only when installing/restarting this build.
stop_desktop

# 3. Re-link into Omarchy paths.
mkdir -p "$HOME/.local/bin"
mkdir -p "$CONFIG_DIR"
chmod +x "$BIN_SRC"
ln -sf "$BIN_SRC" "$BIN_DST"
echo "✓ Symlinked $BIN_DST -> $BIN_SRC"

if [[ -d "$SCRIPT_DIR/assets" ]]; then
  mkdir -p "$CONFIG_DIR/assets"
  cp -r "$SCRIPT_DIR/assets/." "$CONFIG_DIR/assets/"
  echo "✓ Refreshed toolbar assets in $CONFIG_DIR/assets"
fi

# 4. Reload Hyprland + restart daemon.
if command -v hyprctl >/dev/null 2>&1; then
  echo "--> hyprctl reload..."
  hyprctl reload >/dev/null || true
  ERRORS="$(hyprctl configerrors 2>/dev/null || true)"
  if [[ -n "$ERRORS" && "$ERRORS" != "ok" && "$ERRORS" != "OK" ]]; then
    echo "WARNING: Hyprland reported config errors: $ERRORS" >&2
  else
    echo "✓ Hyprland configuration reloaded cleanly"
  fi
else
  echo "    (hyprctl not found, skipping Hyprland reload)"
fi

# 5. Start, then make sure the answering daemon is this build: anything born
# before the build finished may hold the socket with old code.
if ! start_desktop; then
  echo "ERROR: daemon did not answer on $SOCK. See /tmp/super-desktop-daemon.log" >&2
  exit 1
fi
if ! all_daemons_fresh "$BUILD_DONE"; then
  echo "--> Daemon predates this rebuild (cold-started mid-build); replacing once..."
  stop_desktop
  BUILD_DONE=$(date +%s)
  if ! start_desktop || ! all_daemons_fresh "$BUILD_DONE"; then
    echo "ERROR: could not establish a fresh daemon on $SOCK. See /tmp/super-desktop-daemon.log" >&2
    exit 1
  fi
fi
"$BIN_DST" status || true
"$BIN_DST" reload-theme >/dev/null 2>&1 || true
echo "=== SUPER DESKTOP reloaded! Press SUPER + SHIFT + Q to toggle ==="
