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
    command -v curl >/dev/null 2>&1 || {
        printf 'Error: curl not found.\n' >&2
        exit 1
    }
    export SUPER_DESKTOP_BUILD_FROM_SOURCE=1
    local install_url script
    install_url="${SUPER_DESKTOP_INSTALL_SH_URL:-https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh}"
    script="$(curl --proto '=https' --tlsv1.2 -fsSL --retry 3 "$install_url")" || {
        printf 'Error: Could not download installer from %s\n' "$install_url" >&2
        exit 1
    }
    [[ -n "$script" ]] || {
        printf 'Error: Downloaded empty installer from %s\n' "$install_url" >&2
        exit 1
    }
    exec bash -s -- "$@" <<<"$script"
}

main "$@"
