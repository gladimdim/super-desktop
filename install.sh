#!/usr/bin/env bash
# Installer for SUPER DESKTOP on Omarchy / Hyprland
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_SRC="$SCRIPT_DIR/bin/super-desktop"
BIN_DST="$HOME/.local/bin/super-desktop"
CONFIG_DIR="$HOME/.config/super-desktop"
APP_DST="$HOME/.local/share/applications/super-desktop.desktop"
BINDINGS_LUA="$HOME/.config/hypr/bindings.lua"
HYPRLAND_LUA="$HOME/.config/hypr/hyprland.lua"

echo "=== Installing SUPER DESKTOP ==="

# Check for cargo
if ! command -v cargo &> /dev/null; then
    echo "Rust/Cargo is not installed. Installing rustup..."
    if [ -t 1 ]; then
        omarchy pkg add rustup || echo "Warning: could not install rustup automatically."
    else
        echo "Warning: Not running in a terminal, please manually install rustup: omarchy pkg add rustup"
    fi
fi

# Check for vte4
if ! pkg-config --exists vte-2.91-gtk4 2>/dev/null; then
    echo "Installing vte4 (GTK4 terminal widget for in-overlay agent sessions)..."
    if [ -t 1 ]; then
        omarchy pkg add vte4 || echo "Warning: could not install vte4 automatically."
    else
        echo "Warning: Not running in a terminal, please manually install vte4: omarchy pkg add vte4"
    fi
fi

echo "Building native Rust binary (release)..."
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

# 1. Ensure ~/.local/bin exists and symlink executable
mkdir -p "$HOME/.local/bin"
mkdir -p "$CONFIG_DIR"
mkdir -p "$HOME/.local/share/applications"

chmod +x "$BIN_SRC"
ln -sf "$BIN_SRC" "$BIN_DST"
echo "✓ Symlinked executable to $BIN_DST"

# 1b. Install vendored toolbar logos (company SVGs)
if [[ -d "$SCRIPT_DIR/assets" ]]; then
    mkdir -p "$CONFIG_DIR/assets"
    cp -r "$SCRIPT_DIR/assets/." "$CONFIG_DIR/assets/"
    echo "✓ Installed toolbar assets to $CONFIG_DIR/assets"
fi

# 2. Install desktop entry
sed "s|^Exec=.*|Exec=$BIN_DST toggle|g" "$SCRIPT_DIR/super-desktop.desktop" > "$APP_DST"
echo "✓ Installed desktop entry to $APP_DST"

# 3. Add the overlay's toggle binding to ~/.config/hypr/bindings.lua.
#
# Everything between the two markers below belongs to the app: the overlay's
# ⚙ Settings panel rewrites this block when the user records another shortcut,
# and finds it again by the markers. src/shortcut.rs holds the same two strings
# (MANAGED_BEGIN/MANAGED_END) — change them in both places or in neither.
BIND_MARKER='-- >>> super-desktop shortcut (managed by the overlay settings) >>>'
mkdir -p "$(dirname "$BINDINGS_LUA")"
touch "$BINDINGS_LUA"
if ! grep -qF "$BIND_MARKER" "$BINDINGS_LUA"; then
    # Older runs appended loose o.bind(...) lines together with the
    # hl.unbind(...) lines that cleared the way for them. Drop those first:
    # otherwise a re-run leaves two blocks fighting over the same shortcut (and
    # the unbinds, which can outlive their bind, would keep an unused key dead).
    if grep -qF "super-desktop toggle" "$BINDINGS_LUA"; then
        BINDINGS_TMP="$(mktemp)"
        grep -v -F \
            -e "super-desktop toggle" \
            -e '-- SUPER DESKTOP: Sticky notes and AI agent terminal overlay' \
            -e 'hl.unbind("SUPER + SHIFT + q")' \
            -e 'hl.unbind("SUPER + SHIFT + Cyrillic_shorti")' \
            -e 'hl.unbind("SUPER + SHIFT + Cyrillic_SHORTI")' \
            -e 'hl.unbind("SUPER + SHIFT + code:24")' \
            "$BINDINGS_LUA" > "$BINDINGS_TMP" || true
        cat "$BINDINGS_TMP" > "$BINDINGS_LUA" # in place: keeps mode and owner
        rm -f "$BINDINGS_TMP"
        echo "✓ Migrated the loose super-desktop bind lines in $BINDINGS_LUA"
    fi
    cat >> "$BINDINGS_LUA" << 'EOF'

-- >>> super-desktop shortcut (managed by the overlay settings) >>>
-- Set in the overlay: ⚙ Settings → Keyboard shortcut.
-- Rewritten there on every change; edits inside this block are lost.
hl.unbind("SUPER + SHIFT + Q")
o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle")
hl.unbind("SUPER + SHIFT + code:24")
o.bind("SUPER + SHIFT + code:24", "Super Desktop", "super-desktop toggle")
-- <<< super-desktop shortcut <<<
EOF
    echo "✓ Added the SUPER + SHIFT + Q binding to $BINDINGS_LUA"
else
    echo "✓ Hyprland keybinding already present in $BINDINGS_LUA"
fi

# 3b. Keep the daemon warm so the shortcut does not have to spawn it.
# `daemon` (not `start`) comes up hidden: the overlay is only built on the
# first toggle, but the process, GTK and the IPC socket are already there.
if ! grep -q "super-desktop daemon" "$BINDINGS_LUA"; then
    echo 'o.exec_on_start("super-desktop daemon")' >> "$BINDINGS_LUA"
    echo "✓ Added warm daemon autostart to $BINDINGS_LUA"
else
    echo "✓ Warm daemon autostart already present in $BINDINGS_LUA"
fi

# 4. Add Layer Rule for blur effect to ~/.config/hypr/hyprland.lua
mkdir -p "$(dirname "$HYPRLAND_LUA")"
touch "$HYPRLAND_LUA"
if ! grep -q "super-desktop" "$HYPRLAND_LUA"; then
    echo "" >> "$HYPRLAND_LUA"
    echo "-- Super Desktop overlay blur effect" >> "$HYPRLAND_LUA"
    echo 'hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true })' >> "$HYPRLAND_LUA"
    echo "✓ Added layer rule to $HYPRLAND_LUA"
else
    echo "✓ Hyprland layer rule already present in $HYPRLAND_LUA"
fi

# 5. Install Omarchy theme-set hook for instant theme synchronization
HOOK_DIR="$HOME/.config/omarchy/hooks/theme-set.d"
HOOK_FILE="$HOOK_DIR/super-desktop"
mkdir -p "$HOOK_DIR"
cat << 'EOF' > "$HOOK_FILE"
#!/usr/bin/env bash
if which super-desktop >/dev/null 2>&1; then
    super-desktop reload-theme >/dev/null 2>&1 || true
fi
EOF
chmod +x "$HOOK_FILE"
echo "✓ Installed Omarchy theme hook to $HOOK_FILE"

# 6. Reload Hyprland and validate
if which hyprctl >/dev/null 2>&1; then
    echo "Reloading Hyprland configuration..."
    hyprctl reload >/dev/null || true
    ERRORS=$(hyprctl configerrors 2>/dev/null || true)
    if [[ -n "$ERRORS" && "$ERRORS" != "ok" && "$ERRORS" != "OK" ]]; then
        echo "Warning: Hyprland reported config error: $ERRORS"
    else
        echo "✓ Hyprland configuration validated cleanly!"
    fi
fi

echo "=== SUPER DESKTOP Installation Complete! ==="
echo "Press SUPER + SHIFT + Q to toggle your new workspace!"
