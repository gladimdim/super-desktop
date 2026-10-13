#!/usr/bin/env bash
# Builds sd-qml-host, the process that draws QML plugin surfaces, into
# target/<profile>/ next to the super-desktop binaries.
#
# Usage: scripts/build-qml-host.sh [release|debug]
# Needs qmake6 and Qt Quick (qt6-declarative). Without them it says so and
# exits 0: SUPER DESKTOP then runs without QML plugin surfaces.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-release}"
OUT_DIR="$ROOT/target/$PROFILE"
BUILD_DIR="$ROOT/target/qml-host-build"

if ! command -v qmake6 >/dev/null 2>&1; then
  echo "    (qmake6 not found: QML plugin surfaces are off; install qt6-declarative to enable them)"
  exit 0
fi

mkdir -p "$BUILD_DIR" "$OUT_DIR"
(
  cd "$BUILD_DIR"
  qmake6 "$ROOT/qml-host/sd-qml-host.pro" >/dev/null
  make -s -j"$(nproc 2>/dev/null || echo 2)" >/dev/null
)
install -m 755 "$BUILD_DIR/sd-qml-host" "$OUT_DIR/sd-qml-host"
echo "✓ Built $OUT_DIR/sd-qml-host"
