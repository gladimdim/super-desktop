#!/usr/bin/env bash
# SUPER DESKTOP installer for Omarchy (Hyprland on Arch Linux).
#
# Install or update with one command, as your normal user:
#
#   curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash
#
# Run from a clone (./install.sh), it installs that clone and leaves its git
# state alone.
#
# In order, it:
#   1. installs missing Arch packages without running a full system upgrade;
#   2. installs Rust with rustup when there is no Rust 1.92 or newer, into
#      ~/.cargo and ~/.rustup, without editing your shell profile;
#   3. clones the source into ~/.local/share/super-desktop/source (or uses
#      the clone an earlier install runs from) and checks out the newest
#      release, or the release SUPER_DESKTOP_VERSION names;
#   4. builds the release binaries in that clone;
#   5. links ~/.local/bin/super-desktop, copies the toolbar assets, and adds
#      the launcher entry, the SUPER + SHIFT + Q binding, the overlay's layer
#      rule and the Omarchy theme hook;
#   6. restarts SUPER DESKTOP if it is running, or reloads Hyprland.
#
# Running it again updates the app to the newest release and keeps every
# setting already in place.
#
# Environment:
#   SUPER_DESKTOP_DIR     clone to install from (default: step 3)
#   SUPER_DESKTOP_REPO    git URL to clone
#   SUPER_DESKTOP_BRANCH  branch to follow with SUPER_DESKTOP_VERSION=master
#                         (default: master); setting it implies that
#   SUPER_DESKTOP_VERSION what to install:
#                         unset   the newest release, and a pinned install
#                                 stays on its release
#                         latest  the newest release, leaving any pin
#                         v1.1.17 (or 1.1.17) that release, pinned: the source
#                                 stays on it until you choose another
#                         master  the branch with unreleased changes, followed
#                                 until you choose a release again
#
# Everything runs from main, called on the last line, so a download cut
# short runs nothing.
set -euo pipefail

# Empty when the script arrives on stdin (curl | bash).
SELF="${BASH_SOURCE[0]:-}"

INSTALL_COMMAND="curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash"
REPO_URL="${SUPER_DESKTOP_REPO:-https://github.com/gladimdim/super-desktop.git}"
BRANCH="${SUPER_DESKTOP_BRANCH:-master}"
# "", "latest", "master", or a release tag vX.Y.Z; see resolve_release.
RELEASE="${SUPER_DESKTOP_VERSION:-}"
# What the clone follows, in its git config: releases, pinned or branch.
# src/updates.rs (Settings → Updates) reads and writes the same key.
CHANNEL_KEY="superdesktop.channel"
RELEASES_URL="https://github.com/gladimdim/super-desktop/releases"
DEFAULT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/super-desktop/source"

PACMAN_REPO_URL="${SUPER_DESKTOP_PACMAN_REPO_URL:-https://superdesktop.dmytrogladkyi.com/repo}"
RELEASE_BASE_URL="${SUPER_DESKTOP_RELEASE_BASE_URL:-https://github.com/gladimdim/super-desktop/releases/download}"
REPO_KEY_FINGERPRINT="EBCB860628250D70F3C4D033304840D354676353"
PACMAN_REPO_NAME="super-desktop"
PACMAN_REPO_CONF="${SUPER_DESKTOP_PACMAN_REPO_CONF:-/etc/pacman.d/super-desktop.conf}"
PACMAN_CONF="${SUPER_DESKTOP_PACMAN_CONF:-/etc/pacman.conf}"
BUILD_FROM_SOURCE="${SUPER_DESKTOP_BUILD_FROM_SOURCE:-0}"

# The oldest Rust that builds the locked dependencies (gtk4 0.11).
MIN_RUST="1.92"
MIN_GTK4="4.18"
MIN_VTE4="0.84"

OS_RELEASE_FILE="${SUPER_DESKTOP_OS_RELEASE:-/etc/os-release}"
DISTRO_FAMILY="omarchy"
DISTRO_NAME="Omarchy"
IS_OMARCHY=1
NON_OMARCHY_NEEDS_BINDING=0

# The build needs a C compiler, GTK 4, gtk4-layer-shell and VTE, and Qt Quick
# for the plugin host. At run time the app uses tmux
# for sessions, wl-clipboard for copying, libnotify for notifications, sqlite
# for OpenCode card titles, avahi for phone discovery, bubblewrap with poppler
# for sandboxed PDF previews, and GStreamer's plugins to play sound and video
# previews.
PACKAGES=(
    base-devel git curl
    gtk4 gtk4-layer-shell vte4 qt6-base qt6-declarative
    tmux wl-clipboard libnotify sqlite avahi bubblewrap poppler
    gst-plugins-good gst-libav
)

BIN_DST="$HOME/.local/bin/super-desktop"
CONFIG_DIR="$HOME/.config/super-desktop"
APP_DST="$HOME/.local/share/applications/super-desktop.desktop"
BINDINGS_LUA="$HOME/.config/hypr/bindings.lua"
HYPRLAND_LUA="$HOME/.config/hypr/hyprland.lua"

SRC=""
RUST_INSTALLED=0

say() { printf '%s\n' "$*"; }
warn() { printf 'Warning: %s\n' "$*" >&2; }
die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<EOF
Install or update SUPER DESKTOP on Linux (auto-detects Omarchy, Arch, Fedora, Ubuntu/Debian, and openSUSE):

  $INSTALL_COMMAND

Environment:
  SUPER_DESKTOP_DIR     clone to install from (default: $DEFAULT_DIR,
                        or the clone an earlier install runs from)
  SUPER_DESKTOP_REPO    git URL to clone (default: $REPO_URL)
  SUPER_DESKTOP_BRANCH  branch for SUPER_DESKTOP_VERSION=master (default: $BRANCH)
  SUPER_DESKTOP_VERSION unset: the newest release (a pinned install stays put)
                        latest: the newest release, leaving a pin
                        v1.1.17: that release, pinned
                        master: the branch, with unreleased changes
                        (releases: $RELEASES_URL)
  SUPER_DESKTOP_BUILD_FROM_SOURCE
                        1: build from source instead of installing packages

A specific release, for example the one that matches an older phone app:

  ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=v1.1.17 bash
EOF
}

