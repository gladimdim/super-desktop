#!/usr/bin/env bash
# Build and install the latest released version of SUPER DESKTOP from source.
#
# Usage:
#   curl -fsSL https://superdesktop.dmytrogladkyi.com/install/source.sh | bash
set -euo pipefail

main() {
    if ((EUID == 0)); then
        printf 'Error: Run as your normal user, not with sudo.\n' >&2
        exit 1
    fi
    export SUPER_DESKTOP_BUILD_FROM_SOURCE=1
    exec bash <(curl --proto '=https' --tlsv1.2 -fsSL --retry 3 \
        "https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh") "$@"
}

main "$@"
