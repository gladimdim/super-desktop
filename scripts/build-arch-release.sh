#!/usr/bin/env bash
# Run as a normal user in a clean, annotated release checkout on Arch Linux.
set -euo pipefail
export RELEASE_TAG="${1:?Pass the release tag}"
output="$(realpath -m "${2:?Pass a fresh output directory}")"
python3 - <<'PY'
import os
import runpy
p = runpy.run_path('scripts/package-arch.py')
version, _ = p['release_metadata'](p['ROOT'])
p['check_release'](p['ROOT'], os.environ['RELEASE_TAG'], version)
PY
python3 -m unittest discover -s tests -p 'test_arch_packaging.py'
env -u SD_GTK_TESTS_ON_DESKTOP cargo test --locked
sources="$(mktemp -d)"
trap 'rm -rf "$sources"' EXIT
python3 scripts/arch_sources.py --output "$sources/source"
# Compile the exact source and asset manifest shipped alongside the binaries.
(cd "$sources/source" && CARGO_TARGET_DIR="$sources/target" cargo build --release --locked --offline --bins)
python3 scripts/package-arch.py --release-tag "$RELEASE_TAG" --output "$output" --source-dir "$sources/source" --binary-dir "$sources/target/release"
cp "$output"/*.tar.gz "$output/aur/"
cd "$output/aur"
makepkg --nodeps --nocheck --clean
mv ./*.pkg.tar.zst ..
cd ..
sha256sum ./*.pkg.tar.zst >> SHA256SUMS