os_release_field() {
    local key="$1" k v
    [[ -r "$OS_RELEASE_FILE" ]] || return 0
    while IFS='=' read -r k v || [[ -n "$k" ]]; do
        [[ "$k" == "$key" ]] || continue
        v="${v%$'\r'}"
        if [[ "$v" == \"*\" || "$v" == \'*\' ]]; then
            v="${v:1:${#v}-2}"
        fi
        printf '%s\n' "$v"
        return 0
    done <"$OS_RELEASE_FILE"
}

detect_distro() {
    local os_id os_id_like pretty has_omarchy=0
    os_id="$(os_release_field ID | tr '[:upper:]' '[:lower:]')"
    os_id_like="$(os_release_field ID_LIKE | tr '[:upper:]' '[:lower:]')"
    pretty="$(os_release_field PRETTY_NAME)"

    if [[ "${SUPER_DESKTOP_FORCE_NO_OMARCHY:-0}" != "1" ]] && {
        command -v omarchy >/dev/null 2>&1 ||
            [[ -d /usr/share/omarchy || -d "$HOME/.local/share/omarchy" || "$os_id" == "omarchy" ]]
    }; then
        has_omarchy=1
    fi

    if ((has_omarchy)) && command -v pacman >/dev/null 2>&1; then
        DISTRO_FAMILY="omarchy"
        DISTRO_NAME="${pretty:-Omarchy}"
        IS_OMARCHY=1
    elif [[ "$os_id" == "arch" || " $os_id_like " == *" arch "* ]]; then
        command -v pacman >/dev/null 2>&1 || die "Arch-based distribution detected, but pacman was not found."
        DISTRO_FAMILY="arch"
        DISTRO_NAME="${pretty:-Arch Linux}"
        IS_OMARCHY=0
    elif [[ "$os_id" == "fedora" || " $os_id_like " == *" fedora "* ]]; then
        command -v dnf >/dev/null 2>&1 || die "Fedora-based distribution detected, but dnf was not found."
        DISTRO_FAMILY="fedora"
        DISTRO_NAME="${pretty:-Fedora}"
        IS_OMARCHY=0
    elif [[ "$os_id" == "ubuntu" || "$os_id" == "debian" || "$os_id" == "pop" || "$os_id" == "linuxmint" || "$os_id" == "neon" || "$os_id" == "elementary" || "$os_id" == "zorin" || " $os_id_like " == *" debian "* || " $os_id_like " == *" ubuntu "* ]]; then
        command -v apt-get >/dev/null 2>&1 || die "Debian/Ubuntu-based distribution detected, but apt-get was not found."
        DISTRO_FAMILY="debian"
        DISTRO_NAME="${pretty:-Debian/Ubuntu}"
        IS_OMARCHY=0
    elif [[ "$os_id" == opensuse* || "$os_id" == "sles" || " $os_id_like " == *" suse "* ]]; then
        command -v zypper >/dev/null 2>&1 || die "openSUSE-based distribution detected, but zypper was not found."
        DISTRO_FAMILY="opensuse"
        DISTRO_NAME="${pretty:-openSUSE}"
        IS_OMARCHY=0
    elif command -v pacman >/dev/null 2>&1; then
        DISTRO_FAMILY="arch"
        DISTRO_NAME="${pretty:-Arch Linux}"
        IS_OMARCHY=0
    elif command -v dnf >/dev/null 2>&1; then
        DISTRO_FAMILY="fedora"
        DISTRO_NAME="${pretty:-Fedora}"
        IS_OMARCHY=0
    elif command -v apt-get >/dev/null 2>&1; then
        DISTRO_FAMILY="debian"
        DISTRO_NAME="${pretty:-Debian/Ubuntu}"
        IS_OMARCHY=0
    elif command -v zypper >/dev/null 2>&1; then
        DISTRO_FAMILY="opensuse"
        DISTRO_NAME="${pretty:-openSUSE}"
        IS_OMARCHY=0
    else
        die "Unsupported Linux distribution (${pretty:-unknown}). Supported: Omarchy, Arch Linux, Fedora 45+, Ubuntu 26.04+, Debian sid, openSUSE Tumbleweed (needs GTK4 >= $MIN_GTK4, VTE4 >= $MIN_VTE4, gtk4-layer-shell, Qt6, Rust >= $MIN_RUST)."
    fi

    say "Detected Linux distribution: $DISTRO_NAME ($DISTRO_FAMILY)"
}

preflight() {
    [[ "$(uname -s)" == Linux ]] || die "SUPER DESKTOP runs on Linux (Omarchy, Arch, Fedora 45+, Ubuntu 26.04+, Debian sid, openSUSE Tumbleweed)."
    if ((EUID == 0)); then
        die "Run the installer as your normal user, not as root or with sudo. It asks for your password when it installs packages."
    fi
    detect_distro
}

# A package that arrived as another program's dependency is removed together
# with that program (`pacman -Rns`, which a system update may run), and the app
# then no longer starts. Record the ones the app needs as installed on purpose.
keep_packages() {
    local dependencies=()
    mapfile -t dependencies < <(pacman -Qdq "${PACKAGES[@]}" 2>/dev/null || true)
    ((${#dependencies[@]} > 0)) || return 0
    if command -v sudo >/dev/null 2>&1 &&
        sudo pacman -D --asexplicit "${dependencies[@]}" </dev/null >/dev/null; then
        say "✓ Kept as needed by SUPER DESKTOP: ${dependencies[*]}"
    else
        warn "Could not mark ${dependencies[*]} as needed. Removing another program may remove them too; keep them with 'sudo pacman -D --asexplicit ${dependencies[*]}'."
    fi
}

install_packages_pacman() {
    local missing=()
    local needed plan status
    if needed="$(pacman -T "${PACKAGES[@]}")"; then
        say "✓ System packages present"
        keep_packages
        return
    elif [[ $? != 127 || -z "$needed" ]]; then
        die "Could not check installed packages with pacman."
    fi
    mapfile -t missing <<<"$needed"
    command -v sudo >/dev/null 2>&1 ||
        die "sudo not found. Install the packages as root, then run the installer again: pacman -S --needed ${missing[*]}"

    # Resolve against this machine's package database before starting sudo.
    # Pacman writes parts of a dependency diagnostic to both output streams.
    set +e
    plan="$(LC_ALL=C pacman -Sp --needed --noconfirm "${missing[@]}" </dev/null 2>&1)"
    status=$?
    set -e
    if ((status != 0)); then
        printf '%s\n' "$plan" >&2
        if [[ "$plan" == *" breaks dependency "* ]] ||
            [[ "$plan" == *"unable to satisfy dependency"* ]] ||
            [[ "$plan" == *"could not satisfy dependencies"* ]]; then
            die "Pacman could not resolve ${missing[*]} without a system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
        fi
        die "Pacman could not resolve ${missing[*]}."
    fi
    say "Installing system packages: ${missing[*]}"
    sudo pacman -S --needed --noconfirm "${missing[@]}" </dev/null ||
        die "Could not install ${missing[*]}. See the pacman error above."
    say "✓ Installed ${missing[*]}"
    keep_packages
}

strip_pkg_version() {
    local ver="${1#*:}"
    printf '%s\n' "${ver%%[-+~]*}"
}

check_native_libs() {
    command -v pkg-config >/dev/null 2>&1 || die "pkg-config is required to verify GTK4/VTE4 development libraries."
    local gtk_ver vte_ver
    gtk_ver="$(pkg-config --modversion gtk4 2>/dev/null || true)"
    vte_ver="$(pkg-config --modversion vte-2.91-gtk4 2>/dev/null || true)"
    [[ -n "$gtk_ver" ]] && version_at_least "$(strip_pkg_version "$gtk_ver")" "$MIN_GTK4" ||
        die "$DISTRO_NAME provides GTK4 ${gtk_ver:-missing}, but SUPER DESKTOP requires gtk4 >= $MIN_GTK4."
    [[ -n "$vte_ver" ]] && version_at_least "$(strip_pkg_version "$vte_ver")" "$MIN_VTE4" ||
        die "$DISTRO_NAME provides vte-2.91-gtk4 ${vte_ver:-missing}, but SUPER DESKTOP requires vte-2.91-gtk4 >= $MIN_VTE4."
    pkg-config --exists gtk4-layer-shell-0 2>/dev/null ||
        die "$DISTRO_NAME is missing gtk4-layer-shell-0 development files."
    say "✓ Native libraries ready (gtk4 $gtk_ver, vte-2.91-gtk4 $vte_ver, gtk4-layer-shell)"
}

install_packages_fedora() {
    command -v sudo >/dev/null 2>&1 || die "sudo is required to install Fedora packages."
    local pkgs=(
        gcc gcc-c++ make pkgconf-pkg-config git curl rust cargo
        gtk4-devel gtk4-layer-shell-devel vte291-gtk4-devel
        qt6-qtbase-devel qt6-qtdeclarative-devel
        tmux wl-clipboard libnotify sqlite avahi bubblewrap poppler
        gstreamer1-plugins-good
    )
    say "Installing Fedora build & runtime packages via dnf..."
    sudo dnf install -y "${pkgs[@]}" </dev/null ||
        die "Could not install Fedora packages via dnf."
    sudo dnf install -y gstreamer1-plugin-libav </dev/null >/dev/null 2>&1 ||
        warn "Optional GStreamer libav plugin (gstreamer1-plugin-libav) could not be installed; video previews may need RPM Fusion."
    check_native_libs
}

apt_candidate_version() {
    LC_ALL=C apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ { print $2; exit }'
}

install_packages_debian() {
    command -v sudo >/dev/null 2>&1 || die "sudo is required to install Debian/Ubuntu packages."
    say "Updating apt package index..."
    sudo apt-get update -qq </dev/null ||
        die "Could not update apt package lists."

    local gtk_cand vte_cand ls_cand
    gtk_cand="$(apt_candidate_version libgtk-4-dev)"
    vte_cand="$(apt_candidate_version libvte-2.91-gtk4-dev)"
    ls_cand="$(apt_candidate_version libgtk4-layer-shell-dev)"

    if [[ -z "$gtk_cand" || "$gtk_cand" == "(none)" ]] || ! version_at_least "$(strip_pkg_version "$gtk_cand")" "$MIN_GTK4" ||
        [[ -z "$vte_cand" || "$vte_cand" == "(none)" ]] || ! version_at_least "$(strip_pkg_version "$vte_cand")" "$MIN_VTE4" ||
        [[ -z "$ls_cand" || "$ls_cand" == "(none)" ]]; then
        die "$DISTRO_NAME repositories do not meet SUPER DESKTOP's minimum library versions (found libgtk-4-dev=${gtk_cand:-missing}, libvte-2.91-gtk4-dev=${vte_cand:-missing}, libgtk4-layer-shell-dev=${ls_cand:-missing}; requires gtk4 >= $MIN_GTK4, vte-2.91-gtk4 >= $MIN_VTE4, gtk4-layer-shell). Supported Debian/Ubuntu releases: Ubuntu 26.04 LTS+ and Debian sid/forky+."
    fi

    local pkgs=(
        build-essential pkg-config git curl rustc cargo
        libgtk-4-dev libvte-2.91-gtk4-dev libgtk4-layer-shell-dev
        qt6-base-dev qt6-declarative-dev qmake6
        tmux wl-clipboard libnotify-bin sqlite3 avahi-daemon bubblewrap poppler-utils
        gstreamer1.0-plugins-good gstreamer1.0-libav adwaita-icon-theme
    )
    say "Installing Debian/Ubuntu build & runtime packages via apt-get..."
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${pkgs[@]}" </dev/null ||
        die "Could not install Debian/Ubuntu packages via apt-get."
    check_native_libs
}

install_packages_opensuse() {
    command -v sudo >/dev/null 2>&1 || die "sudo is required to install openSUSE packages."
    local pkgs=(
        gcc gcc-c++ make pkgconf-pkg-config gawk git curl rust cargo
        gtk4-devel gtk4-layer-shell-devel vte-devel
        qt6-base-devel qt6-declarative-devel
        tmux wl-clipboard libnotify-tools sqlite3 avahi bubblewrap poppler-tools
        gstreamer-plugins-good adwaita-icon-theme liberation-fonts gsettings-desktop-schemas
    )
    say "Installing openSUSE build & runtime packages via zypper..."
    sudo zypper --non-interactive install "${pkgs[@]}" </dev/null ||
        die "Could not install openSUSE packages via zypper."
    sudo zypper --non-interactive install gstreamer-plugins-libav </dev/null >/dev/null 2>&1 ||
        warn "Optional GStreamer libav plugin (gstreamer-plugins-libav) could not be installed."
    check_native_libs
}

install_packages() {
    case "$DISTRO_FAMILY" in
    omarchy | arch) install_packages_pacman ;;
    fedora) install_packages_fedora ;;
    debian) install_packages_debian ;;
    opensuse) install_packages_opensuse ;;
    *) die "Unsupported distribution family: $DISTRO_FAMILY" ;;
    esac
}

