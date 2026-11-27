#!/usr/bin/env bash
# Install the Omarchy pre-refresh-pacman hook so channel switches preserve
# Include = /etc/pacman.d/super-desktop.conf in /etc/pacman.conf.
#
# Usage:
#   curl -fsSL https://superdesktop.dmytrogladkyi.com/install/pacman-hook.sh | bash
set -euo pipefail

main() {
    if ((EUID == 0)); then
        printf 'Error: Run as your normal user, not with sudo.\n' >&2
        exit 1
    fi
    local hook_dir="$HOME/.config/omarchy/hooks/pre-refresh-pacman.d"
    local hook_file="$hook_dir/super-desktop"
    mkdir -p "$hook_dir"
    cat >"$hook_file" <<'EOF'
#!/bin/bash
CONF=/etc/pacman.conf
SNIPPET=/etc/pacman.d/super-desktop.conf
MARKER="Include = $SNIPPET"
[[ -r $SNIPPET ]] || exit 0
grep -qxF "$MARKER" "$CONF" && exit 0
if grep -q '^\[omarchy\]' "$CONF"; then
    sudo sed -i "0,/^\[omarchy\]/s||$MARKER\n\n[omarchy]|" "$CONF"
else
    printf '\n%s\n' "$MARKER" | sudo tee -a "$CONF" >/dev/null
fi
EOF
    chmod 755 "$hook_file"
    printf '✓ Installed Omarchy pre-refresh-pacman hook at %s\n' "$hook_file"
}

main "$@"
