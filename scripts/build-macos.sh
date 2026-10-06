#!/bin/bash
# Build a local development app against Homebrew's GTK/VTE libraries.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -s)" != Darwin ]]; then
    echo 'This build requires macOS.' >&2
    exit 1
fi
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
for command in cargo pkg-config tmux; do
    if ! command -v "$command" >/dev/null; then
        echo "Missing $command. Install Rust and run: brew install gtk4 vte3 tmux pkgconf" >&2
        exit 1
    fi
done
pkg-config --print-errors --exists 'gtk4 >= 4.18'
pkg-config --print-errors --exists 'vte-2.91-gtk4 >= 0.84'
export MACOSX_DEPLOYMENT_TARGET=15.0
cargo build --locked --bins
python3 - <<'PY'
from pathlib import Path
import os, plistlib, re, shutil

root = Path.cwd()
target = Path(os.environ.get('CARGO_TARGET_DIR', root / 'target')).resolve()
app = target / 'macos' / 'SUPER DESKTOP Dev.app'
contents = app / 'Contents'
binary = contents / 'MacOS'
resources = contents / 'Resources'
binary.mkdir(parents=True, exist_ok=True)
resources.mkdir(parents=True, exist_ok=True)
for name in ['super-desktop', 'super-desktop-client']:
    # Replacing a file keeps a running process's mapped image intact.
    temporary = binary / (name + '.new')
    shutil.copy2(target / 'debug' / name, temporary)
    temporary.replace(binary / name)
shutil.copytree(root / 'assets', resources / 'assets', dirs_exist_ok=True)
version = re.search(r'^version = "([^"]+)"', (root / 'Cargo.toml').read_text(), re.M).group(1)
with (contents / 'Info.plist').open('wb') as handle:
    plistlib.dump({
        'CFBundleExecutable': 'super-desktop',
        'CFBundleIdentifier': 'com.superdesktop.development',
        'CFBundleName': 'SUPER DESKTOP',
        'CFBundleDisplayName': 'SUPER DESKTOP Dev',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': version,
        'CFBundleVersion': version,
        'LSMinimumSystemVersion': '15.0',
        'NSHighResolutionCapable': True,
        'NSLocalNetworkUsageDescription': 'Connect SUPER DESKTOP to your paired devices on your local network.',
        'NSBonjourServices': ['_omarchy-harness._tcp'],
    }, handle)
launcher = target / 'macos' / 'SUPER DESKTOP.command'
launcher.write_text('#!/bin/bash\nset -e\ncd "$(dirname "$0")"\nexec /usr/bin/open "$PWD/SUPER DESKTOP Dev.app"\n')
launcher.chmod(0o755)
diagnostics = target / 'macos' / 'SUPER DESKTOP Diagnostics.command'
diagnostics.write_text('#!/bin/bash\ncd "$(dirname "$0")"\n"SUPER DESKTOP Dev.app/Contents/MacOS/super-desktop" diagnose\nread -r -p "Press Return to close… "\n')
diagnostics.chmod(0o755)
print(f'Built: {app}')
print(f'Double-click {launcher} to show or hide the interface.')
print('While running, Control–Option–Space toggles it (or your saved shortcut).')
print(f'For startup problems, double-click {diagnostics}.')
print('Development build: this Mac must retain its Homebrew GTK/VTE/tmux dependencies.')
PY