rust_version() {
    { rustc --version 2>/dev/null || true; } | awk '{ print $2 }'
}

# True when version $1 is $2 or newer.
version_at_least() {
    [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n 1)" == "$2" ]]
}

rust_usable() {
    command -v cargo >/dev/null 2>&1 && cargo --version >/dev/null 2>&1 &&
        version_at_least "$(rust_version)" "$MIN_RUST"
}

# The official rustup-init for this machine, checked against its published
# SHA-256. --no-modify-path leaves shell profiles alone; rebuild.sh finds
# cargo in ~/.cargo/bin by itself.
install_rustup() {
    local arch work url want have
    arch="$(uname -m)"
    case "$arch" in
    x86_64 | aarch64) ;;
    *) die "rustup has no build for $arch. Install Rust $MIN_RUST or newer (https://rustup.rs), then run the installer again." ;;
    esac
    say "Installing Rust (rustup, stable toolchain) into ${CARGO_HOME:-~/.cargo} and ${RUSTUP_HOME:-~/.rustup}..."
    work="$(mktemp -d)"
    url="https://static.rust-lang.org/rustup/dist/$arch-unknown-linux-gnu/rustup-init"
    if ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/rustup-init" "$url" ||
        ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/rustup-init.sha256" "$url.sha256"; then
        rm -rf "$work"
        die "Could not download rustup from $url. Check the network connection and run the installer again."
    fi
    want="$(cut -d ' ' -f 1 "$work/rustup-init.sha256")"
    have="$(sha256sum "$work/rustup-init" | cut -d ' ' -f 1)"
    if [[ -z "$want" || "$want" != "$have" ]]; then
        rm -rf "$work"
        die "The rustup download does not match its checksum. Run the installer again."
    fi
    chmod +x "$work/rustup-init"
    # Progress goes to stderr. Stdout only repeats PATH advice for a shell
    # profile this install leaves alone.
    if ! "$work/rustup-init" -y --no-modify-path --profile minimal --default-toolchain stable </dev/null >/dev/null; then
        rm -rf "$work"
        die "rustup could not install the stable Rust toolchain. See the messages above."
    fi
    rm -rf "$work"
    RUST_INSTALLED=1
}

ensure_rust() {
    local cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    # A rustup installed without editing profiles is not on PATH yet.
    if ! rust_usable && [[ -x "$cargo_bin/cargo" ]]; then
        PATH="$cargo_bin:$PATH"
    fi
    if rust_usable; then
        say "✓ Rust $(rust_version)"
        return
    fi
    if command -v rustup >/dev/null 2>&1; then
        # A fresh rustup has no default toolchain, so its cargo proxy refuses
        # to run ("rustup could not choose a version of cargo to run").
        if cargo --version >/dev/null 2>&1; then
            say "Updating Rust $(rust_version) (SUPER DESKTOP needs $MIN_RUST or newer)..."
            rustup update stable </dev/null || true
        else
            say "No default Rust toolchain is set. Installing and selecting stable..."
            rustup default stable </dev/null || true
        fi
        if ! rust_usable; then
            local found
            found="$(rust_version)"
            die "SUPER DESKTOP needs Rust $MIN_RUST or newer, and rustup's default is ${found:-not a working toolchain}. Run 'rustup default stable', then run the installer again."
        fi
    else
        install_rustup
        PATH="$cargo_bin:$PATH"
        rust_usable || die "Rust was installed into $cargo_bin, but cargo does not run from there."
    fi
    say "✓ Rust $(rust_version)"
}

is_checkout() {
    [[ -f "$1/Cargo.toml" ]] && grep -q '^name = "super-desktop"$' "$1/Cargo.toml"
}

# The clone holding this script, when it runs from one (./install.sh).
script_checkout() {
    local dir
    [[ -n "$SELF" && -f "$SELF" ]] || return 1
    dir="$(cd "$(dirname "$SELF")" && pwd)" || return 1
    is_checkout "$dir" || return 1
    printf '%s\n' "$dir"
}

# The clone an existing install runs from: ~/.local/bin/super-desktop links
# into its target/release (or, from older installs, its bin/ launcher).
installed_checkout() {
    local target dir
    [[ -L "$BIN_DST" ]] || return 1
    target="$(readlink -f "$BIN_DST")" || return 1
    case "$target" in
    */target/release/super-desktop-client) dir="${target%/target/release/super-desktop-client}" ;;
    */bin/super-desktop) dir="${target%/bin/super-desktop}" ;;
    *) return 1 ;;
    esac
    is_checkout "$dir" || return 1
    printf '%s\n' "$dir"
}

