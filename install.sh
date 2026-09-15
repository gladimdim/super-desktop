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

# 1. Ensure ~/.local/bin exists and symlink executable
mkdir -p "$HOME/.local/bin"
mkdir -p "$CONFIG_DIR"
mkdir -p "$HOME/.local/share/applications"

chmod +x "$BIN_SRC"
ln -sf "$BIN_SRC" "$BIN_DST"
echo "✓ Symlinked executable to $BIN_DST"

# 2. Install desktop entry
sed "s|/home/gladimdim/.local/bin/super-desktop|$BIN_DST|g" "$SCRIPT_DIR/super-desktop.desktop" > "$APP_DST"
echo "✓ Installed desktop entry to $APP_DST"

# 3. Add Hyprland keybinding to ~/.config/hypr/bindings.lua
if [[ -f "$BINDINGS_LUA" ]]; then
    if ! grep -q "super-desktop toggle" "$BINDINGS_LUA"; then
        echo "" >> "$BINDINGS_LUA"
        echo "-- SUPER DESKTOP: Sticky notes and AI agent terminal overlay" >> "$BINDINGS_LUA"
        echo 'o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle")' >> "$BINDINGS_LUA"
        echo 'o.bind("SUPER + SHIFT + Cyrillic_shorti", "Super Desktop", "super-desktop toggle")' >> "$BINDINGS_LUA"
        echo 'o.bind("SUPER + SHIFT + Cyrillic_SHORTI", "Super Desktop", "super-desktop toggle")' >> "$BINDINGS_LUA"
        echo 'o.bind("SUPER + SHIFT + code:24", "Super Desktop", "super-desktop toggle")' >> "$BINDINGS_LUA"
        echo "✓ Added SUPER + SHIFT + Q bindings to $BINDINGS_LUA"
    else
        echo "✓ Hyprland keybinding already present in $BINDINGS_LUA"
    fi
fi

# 4. Add Layer Rule for blur effect to ~/.config/hypr/hyprland.lua
if [[ -f "$HYPRLAND_LUA" ]]; then
    if ! grep -q "super-desktop" "$HYPRLAND_LUA"; then
        echo "" >> "$HYPRLAND_LUA"
        echo "-- Super Desktop overlay blur effect" >> "$HYPRLAND_LUA"
        echo 'hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true })' >> "$HYPRLAND_LUA"
        echo "✓ Added layer rule to $HYPRLAND_LUA"
    else
        echo "✓ Hyprland layer rule already present in $HYPRLAND_LUA"
    fi
fi

# 5. Reload Hyprland and validate
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
