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
#   1. installs the Arch packages it needs that are missing (one sudo prompt);
#   2. installs Rust with rustup when there is no Rust 1.92 or newer, into
#      ~/.cargo and ~/.rustup, without editing your shell profile;
#   3. clones the source into ~/.local/share/super-desktop/source, or
#      fast-forwards the clone an earlier install runs from;
#   4. builds the release binaries in that clone;
#   5. links ~/.local/bin/super-desktop, copies the toolbar assets, and adds
#      the launcher entry, the SUPER + SHIFT + Q binding, the overlay's layer
#      rule and the Omarchy theme hook;
#   6. restarts SUPER DESKTOP if it is running, or reloads Hyprland.
#
# Running it again updates the app and keeps every setting already in place.
#
# Environment:
#   SUPER_DESKTOP_DIR     clone to install from (default: step 3)
#   SUPER_DESKTOP_REPO    git URL to clone
#   SUPER_DESKTOP_BRANCH  branch to clone (default: master)
#
# Everything runs from main, called on the last line, so a download cut
# short runs nothing.
set -euo pipefail

# Empty when the script arrives on stdin (curl | bash).
SELF="${BASH_SOURCE[0]:-}"

INSTALL_COMMAND="curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash"
REPO_URL="${SUPER_DESKTOP_REPO:-https://github.com/gladimdim/super-desktop.git}"
BRANCH="${SUPER_DESKTOP_BRANCH:-master}"
DEFAULT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/super-desktop/source"

# The oldest Rust that builds the locked dependencies (gtk4 0.11).
MIN_RUST="1.92"

# The build needs a C compiler, GTK 4, gtk4-layer-shell and VTE. At run time
# the app uses tmux for sessions, wl-clipboard for copying, libnotify for
# notifications, sqlite for OpenCode card titles, avahi for phone discovery,
# and bubblewrap with poppler for sandboxed PDF previews.
PACKAGES=(
    base-devel git curl
    gtk4 gtk4-layer-shell vte4
    tmux wl-clipboard libnotify sqlite avahi bubblewrap poppler
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
Install or update SUPER DESKTOP on Omarchy:

  $INSTALL_COMMAND

Environment:
  SUPER_DESKTOP_DIR     clone to install from (default: $DEFAULT_DIR,
                        or the clone an earlier install runs from)
  SUPER_DESKTOP_REPO    git URL to clone (default: $REPO_URL)
  SUPER_DESKTOP_BRANCH  branch to clone (default: $BRANCH)
EOF
}

preflight() {
    [[ "$(uname -s)" == Linux ]] || die "SUPER DESKTOP runs on Omarchy (Arch Linux with Hyprland)."
    if ((EUID == 0)); then
        die "Run the installer as your normal user, not as root or with sudo. It asks for your password when it installs packages."
    fi
    command -v pacman >/dev/null 2>&1 ||
        die "pacman not found. SUPER DESKTOP needs Omarchy (Arch Linux with Hyprland)."
    if ! command -v omarchy >/dev/null 2>&1 &&
        [[ ! -d /usr/share/omarchy && ! -d "$HOME/.local/share/omarchy" ]]; then
        die "Omarchy not found. SUPER DESKTOP uses Omarchy's Hyprland configuration and themes."
    fi
}

install_packages() {
    local missing=()
    mapfile -t missing < <(pacman -T "${PACKAGES[@]}")
    if ((${#missing[@]} == 0)); then
        say "✓ System packages present"
        return
    fi
    say "Installing system packages: ${missing[*]}"
    command -v sudo >/dev/null 2>&1 ||
        die "sudo not found. Install the packages as root, then run the installer again: pacman -S --needed ${missing[*]}"
    # sudo reads the password from the terminal, not from the piped script.
    if ! sudo pacman -S --needed --noconfirm "${missing[@]}" </dev/null; then
        die "Could not install ${missing[*]}. Install them with 'sudo pacman -S --needed ${missing[*]}' (after 'sudo pacman -Syu' if the package database is out of date), then run the installer again."
    fi
    say "✓ Installed ${missing[*]}"
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

# Fast-forward only: a clone with local work or its own history is installed
# as it is.
update_checkout() {
    local dir="$1"
    say "Updating $dir..."
    if [[ -n "$(git -C "$dir" status --porcelain --untracked-files=no)" ]]; then
        warn "$dir has local changes, so it was not updated. Installing it as it is."
        return
    fi
    if ! git -C "$dir" pull --ff-only </dev/null; then
        warn "Could not fast-forward $dir, so it was not updated. Installing it as it is."
    fi
}

prepare_source() {
    local dir
    if dir="$(script_checkout)"; then
        SRC="$dir"
        say "Installing from this clone: $SRC"
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
        git clone --branch "$BRANCH" "$REPO_URL" "$dir" </dev/null ||
            die "Could not clone $REPO_URL. Check the network connection and run the installer again."
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
    sed "s|^Exec=.*|Exec=$BIN_DST toggle|g" "$SRC/super-desktop.desktop" >"$APP_DST"
    say "✓ Installed desktop entry to $APP_DST"

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
o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle", { release = true })
hl.unbind("SUPER + SHIFT + code:24")
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
    if ! grep -q "super-desktop daemon" "$BINDINGS_LUA"; then
        echo 'o.exec_on_start("super-desktop daemon")' >>"$BINDINGS_LUA"
        say "✓ Added warm daemon autostart to $BINDINGS_LUA"
    else
        say "✓ Warm daemon autostart already present in $BINDINGS_LUA"
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
if which super-desktop >/dev/null 2>&1; then
    super-desktop reload-theme >/dev/null 2>&1 || true
fi
EOF
    chmod +x "$hook_file"
    say "✓ Installed Omarchy theme hook to $hook_file"
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

daemon_running() {
    pgrep -f "super-desktop.*daemon" >/dev/null 2>&1
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
    say "Press SUPER + SHIFT + Q (or the shortcut you recorded in ⚙ Settings) to toggle your workspace, or run: super-desktop toggle"
    say "Install and sign in to the AI coding CLIs you want to use separately."
    say "Source: $SRC (keep it: the installed command runs the binaries built there)"
    say "Update: $INSTALL_COMMAND"
    if ((RUST_INSTALLED)); then
        say "Rust was installed into ${CARGO_HOME:-$HOME/.cargo}/bin and is not on your PATH."
    fi
    case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) warn "$HOME/.local/bin is not on your PATH, so the 'super-desktop' command is only reachable as $BIN_DST." ;;
    esac
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
    install_packages
    ensure_rust
    prepare_source
    build
    configure
    restart_or_reload
    summary
}

main "$@"