# SUPER_DESKTOP_VERSION as "", "latest", "master" or a tag vX.Y.Z (1.1.17 is
# v1.1.17). A branch named with SUPER_DESKTOP_BRANCH is followed.
resolve_release() {
    if [[ ! "$BRANCH" =~ ^[A-Za-z0-9._/-]+$ || "$BRANCH" == -* || "$BRANCH" == *..* ]]; then
        die "SUPER_DESKTOP_BRANCH contains invalid characters."
    fi
    if [[ "$REPO_URL" == -* || "$REPO_URL" =~ [[:space:]] ]] ||
        [[ ! "$REPO_URL" =~ ^(https://|ssh://|git@) ]]; then
        die "SUPER_DESKTOP_REPO must be an https://, ssh://, or git@ repository URL."
    fi
    if [[ -z "$RELEASE" && -n "${SUPER_DESKTOP_BRANCH:-}" ]]; then
        RELEASE=master
    fi
    case "$RELEASE" in
    "" | latest | master) return ;;
    esac
    RELEASE="v${RELEASE#v}"
    [[ "$RELEASE" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
        die "SUPER_DESKTOP_VERSION must look like v1.1.17, or be \"latest\" or \"master\". See $RELEASES_URL"
}

# The release tag vX.Y.Z that "$1" is pinned to (HEAD detached exactly on it).
pinned_release() {
    local tag
    git -C "$1" symbolic-ref -q HEAD >/dev/null 2>&1 && return 1
    tag="$(git -C "$1" tag --points-at HEAD | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -n 1)"
    [[ -n "$tag" ]] || return 1
    printf '%s\n' "$tag"
}

channel_of() {
    git -C "$1" config --get "$CHANNEL_KEY" 2>/dev/null || true
}

set_channel() {
    git -C "$1" config "$CHANNEL_KEY" "$2"
}

# The newest release tag vX.Y.Z on the clone's origin; empty when there is none.
newest_release() {
    local tags
    tags="$(git -C "$1" ls-remote --tags --refs origin 'v*' </dev/null)" ||
        die "Could not read the releases from $REPO_URL. Check the network connection and run the installer again."
    printf '%s\n' "$tags" | sed -n 's|.*refs/tags/\(v[0-9]*\.[0-9]*\.[0-9]*\)$|\1|p' | sort -V | tail -n 1
}

has_local_changes() {
    [[ -n "$(git -C "$1" status --porcelain --untracked-files=no)" ]]
}

# Move the clone to the release tag "$2". A clone with local changes is left
# as it is (status 1); a tag that does not exist stops the install.
checkout_release() {
    local dir="$1" tag="$2"
    if has_local_changes "$dir"; then
        warn "$dir has local changes, so it was not switched to $tag. Installing it as it is."
        return 1
    fi
    git -C "$dir" fetch --quiet --force --no-tags origin '+refs/tags/v*:refs/tags/v*' </dev/null ||
        die "Could not fetch the releases from $REPO_URL. Check the network connection and run the installer again."
    git -C "$dir" rev-parse -q --verify "refs/tags/$tag^{commit}" >/dev/null ||
        die "There is no release $tag. The releases are listed at $RELEASES_URL"
    if [[ "$(git -C "$dir" rev-parse HEAD)" == "$(git -C "$dir" rev-parse "refs/tags/$tag^{commit}")" ]] &&
        ! git -C "$dir" symbolic-ref -q HEAD >/dev/null; then
        say "✓ $dir is on $tag"
        return
    fi
    say "Switching $dir to $tag..."
    git -C "$dir" -c advice.detachedHead=false checkout --quiet --detach "refs/tags/$tag" ||
        die "Could not check out $tag in $dir."
}

# Follow the branch: switch to it, or fast-forward it. A clone with local work
# or its own history is installed as it is (status 1).
follow_branch() {
    local dir="$1"
    if has_local_changes "$dir"; then
        warn "$dir has local changes, so it was not updated. Installing it as it is."
        return 1
    fi
    if [[ "$(git -C "$dir" symbolic-ref -q --short HEAD 2>/dev/null)" == "$BRANCH" ]]; then
        say "Updating $dir ($BRANCH)..."
        if ! git -C "$dir" pull --ff-only </dev/null; then
            warn "Could not fast-forward $dir, so it was not updated. Installing it as it is."
            return 1
        fi
        return
    fi
    say "Switching $dir to $BRANCH..."
    git -C "$dir" fetch --quiet --no-tags origin -- "$BRANCH" </dev/null ||
        die "Could not fetch $BRANCH from $REPO_URL. Check the network connection and run the installer again."
    git -C "$dir" checkout --quiet -B "$BRANCH" "origin/$BRANCH" -- ||
        die "Could not switch $dir to $BRANCH."
    git -C "$dir" branch --quiet --set-upstream-to="origin/$BRANCH" -- "$BRANCH" >/dev/null 2>&1 || true
}

# Move the clone to the newest release. A clone on a branch with commits that
# are not on GitHub is someone's own work: it is installed as it is.
follow_releases() {
    local dir="$1" tag
    tag="$(newest_release "$dir")"
    if [[ -z "$tag" ]]; then
        warn "$REPO_URL has no releases yet, so $dir follows $BRANCH instead."
        follow_branch "$dir" || true
        return
    fi
    if git -C "$dir" symbolic-ref -q HEAD >/dev/null 2>&1 &&
        [[ -n "$(git -C "$dir" rev-list '@{upstream}..HEAD' 2>/dev/null || echo own)" ]]; then
        warn "$dir has commits that are not on GitHub, so it was not switched to $tag. Installing it as it is."
        return
    fi
    checkout_release "$dir" "$tag" && set_channel "$dir" releases
    return 0
}

# A requested version wins. With nothing requested, a pinned clone stays put
# (pinned in its git config, or detached on a release by an older installer
# that recorded nothing), a clone set to follow the branch follows it, and
# every other clone moves to the newest release.
update_checkout() {
    local dir="$1" channel pinned
    case "$RELEASE" in
    master)
        follow_branch "$dir" && set_channel "$dir" branch
        return 0
        ;;
    latest)
        follow_releases "$dir"
        return 0
        ;;
    v*)
        checkout_release "$dir" "$RELEASE" && set_channel "$dir" pinned
        return 0
        ;;
    esac
    channel="$(channel_of "$dir")"
    pinned="$(pinned_release "$dir")" || pinned=""
    if [[ -n "$pinned" && ("$channel" == pinned || -z "$channel") ]]; then
        say "$dir is pinned to $pinned, so it was not updated."
        say "  Newest:  ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=latest bash"
        say "  Another: ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=vX.Y.Z bash   (releases: $RELEASES_URL)"
        return
    fi
    if [[ "$channel" == branch ]]; then
        follow_branch "$dir" || true
        return
    fi
    follow_releases "$dir"
}

prepare_source() {
    local dir
    resolve_release
    if dir="$(script_checkout)"; then
        SRC="$dir"
        say "Installing from this clone: $SRC"
        [[ -z "${SUPER_DESKTOP_VERSION:-}" ]] || warn "SUPER_DESKTOP_VERSION is ignored when installing from a clone: check out the release in the clone yourself."
        return
    fi
    dir="${SUPER_DESKTOP_DIR:-}"
    if [[ -z "$dir" ]]; then
        dir="$(installed_checkout)" || dir="$DEFAULT_DIR"
    fi
    if [[ -e "$dir/.git" ]]; then
        is_checkout "$dir" ||
            die "$dir is not a SUPER DESKTOP clone. Set SUPER_DESKTOP_DIR to another folder."
        update_checkout "$dir"
    elif [[ -e "$dir" && -n "$(ls -A "$dir" 2>/dev/null)" ]]; then
        die "$dir exists and is not a SUPER DESKTOP clone. Move it away or set SUPER_DESKTOP_DIR to another folder."
    else
        say "Downloading SUPER DESKTOP into $dir..."
        mkdir -p "$(dirname "$dir")"
        # Clone the branch, then check the release out: `git clone --branch <tag>`
        # prints a confusing "is not a commit!" warning for an annotated tag.
        git -c advice.detachedHead=false clone --branch "$BRANCH" -- "$REPO_URL" "$dir" </dev/null ||
            die "Could not clone $REPO_URL. Check the network connection and run the installer again."
        update_checkout "$dir"
    fi
    SRC="$(cd "$dir" && pwd)"
}

build() {
    say "Building SUPER DESKTOP (release). A first build takes several minutes..."
    # The installed command links into this clone's target/release.
    unset CARGO_TARGET_DIR
    cargo build --release --locked --manifest-path "$SRC/Cargo.toml" </dev/null ||
        die "The build failed. See the messages above, then run the installer again."
    [[ -x "$SRC/target/release/super-desktop" && -x "$SRC/target/release/super-desktop-client" ]] ||
        die "The build produced no binaries in $SRC/target/release."
    say "✓ Built $SRC/target/release/super-desktop"
    # QML plugin surfaces. Optional: without Qt Quick the overlay runs without them.
    "$SRC/scripts/build-qml-host.sh" release </dev/null ||
        say "    (the QML plugin host did not build; plugins stay off)"
}

configure() {
    local bin_src="$SRC/target/release/super-desktop-client"

    # 1. Link the executable into ~/.local/bin.
    mkdir -p "$HOME/.local/bin" "$CONFIG_DIR" "$(dirname "$APP_DST")"
    chmod +x "$bin_src"
    ln -sf "$bin_src" "$BIN_DST"
    say "✓ Linked $BIN_DST -> $bin_src"

    # 1b. Install vendored toolbar logos (company SVGs).
    if [[ -d "$SRC/assets" ]]; then
        mkdir -p "$CONFIG_DIR/assets"
        cp -r "$SRC/assets/." "$CONFIG_DIR/assets/"
        say "✓ Installed toolbar assets to $CONFIG_DIR/assets"
    fi

    # 2. Install desktop entry.
    EXEC_LINE="Exec=$BIN_DST toggle" awk '{
        if ($0 ~ /^Exec=/) { print ENVIRON["EXEC_LINE"] } else { print }
    }' "$SRC/super-desktop.desktop" >"$APP_DST"
    say "✓ Installed desktop entry to $APP_DST"

    if ((IS_OMARCHY == 1)) || [[ -f "$HYPRLAND_LUA" ]]; then
        # 3. Add the overlay's toggle binding to ~/.config/hypr/bindings.lua.
        #
        # Everything between the two markers below belongs to the app: the
        # overlay's ⚙ Settings panel rewrites this block when the user records
        # another shortcut, and finds it again by the markers. src/shortcut.rs
        # holds the same two strings (MANAGED_BEGIN/MANAGED_END) — change them in
        # both places or in neither.
        local bind_marker='-- >>> super-desktop shortcut (managed by the overlay settings) >>>'
        mkdir -p "$(dirname "$BINDINGS_LUA")"
        touch "$BINDINGS_LUA"
        # -e: the marker starts with "--", which grep would read as an option.
        if ! grep -qF -e "$bind_marker" "$BINDINGS_LUA"; then
            # Older runs appended loose o.bind(...) lines together with the
            # hl.unbind(...) lines that cleared the way for them. Drop those
            # first: otherwise a re-run leaves two blocks fighting over the same
            # shortcut (and the unbinds, which can outlive their bind, would keep
            # an unused key dead).
            if grep -qF "super-desktop toggle" "$BINDINGS_LUA"; then
                local bindings_tmp
                bindings_tmp="$(mktemp)"
                grep -v -F \
                    -e "super-desktop toggle" \
                    -e '-- SUPER DESKTOP: Sticky notes and AI agent terminal overlay' \
                    -e 'hl.unbind("SUPER + SHIFT + q")' \
                    -e 'hl.unbind("SUPER + SHIFT + Cyrillic_shorti")' \
                    -e 'hl.unbind("SUPER + SHIFT + Cyrillic_SHORTI")' \
                    -e 'hl.unbind("SUPER + SHIFT + code:24")' \
                    "$BINDINGS_LUA" >"$bindings_tmp" || true
                cat "$bindings_tmp" >"$BINDINGS_LUA" # in place: keeps mode and owner
                rm -f "$bindings_tmp"
                say "✓ Migrated the loose super-desktop bind lines in $BINDINGS_LUA"
            fi
            cat >>"$BINDINGS_LUA" <<'EOF'

-- >>> super-desktop shortcut (managed by the overlay settings) >>>
-- Set in the overlay: ⚙ Settings → Keyboard shortcut.
-- Rewritten there on every change; edits inside this block are lost.
hl.unbind("SUPER + SHIFT + Q")
o.bind("SUPER + SHIFT + Q", "Super Desktop", hl.dsp.no_op())
o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle", { release = true })
hl.unbind("SUPER + SHIFT + code:24")
o.bind("SUPER + SHIFT + code:24", "Super Desktop", hl.dsp.no_op())
o.bind("SUPER + SHIFT + code:24", "Super Desktop", "super-desktop toggle", { release = true })
-- <<< super-desktop shortcut <<<
EOF
            say "✓ Added the SUPER + SHIFT + Q binding to $BINDINGS_LUA"
        else
            say "✓ Hyprland keybinding already present in $BINDINGS_LUA"
        fi

        # 3b. Keep the daemon warm so the shortcut does not have to spawn it.
        # `daemon` (not `start`) comes up hidden: the overlay is only built on the
        # first toggle, but the process, GTK and the IPC socket are already there.
        # Omarchy loads autostart.lua from hyprland.lua; an older install put the
        # line in bindings.lua, which still counts.
        local autostart_lua="$HOME/.config/hypr/autostart.lua"
        if grep -qs "super-desktop daemon" "$BINDINGS_LUA" "$autostart_lua"; then
            say "✓ Warm daemon autostart already present"
        else
            mkdir -p "$(dirname "$autostart_lua")"
            printf '\n-- SUPER DESKTOP: start the daemon (and its harnesses and phone bridge) at login.\no.exec_on_start("super-desktop daemon")\n' >>"$autostart_lua"
            say "✓ Added warm daemon autostart to $autostart_lua"
        fi

        # 4. Add Layer Rule for blur effect to ~/.config/hypr/hyprland.lua.
        # The overlay animates its own slide. Without no_anim Hyprland also fades
        # the layer (Omarchy: ~180ms in, ~140ms more after the unmap), on every
        # toggle.
        local layer_rule_old='hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true })'
        local layer_rule='hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true, no_anim = true, animation = "none" })'
        mkdir -p "$(dirname "$HYPRLAND_LUA")"
        touch "$HYPRLAND_LUA"
        if grep -qxF "$layer_rule_old" "$HYPRLAND_LUA"; then
            # Only the exact line this script used to write; a customized rule is kept.
            local migrated
            migrated="$(awk -v old="$layer_rule_old" -v new="$layer_rule" '$0 == old { print new; next } { print }' "$HYPRLAND_LUA")"
            printf '%s\n' "$migrated" >"$HYPRLAND_LUA"
            say "✓ Turned off Hyprland's layer fade for the overlay in $HYPRLAND_LUA"
        elif ! grep -q "super-desktop" "$HYPRLAND_LUA"; then
            {
                echo ""
                echo "-- Super Desktop overlay blur effect"
                echo "$layer_rule"
            } >>"$HYPRLAND_LUA"
            say "✓ Added layer rule to $HYPRLAND_LUA"
        else
            say "✓ Hyprland layer rule already present in $HYPRLAND_LUA"
        fi

        # 5. Install Omarchy theme-set hook for instant theme synchronization.
        local hook_dir="$HOME/.config/omarchy/hooks/theme-set.d"
        local hook_file="$hook_dir/super-desktop"
        mkdir -p "$hook_dir"
        cat <<'EOF' >"$hook_file"
#!/usr/bin/env bash
BIN="$HOME/.local/bin/super-desktop"
if [[ -x "$BIN" ]]; then
    "$BIN" reload-theme >/dev/null 2>&1 || true
elif command -v super-desktop >/dev/null 2>&1; then
    super-desktop reload-theme >/dev/null 2>&1 || true
fi
EOF
        chmod 700 "$hook_file"
        say "✓ Installed Omarchy theme hook to $hook_file"
    else
        configure_non_omarchy_compositor "$BIN_DST"
    fi
}

