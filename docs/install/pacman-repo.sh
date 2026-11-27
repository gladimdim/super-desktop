#!/usr/bin/env bash
# Register the signed SUPER DESKTOP Pacman repository, install super-desktop-bin,
# configure the Omarchy refresh hook, and run user setup (or migration).
#
# Usage:
#   curl -fsSL https://superdesktop.dmytrogladkyi.com/install/pacman-repo.sh | bash
set -euo pipefail

REPO_URL="${SUPER_DESKTOP_PACMAN_REPO_URL:-https://superdesktop.dmytrogladkyi.com/repo}"
REPO_FPR="EBCB860628250D70F3C4D033304840D354676353"
REPO_CONF="${SUPER_DESKTOP_PACMAN_REPO_CONF:-/etc/pacman.d/super-desktop.conf}"
PACMAN_CONF="${SUPER_DESKTOP_PACMAN_CONF:-/etc/pacman.conf}"

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

import_key() {
    if pacman-key --list-keys "$REPO_FPR" >/dev/null 2>&1; then
        printf '✓ Package signing key already trusted (%s)\n' "$REPO_FPR"
        return 0
    fi
    local work keyfile gpghome match
    work="$(mktemp -d)"
    keyfile="$work/super-desktop.gpg"
    gpghome="$work/gnupg"
    mkdir -p "$gpghome"
    chmod 700 "$gpghome"
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$keyfile" "$REPO_URL/super-desktop.gpg" || {
        rm -rf "$work"
        die "Could not download $REPO_URL/super-desktop.gpg"
    }
    match="$(
        GNUPGHOME="$gpghome" gpg --batch --show-keys --with-colons "$keyfile" 2>/dev/null |
            awk -F: '$1 == "pub" { in_pub = 1; next } $1 == "sub" { in_pub = 0 } in_pub && $1 == "fpr" { print $10; in_pub = 0 }' |
            grep -Fx "$REPO_FPR" || true
    )"
    if [[ -z "$match" ]]; then
        rm -rf "$work"
        die "Downloaded key does not match expected fingerprint $REPO_FPR"
    fi
    sudo pacman-key --add "$keyfile" </dev/null
    sudo pacman-key --lsign-key "$REPO_FPR" </dev/null
    rm -rf "$work"
    printf '✓ Trusted SUPER DESKTOP signing key (%s)\n' "$REPO_FPR"
}

configure_repo() {
    local work repo_tmp conf_tmp
    work="$(mktemp -d)"
    repo_tmp="$work/super-desktop.conf"
    cat >"$repo_tmp" <<EOF
[super-desktop]
SigLevel = Required DatabaseRequired
Server = $REPO_URL/\$arch
EOF
    sudo mkdir -p "$(dirname "$REPO_CONF")" </dev/null
    sudo install -m 644 "$repo_tmp" "$REPO_CONF" </dev/null
    if ! grep -qxF "Include = $REPO_CONF" "$PACMAN_CONF" 2>/dev/null; then
        conf_tmp="$work/pacman.conf"
        if grep -q '^\[omarchy\]' "$PACMAN_CONF" 2>/dev/null; then
            awk -v inc="Include = $REPO_CONF" '
                !done && /^\[omarchy\]/ { print inc; print ""; done = 1 }
                { print }
            ' "$PACMAN_CONF" >"$conf_tmp"
        else
            {
                cat "$PACMAN_CONF"
                printf '\nInclude = %s\n' "$REPO_CONF"
            } >"$conf_tmp"
        fi
        sudo install -m 644 "$conf_tmp" "$PACMAN_CONF" </dev/null
    fi
    rm -rf "$work"

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
    printf '✓ Configured [super-desktop] Pacman repository and Omarchy refresh hook\n'
}

