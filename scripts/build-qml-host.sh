#!/usr/bin/env bash
# Builds sd-qml-host, the process that draws QML plugin surfaces, into
# target/<profile>/ next to the super-desktop binaries.
#
# Usage: scripts/build-qml-host.sh [release|debug]
# Needs qmake6 and Qt Quick (qt6-declarative). Without them it says so and
# exits 0: SUPER DESKTOP then runs without QML plugin surfaces. Release
# packaging sets SD_QML_HOST_REQUIRED=1, which makes that an error.
# Honors CARGO_TARGET_DIR like cargo does.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT_DIR="$TARGET_DIR/$PROFILE"
BUILD_DIR="$TARGET_DIR/qml-host-build"

if ! command -v qmake6 >/dev/null 2>&1; then
  if [[ "${SD_QML_HOST_REQUIRED:-}" == 1 ]]; then
    echo "ERROR: qmake6 not found; install qt6-base and qt6-declarative" >&2
    exit 1
  fi
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