hyprland_reachable() {
    command -v hyprctl >/dev/null 2>&1 && hyprctl version >/dev/null 2>&1
}

reload_hyprland() {
    if ! hyprland_reachable; then
        say "Hyprland is not reachable from this shell; the shortcut works from your next Hyprland login."
        return
    fi
    say "Reloading Hyprland configuration..."
    hyprctl reload >/dev/null || true
    local errors
    errors="$(hyprctl configerrors 2>/dev/null || true)"
    if [[ -n "$errors" && "$errors" != "ok" && "$errors" != "OK" ]]; then
        warn "Hyprland reported config error: $errors"
    else
        say "✓ Hyprland configuration validated cleanly!"
    fi
}

# The daemon exactly: its binary as the first word, then `daemon` (the same
# pattern rebuild.sh stops it with). A looser one also matches any process
# whose command text mentions both words.
DAEMON_PROCESS='^([^ ]*/)?super-desktop daemon( |$)'
BRIDGE_PROCESS='^([^ ]*/)?super-desktop harness-bridge( |$)'
TELEGRAM_PROCESS='^([^ ]*/)?super-desktop telegram-bot( |$)'

daemon_running() {
    pgrep -u "$(id -u)" -f "$DAEMON_PROCESS" >/dev/null 2>&1
}