sync_super_desktop_db() {
    local work sync_conf
    work="$(mktemp -d)"
    sync_conf="$work/pacman-super-desktop.conf"
    # Keep [options] from pacman.conf (Architecture, DownloadUser, GPGDir, DBPath, etc.)
    # while excluding all other repositories so only [super-desktop] is synced.
    if [[ -r "$PACMAN_CONF" ]]; then
        awk '
            /^[[:space:]]*\[/ {
                if ($0 ~ /^[[:space:]]*\[options\][[:space:]]*$/) {
                    in_options = 1
                    print
                    next
                }
                exit
            }
            in_options && /^[[:space:]]*Include[[:space:]]*=/ { next }
            in_options { print }
        ' "$PACMAN_CONF" >"$sync_conf"
    fi
    if ! grep -q '^[[:space:]]*\[options\][[:space:]]*$' "$sync_conf" 2>/dev/null; then
        printf '[options]\nArchitecture = auto\n' >"$sync_conf"
    fi
    printf '\nInclude = %s\n' "$REPO_CONF" >>"$sync_conf"
    chmod 644 "$sync_conf"
    chmod 755 "$work"
    if ! sudo pacman --config "$sync_conf" -Sy --noconfirm </dev/null; then
        rm -rf "$work"
        die "Could not synchronize the [super-desktop] package database."
    fi
    rm -rf "$work"
}

install_package() {
    local plan status pkgs pkg upgraded=()
    sync_super_desktop_db
    set +e
    plan="$(LC_ALL=C pacman -Sp --needed --noconfirm super-desktop-bin </dev/null 2>&1)"
    status=$?
    set -e
    if ((status != 0)); then
        [[ -z "$plan" ]] || printf '%s\n' "$plan" >&2
        die "Could not resolve super-desktop-bin without a system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run: sudo pacman -S --needed super-desktop-bin && /usr/bin/super-desktop-setup"
    fi
    pkgs="$(LC_ALL=C pacman -Sp --print-format '%n' --needed --noconfirm super-desktop-bin </dev/null 2>/dev/null || true)"
    for pkg in $pkgs; do
        if [[ "$pkg" != "super-desktop-bin" ]] && pacman -Q "$pkg" >/dev/null 2>&1; then
            upgraded+=("$pkg")
        fi
    done
    if ((${#upgraded[@]} > 0)); then
        die "Installing super-desktop-bin requires upgrading already-installed system packages (${upgraded[*]}). This installer does not run a full system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run: sudo pacman -S --needed super-desktop-bin && /usr/bin/super-desktop-setup"
    fi
    sudo pacman -S --needed --noconfirm super-desktop-bin </dev/null ||
        die "Could not install super-desktop-bin without a system upgrade. If a mirror returned 404 for an older dependency, update your system first (omarchy update or sudo pacman -Syu), then run: sudo pacman -S --needed super-desktop-bin && /usr/bin/super-desktop-setup"
}

run_setup() {
    local bin_dst="$HOME/.local/bin/super-desktop"
    local app_dst="$HOME/.local/share/applications/super-desktop.desktop"
    if [[ -L "$bin_dst" && "$(readlink "$bin_dst" 2>/dev/null || true)" != "/usr/bin/super-desktop" ]] ||
        [[ -e "$bin_dst" && ! -L "$bin_dst" ]] ||
        [[ -e "$app_dst" ]]; then
        /usr/bin/super-desktop-setup --migrate
    else
        /usr/bin/super-desktop-setup
    fi
}

main() {
    ((EUID != 0)) || die "Run as your normal desktop user, not with sudo."
    case "$(uname -m)" in
    x86_64 | aarch64) ;;
    *) die "Prebuilt packages are available for x86_64 and aarch64 only." ;;
    esac
    command -v pacman >/dev/null 2>&1 || die "pacman not found."
    command -v gpg >/dev/null 2>&1 || die "gpg not found."
    command -v curl >/dev/null 2>&1 || die "curl not found."

    import_key
    configure_repo
    install_package
    run_setup
    printf '✓ SUPER DESKTOP installed! Press SUPER + SHIFT + Q or run: super-desktop toggle\n'
}

main "$@"
