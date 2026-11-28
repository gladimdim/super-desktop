#!/usr/bin/env bash
# Download, SHA-256 verify, and install a prebuilt super-desktop-bin .pkg.tar.zst
# directly from GitHub Releases (no compilation needed).
#
# Usage:
#   curl -fsSL https://superdesktop.dmytrogladkyi.com/install/direct-pkg.sh | bash
#   curl -fsSL https://superdesktop.dmytrogladkyi.com/install/direct-pkg.sh | VER=2.2.0 bash
set -euo pipefail

REPO_URL="${SUPER_DESKTOP_PACMAN_REPO_URL:-https://superdesktop.dmytrogladkyi.com/repo}"
RELEASE_BASE_URL="${SUPER_DESKTOP_RELEASE_BASE_URL:-https://github.com/gladimdim/super-desktop/releases/download}"
RELEASE_LATEST_API="${SUPER_DESKTOP_RELEASE_LATEST_API:-https://api.github.com/repos/gladimdim/super-desktop/releases/latest}"
SETUP_BIN="${SUPER_DESKTOP_SETUP_BIN:-/usr/bin/super-desktop-setup}"
REPO_FPR="EBCB860628250D70F3C4D033304840D354676353"

TEMP_DIRS=()
cleanup() {
    local d
    for d in "${TEMP_DIRS[@]:-}"; do
        [[ -z "$d" ]] || rm -rf "$d"
    done
}
trap cleanup EXIT

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

resolve_tag() {
    local requested="${VER:-}"
    if [[ -n "$requested" ]]; then
        requested="v${requested#v}"
        [[ "$requested" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
            die "VER must look like 2.2.0 or v2.2.0"
        printf '%s\n' "$requested"
        return 0
    fi
    local tag
    tag="$(
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 \
            "$RELEASE_LATEST_API" |
            sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
    )"
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
        die "Could not determine latest GitHub Release tag."
    printf '%s\n' "$tag"
}

maybe_import_key() {
    if pacman-key --list-keys "$REPO_FPR" >/dev/null 2>&1; then
        return 0
    fi
    command -v gpg >/dev/null 2>&1 || return 0
    local work keyfile gpghome match
    work="$(mktemp -d)"
    TEMP_DIRS+=("$work")
    keyfile="$work/super-desktop.gpg"
    gpghome="$work/gnupg"
    mkdir -p "$gpghome"
    chmod 700 "$gpghome"
    if curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$keyfile" "$REPO_URL/super-desktop.gpg" 2>/dev/null; then
        match="$(
            GNUPGHOME="$gpghome" gpg --batch --show-keys --with-colons "$keyfile" 2>/dev/null |
                awk -F: '$1 == "pub" { in_pub = 1; next } $1 == "sub" { in_pub = 0 } in_pub && $1 == "fpr" { print $10; in_pub = 0 }' |
                grep -Fx "$REPO_FPR" || true
        )"
        if [[ -n "$match" ]]; then
            sudo pacman-key --add "$keyfile" </dev/null
            sudo pacman-key --lsign-key "$REPO_FPR" </dev/null
        fi
    fi
}

main() {
    ((EUID != 0)) || die "Run as your normal desktop user, not with sudo."
    local arch tag ver pkg_name base_url work want have
    arch="$(uname -m)"
    case "$arch" in
    x86_64 | aarch64) ;;
    *) die "Prebuilt packages are available for x86_64 and aarch64 only." ;;
    esac
    command -v pacman >/dev/null 2>&1 || die "pacman not found."
    command -v curl >/dev/null 2>&1 || die "curl not found."
    command -v sudo >/dev/null 2>&1 || die "sudo not found."
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum not found."

    tag="$(resolve_tag)"
    ver="${tag#v}"
    pkg_name="super-desktop-bin-${ver}-1-${arch}.pkg.tar.zst"
    base_url="${RELEASE_BASE_URL%/}/$tag"

    work="$(mktemp -d)"
    TEMP_DIRS+=("$work")

    printf 'Downloading %s (%s)...\n' "$pkg_name" "$tag"
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$pkg_name" "$base_url/$pkg_name" ||
        die "Release $tag has no prebuilt $pkg_name asset. Use install.sh for older tags."
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/SHA256SUMS" "$base_url/SHA256SUMS" ||
        die "Could not download SHA256SUMS for $tag."

    want="$(awk -v file="$pkg_name" '$2 == file || $2 == ("*" file) { print $1; exit }' "$work/SHA256SUMS")"
    [[ "$want" =~ ^[0-9a-f]{64}$ ]] || die "SHA256SUMS does not contain a valid checksum for $pkg_name."
    have="$(sha256sum "$work/$pkg_name" | awk '{ print $1 }')"
    [[ "$want" == "$have" ]] || die "Checksum mismatch for $pkg_name (expected $want, got $have)."
    printf '✓ Verified SHA-256 checksum for %s\n' "$pkg_name"

    if curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$pkg_name.sig" "$base_url/$pkg_name.sig" 2>/dev/null; then
        maybe_import_key
    fi

    sudo pacman -U --noconfirm "$work/$pkg_name" </dev/null

    local bin_dst="$HOME/.local/bin/super-desktop"
    local app_dst="$HOME/.local/share/applications/super-desktop.desktop"
    if [[ -f "$HOME/.config/hypr/hyprland.lua" ]]; then
        if [[ -L "$bin_dst" && "$(readlink "$bin_dst" 2>/dev/null || true)" != "/usr/bin/super-desktop" ]] ||
            [[ -e "$bin_dst" && ! -L "$bin_dst" ]] ||
            [[ -e "$app_dst" ]]; then
            "$SETUP_BIN" --migrate
        else
            "$SETUP_BIN"
        fi
    else
        if [[ -L "$bin_dst" && "$(readlink "$bin_dst" 2>/dev/null || true)" != "/usr/bin/super-desktop" ]]; then
            mv -f "$bin_dst" "${bin_dst}.before-package"
            mkdir -p "$(dirname "$bin_dst")"
            ln -sf /usr/bin/super-desktop "$bin_dst"
        fi
        if [[ -f "$app_dst" ]] && grep -q "super-desktop" "$app_dst"; then
            mv -f "$app_dst" "${app_dst}.before-package"
        fi
        local hypr_conf="$HOME/.config/hypr/hyprland.conf"
        if [[ -f "$hypr_conf" ]]; then
            if ! grep -qF "# >>> super-desktop >>>" "$hypr_conf"; then
                [[ -e "${hypr_conf}.before-super-desktop" ]] || cp -a "$hypr_conf" "${hypr_conf}.before-super-desktop"
                cat >>"$hypr_conf" <<'EOF'

# >>> super-desktop >>>
exec-once = super-desktop daemon
bindr = SUPER SHIFT, Q, exec, super-desktop toggle
# <<< super-desktop <<<
EOF
            fi
        else
            printf 'Note: Plain Arch setup skipped Omarchy Lua hooks. Add `super-desktop daemon` and `super-desktop toggle` to your Wayland compositor config.\n'
        fi
    fi
    printf '✓ Installed %s! Press SUPER + SHIFT + Q or run: super-desktop toggle\n' "$pkg_name"
}

main "$@"