supported_binary_arch() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
    x86_64 | aarch64) printf '%s\n' "$arch" ;;
    *) return 1 ;;
    esac
}

pacman_repo_available() {
    local arch="$1"
    [[ "$REPO_KEY_FINGERPRINT" =~ ^[0-9A-F]{40}$ ]] || return 1
    curl --proto '=https' --tlsv1.2 -fsSI "$PACMAN_REPO_URL/$arch/$PACMAN_REPO_NAME.db" >/dev/null 2>&1
}

import_repo_key() {
    [[ "$REPO_KEY_FINGERPRINT" =~ ^[0-9A-F]{40}$ ]] ||
        die "Package signing key fingerprint is not configured."
    if pacman-key --list-keys "$REPO_KEY_FINGERPRINT" >/dev/null 2>&1; then
        say "✓ Package signing key already trusted ($REPO_KEY_FINGERPRINT)"
        return 0
    fi
    command -v gpg >/dev/null 2>&1 ||
        die "gpg is required to verify the package signing key."
    command -v sudo >/dev/null 2>&1 ||
        die "sudo is required to trust the package signing key."
    local work keyfile gpghome match
    work="$(mktemp -d)"
    keyfile="$work/super-desktop.gpg"
    gpghome="$work/gnupg"
    mkdir -p "$gpghome"
    chmod 700 "$gpghome"
    if ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$keyfile" "$PACMAN_REPO_URL/super-desktop.gpg"; then
        rm -rf "$work"
        die "Could not download the package signing key from $PACMAN_REPO_URL/super-desktop.gpg."
    fi
    match="$(
        GNUPGHOME="$gpghome" gpg --batch --show-keys --with-colons "$keyfile" 2>/dev/null |
            awk -F: '$1 == "pub" { in_pub = 1; next } $1 == "sub" { in_pub = 0 } in_pub && $1 == "fpr" { print $10; in_pub = 0 }' |
            grep -Fx "$REPO_KEY_FINGERPRINT" || true
    )"
    if [[ -z "$match" ]]; then
        rm -rf "$work"
        die "Downloaded signing key does not match expected fingerprint $REPO_KEY_FINGERPRINT."
    fi
    if ! sudo pacman-key --add "$keyfile" </dev/null ||
        ! sudo pacman-key --lsign-key "$REPO_KEY_FINGERPRINT" </dev/null; then
        rm -rf "$work"
        die "Could not add or trust the SUPER DESKTOP package signing key."
    fi
    rm -rf "$work"
    say "✓ Trusted SUPER DESKTOP package signing key ($REPO_KEY_FINGERPRINT)"
}

configure_pacman_repo() {
    command -v sudo >/dev/null 2>&1 ||
        die "sudo is required to register the SUPER DESKTOP pacman repository."
    local work repo_tmp conf_tmp
    work="$(mktemp -d)"
    repo_tmp="$work/super-desktop.conf"
    cat >"$repo_tmp" <<EOF
[$PACMAN_REPO_NAME]
SigLevel = Required DatabaseRequired
Server = $PACMAN_REPO_URL/\$arch
EOF
    sudo mkdir -p "$(dirname "$PACMAN_REPO_CONF")" </dev/null
    sudo install -m 644 "$repo_tmp" "$PACMAN_REPO_CONF" </dev/null || {
        rm -rf "$work"
        die "Could not write $PACMAN_REPO_CONF."
    }

    if ! grep -qxF "Include = $PACMAN_REPO_CONF" "$PACMAN_CONF" 2>/dev/null; then
        conf_tmp="$work/pacman.conf"
        if grep -q '^\[omarchy\]' "$PACMAN_CONF" 2>/dev/null; then
            awk -v inc="Include = $PACMAN_REPO_CONF" '
                !done && /^\[omarchy\]/ { print inc; print ""; done = 1 }
                { print }
            ' "$PACMAN_CONF" >"$conf_tmp"
        else
            {
                [[ ! -f "$PACMAN_CONF" ]] || cat "$PACMAN_CONF"
                printf '\nInclude = %s\n' "$PACMAN_REPO_CONF"
            } >"$conf_tmp"
        fi
        sudo install -m 644 "$conf_tmp" "$PACMAN_CONF" </dev/null || {
            rm -rf "$work"
            die "Could not add Include = $PACMAN_REPO_CONF to $PACMAN_CONF."
        }
    fi
    rm -rf "$work"

    if ((IS_OMARCHY == 1)); then
        local hook_dir="$HOME/.config/omarchy/hooks/pre-refresh-pacman.d"
        local hook_file="$hook_dir/super-desktop"
        mkdir -p "$hook_dir"
        cat >"$hook_file" <<EOF
#!/bin/bash
CONF=$PACMAN_CONF
SNIPPET=$PACMAN_REPO_CONF
MARKER="Include = \$SNIPPET"
[[ -r \$SNIPPET ]] || exit 0
grep -qxF "\$MARKER" "\$CONF" && exit 0
if grep -q '^\[omarchy\]' "\$CONF"; then
    sudo sed -i "0,/^\[omarchy\]/s||\$MARKER\n\n[omarchy]|" "\$CONF"
else
    printf '\n%s\n' "\$MARKER" | sudo tee -a "\$CONF" >/dev/null
fi
EOF
        chmod 755 "$hook_file"
        say "✓ Configured [$PACMAN_REPO_NAME] pacman repository and Omarchy refresh hook"
    else
        say "✓ Configured [$PACMAN_REPO_NAME] pacman repository"
    fi
}

sync_super_desktop_db() {
    local work sync_conf
    work="$(mktemp -d)"
    sync_conf="$work/pacman-super-desktop.conf"
    # Keep [options] from pacman.conf (Architecture, DownloadUser, GPGDir, DBPath, etc.)
    # while excluding all other repositories so only [$PACMAN_REPO_NAME] is synced.
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
    printf '\nInclude = %s\n' "$PACMAN_REPO_CONF" >>"$sync_conf"
    chmod 644 "$sync_conf"
    chmod 755 "$work"
    if ! sudo pacman --config "$sync_conf" -Sy --noconfirm </dev/null; then
        rm -rf "$work"
        die "Could not synchronize the [$PACMAN_REPO_NAME] package database."
    fi
    rm -rf "$work"
}

resolve_and_install_pacman_target() {
    local mode="$1" target="$2"
    local plan status pkg pkgs upgraded=()
    command -v sudo >/dev/null 2>&1 ||
        die "sudo is required to install super-desktop-bin."

    if [[ "$mode" == "-S" ]]; then
        sync_super_desktop_db
        set +e
        plan="$(LC_ALL=C pacman -Sp --needed --noconfirm "$target" </dev/null 2>&1)"
        status=$?
        set -e
        if ((status != 0)); then
            [[ -z "$plan" ]] || printf '%s\n' "$plan" >&2
            die "Pacman could not resolve $target without a system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
        fi
        pkgs="$(LC_ALL=C pacman -Sp --print-format '%n' --needed --noconfirm "$target" </dev/null 2>/dev/null || true)"
        for pkg in $pkgs; do
            if [[ "$pkg" != "super-desktop-bin" ]] && pacman -Q "$pkg" >/dev/null 2>&1; then
                upgraded+=("$pkg")
            fi
        done
        if ((${#upgraded[@]} > 0)); then
            die "Installing $target requires upgrading already-installed system packages (${upgraded[*]}). This installer does not run a full system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
        fi
        say "Installing prebuilt super-desktop-bin package..."
        sudo pacman -S --needed --noconfirm "$target" </dev/null ||
            die "Could not install super-desktop-bin without a system upgrade. If a mirror returned 404 for an older dependency, update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
        say "✓ Installed super-desktop-bin"
        return 0
    fi

    set +e
    plan="$(LC_ALL=C pacman -Up --noconfirm "$target" </dev/null 2>&1)"
    status=$?
    set -e
    if ((status != 0)); then
        [[ -z "$plan" ]] || printf '%s\n' "$plan" >&2
        die "Pacman could not resolve $(basename "$target") without a system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
    fi
    pkgs="$(LC_ALL=C pacman -Up --print-format '%n' --noconfirm "$target" </dev/null 2>/dev/null || true)"
    for pkg in $pkgs; do
        if [[ "$pkg" != "super-desktop-bin" ]] && pacman -Q "$pkg" >/dev/null 2>&1; then
            upgraded+=("$pkg")
        fi
    done
    if ((${#upgraded[@]} > 0)); then
        die "Installing $(basename "$target") requires upgrading already-installed system packages (${upgraded[*]}). This installer does not run a full system upgrade. Update your system first (omarchy update or sudo pacman -Syu), then run the installer again."
    fi
    say "Installing prebuilt package $(basename "$target")..."
    sudo pacman -U --noconfirm "$target" </dev/null ||
        die "Could not install $(basename "$target"). See the pacman error above."
    say "✓ Installed $(basename "$target")"
}

install_from_pacman_repo() {
    local arch="$1"
    import_repo_key
    configure_pacman_repo
    resolve_and_install_pacman_target -S "super-desktop-bin"
}

install_pinned_package() {
    local version="$1" arch="$2"
    local ver pkg_name base_url work want have
    ver="${version#v}"
    pkg_name="super-desktop-bin-${ver}-1-${arch}.pkg.tar.zst"
    base_url="${RELEASE_BASE_URL%/}/$version"
    work="$(mktemp -d)"

    if ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$pkg_name" "$base_url/$pkg_name" 2>/dev/null ||
        ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/SHA256SUMS" "$base_url/SHA256SUMS" 2>/dev/null; then
        rm -rf "$work"
        return 1
    fi

    want="$(awk -v file="$pkg_name" '$2 == file || $2 == ("*" file) { print $1; exit }' "$work/SHA256SUMS")"
    if [[ ! "$want" =~ ^[0-9a-f]{64}$ ]]; then
        rm -rf "$work"
        die "SHA256SUMS for $version does not contain a valid checksum for $pkg_name."
    fi
    have="$(sha256sum "$work/$pkg_name" | awk '{ print $1 }')"
    if [[ "$want" != "$have" ]]; then
        rm -rf "$work"
        die "Checksum mismatch for $pkg_name (expected $want, got $have)."
    fi

    if [[ "$REPO_KEY_FINGERPRINT" =~ ^[0-9A-F]{40}$ ]] &&
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$pkg_name.sig" "$base_url/$pkg_name.sig" 2>/dev/null; then
        import_repo_key
    fi

    resolve_and_install_pacman_target -U "$work/$pkg_name"
    rm -rf "$work"
    return 0
}

print_compositor_instructions() {
    local bin_cmd="$1"
    say ""
    say "Wayland compositor setup (requires a wlr-layer-shell compositor such as Hyprland, Sway, Niri, or KDE Plasma; GNOME/Mutter and X11 are not supported):"
    say "  Hyprland (~/.config/hypr/hyprland.conf):"
    say "    exec-once = $bin_cmd daemon"
    say "    bindr = SUPER SHIFT, Q, exec, $bin_cmd toggle"
    say "  Sway (~/.config/sway/config):"
    say "    exec $bin_cmd daemon"
    say "    bindsym \$mod+Shift+q exec $bin_cmd toggle"
    say "  Niri (~/.config/niri/config.kdl):"
    say "    spawn-at-startup \"$bin_cmd\" \"daemon\""
    say "    binds { Mod+Shift+Q { spawn \"$bin_cmd\" \"toggle\"; } }"
}

configure_non_omarchy_compositor() {
    local bin_cmd="$1"
    local hypr_conf="$HOME/.config/hypr/hyprland.conf"
    if [[ -f "$hypr_conf" ]]; then
        if ! grep -qF "# >>> super-desktop >>>" "$hypr_conf"; then
            if [[ ! -e "${hypr_conf}.before-super-desktop" ]]; then
                cp -a "$hypr_conf" "${hypr_conf}.before-super-desktop"
                say "✓ Backed up $hypr_conf to ${hypr_conf}.before-super-desktop"
            fi
            cat >>"$hypr_conf" <<EOF

# >>> super-desktop >>>
exec-once = $bin_cmd daemon
bindr = SUPER SHIFT, Q, exec, $bin_cmd toggle
# <<< super-desktop <<<
EOF
            say "✓ Added daemon autostart and SUPER + SHIFT + Q toggle binding to $hypr_conf"
        else
            say "✓ Hyprland binding already present in $hypr_conf"
        fi
    else
        NON_OMARCHY_NEEDS_BINDING=1
    fi
}

configure_non_omarchy_desktop() {
    local bin_cmd="$1" is_packaged="${2:-0}"
    if ((is_packaged)); then
        if [[ -L "$BIN_DST" && "$(readlink "$BIN_DST" 2>/dev/null || true)" != "/usr/bin/super-desktop" ]]; then
            local target
            target="$(readlink -f "$BIN_DST" 2>/dev/null || true)"
            case "$target" in
            */target/release/super-desktop-client | */target/release/super-desktop | */bin/super-desktop)
                mv -f "$BIN_DST" "${BIN_DST}.before-package"
                mkdir -p "$(dirname "$BIN_DST")"
                ln -sf /usr/bin/super-desktop "$BIN_DST"
                say "✓ Migrated $BIN_DST -> /usr/bin/super-desktop"
                ;;
            *)
                warn "Custom launcher left untouched at $BIN_DST"
                ;;
            esac
        fi
        if [[ -f "$APP_DST" ]] && grep -q "super-desktop" "$APP_DST"; then
            mv -f "$APP_DST" "${APP_DST}.before-package"
        fi
    fi
    configure_non_omarchy_compositor "$bin_cmd"
}

configure_packaged_user() {
    if ((IS_OMARCHY == 1)) || [[ -f "$HYPRLAND_LUA" ]]; then
        local setup_bin="${SUPER_DESKTOP_SETUP_BIN:-/usr/bin/super-desktop-setup}"
        [[ -x "$setup_bin" ]] || die "Packaged setup helper not found at $setup_bin."
        if [[ -L "$BIN_DST" && "$(readlink "$BIN_DST" 2>/dev/null || true)" != "/usr/bin/super-desktop" ]] ||
            [[ -e "$BIN_DST" && ! -L "$BIN_DST" ]] ||
            [[ -e "$APP_DST" ]]; then
            "$setup_bin" --migrate
        else
            "$setup_bin"
        fi
    else
        configure_non_omarchy_desktop "/usr/bin/super-desktop" 1
    fi
}

restart_packaged_daemon() {
    if ! daemon_running; then
        reload_hyprland
        return
    fi
    if [[ -z "${WAYLAND_DISPLAY:-}" ]] || ! hyprland_reachable; then
        warn "SUPER DESKTOP is running the previous build. Restart it from your desktop terminal: /usr/bin/super-desktop kill && /usr/bin/super-desktop toggle"
        return
    fi

    local uid state_dir daemon_log sock
    uid="$(id -u)"
    state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/super-desktop"
    daemon_log="$state_dir/daemon.log"
    sock="${XDG_RUNTIME_DIR:-/run/user/$uid}/super-desktop.sock"

    say "Restarting SUPER DESKTOP on the new package..."
    /usr/bin/super-desktop kill >/dev/null 2>&1 || true
    pkill -u "$uid" -f "$DAEMON_PROCESS" >/dev/null 2>&1 || true
    pkill -u "$uid" -f "$BRIDGE_PROCESS" >/dev/null 2>&1 || true
    pkill -u "$uid" -f "$TELEGRAM_PROCESS" >/dev/null 2>&1 || true
    sleep 0.5
    if pgrep -u "$uid" -f "$TELEGRAM_PROCESS" >/dev/null 2>&1; then
        pkill -KILL -u "$uid" -f "$TELEGRAM_PROCESS" >/dev/null 2>&1 || true
        sleep 0.2
    fi
    if [[ -S "$sock" ]] && ! daemon_running; then
        rm -f "$sock"
    fi

    reload_hyprland
    mkdir -p "$state_dir"
    chmod 700 "$state_dir"
    (umask 077 && nohup setsid /usr/bin/super-desktop daemon </dev/null >"$daemon_log" 2>&1 & disown) || true
    for _ in $(seq 1 50); do
        if /usr/bin/super-desktop status >/dev/null 2>&1; then
            /usr/bin/super-desktop reload-theme >/dev/null 2>&1 || true
            say "✓ Restarted SUPER DESKTOP daemon"
            return
        fi
        sleep 0.1
    done
    warn "SUPER DESKTOP daemon did not answer in time. Run /usr/bin/super-desktop toggle to start it."
}

summary_packaged() {
    local installed_ver
    installed_ver="$(/usr/bin/super-desktop --version 2>/dev/null || echo 'super-desktop-bin')"
    say ""
    say "=== SUPER DESKTOP Installation Complete! ==="
    say "Installed: $installed_ver (prebuilt package, no compilation needed)"
    if ((IS_OMARCHY == 1)) || [[ -f "$HYPRLAND_LUA" ]]; then
        say "Press SUPER + SHIFT + Q (or the shortcut you recorded in ⚙ Settings) to toggle your workspace, or run: super-desktop toggle"
    else
        say "Run: super-desktop toggle"
    fi
    say "Install and sign in to the AI coding CLIs you want to use separately."
    if [[ "$RELEASE" == v* ]]; then
        say "Pinned release $RELEASE installed from release asset."
    elif ((IS_OMARCHY == 1)); then
        say "Future updates arrive automatically through Omarchy system updates (omarchy update) or: sudo pacman -Syu super-desktop-bin"
    else
        say "Future updates arrive via: sudo pacman -Syu super-desktop-bin"
    fi
    if ((NON_OMARCHY_NEEDS_BINDING)); then
        print_compositor_instructions "super-desktop"
    fi
    if [[ -d "$DEFAULT_DIR/.git" ]]; then
        say "Note: your previous source checkout at $DEFAULT_DIR is no longer needed and can be removed if unused."
    fi
}

# A running daemon (and its bridge) still runs the previous build.
# rebuild.sh restarts both on this one, and reloads Hyprland.
restart_or_reload() {
    if ! daemon_running; then
        reload_hyprland
        return
    fi
    if [[ -z "${WAYLAND_DISPLAY:-}" ]] || ! hyprland_reachable; then
        warn "SUPER DESKTOP is running the previous build. Run $SRC/rebuild.sh from a terminal on that desktop to restart it."
        return
    fi
    say "Restarting SUPER DESKTOP on the new build..."
    "$SRC/rebuild.sh" ||
        warn "SUPER DESKTOP did not restart cleanly. Run $SRC/rebuild.sh to try again."
}

summary() {
    say ""
    say "=== SUPER DESKTOP Installation Complete! ==="
    if ((IS_OMARCHY == 1)) || [[ -f "$HYPRLAND_LUA" ]]; then
        say "Press SUPER + SHIFT + Q (or the shortcut you recorded in ⚙ Settings) to toggle your workspace, or run: super-desktop toggle"
    else
        say "Run: $BIN_DST toggle"
    fi
    say "Install and sign in to the AI coding CLIs you want to use separately."
    say "Source: $SRC (keep it: the installed command runs the binaries built there)"
    local pinned channel
    pinned="$(pinned_release "$SRC")" || pinned=""
    channel="$(channel_of "$SRC")"
    if [[ -n "$pinned" && "$channel" != releases && "$channel" != branch ]]; then
        say "Pinned to $pinned. It stays on this release until you choose another:"
        say "  Newest: ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=latest bash"
    elif [[ "$channel" == branch ]]; then
        say "Follows $BRANCH, with unreleased changes. Update: ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=master bash"
        say "  Back to the releases: ${INSTALL_COMMAND%| bash}| SUPER_DESKTOP_VERSION=latest bash"
    else
        [[ -z "$pinned" ]] || say "Release: $pinned"
        say "Update to the newest release: $INSTALL_COMMAND"
    fi
    if ((RUST_INSTALLED)); then
        say "Rust was installed into ${CARGO_HOME:-$HOME/.cargo}/bin and is not on your PATH."
    fi
    case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) warn "$HOME/.local/bin is not on your PATH, so the 'super-desktop' command is only reachable as $BIN_DST." ;;
    esac
    if ((NON_OMARCHY_NEEDS_BINDING)); then
        print_compositor_instructions "$BIN_DST"
    fi
}

main() {
    case "${1:-}" in
    -h | --help)
        usage
        return
        ;;
    "") ;;
    *)
        usage >&2
        die "Unknown argument: $1"
        ;;
    esac
    export GIT_TERMINAL_PROMPT=0

    say "=== Installing SUPER DESKTOP ==="
    preflight
    resolve_release

    local arch=""
    if [[ "$DISTRO_FAMILY" == "omarchy" || "$DISTRO_FAMILY" == "arch" ]] &&
        ! script_checkout >/dev/null 2>&1 && ((!BUILD_FROM_SOURCE)) && [[ "$RELEASE" != master ]] &&
        arch="$(supported_binary_arch)"; then
        if [[ "$RELEASE" == v* ]]; then
            if install_pinned_package "$RELEASE" "$arch"; then
                configure_packaged_user
                restart_packaged_daemon
                summary_packaged
                return
            fi
            warn "No prebuilt package found for $RELEASE ($arch); building from source instead..."
        elif pacman_repo_available "$arch"; then
            install_from_pacman_repo "$arch"
            configure_packaged_user
            restart_packaged_daemon
            summary_packaged
            return
        else
            say "Prebuilt Pacman repository is not active for $arch yet; building from source..."
        fi
    fi

    install_packages
    ensure_rust
    prepare_source
    build
    configure
    restart_or_reload
    summary
}

main "$@"
