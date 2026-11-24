# ⚡ SUPER DESKTOP

> 🌐 **Website: [superdesktop.dmytrogladkyi.com](https://superdesktop.dmytrogladkyi.com/)** · 📱 **[Free Android APK](https://github.com/gladimdim/super-desktop/releases/download/v2.0.8/SUPER-DESKTOP-2.2.0.apk)** · **Android app: [SUPER DESKTOP on Google Play](https://play.google.com/store/apps/details?id=com.gladimdim.superdesktop)** · **iPhone and iPad: [App Store](https://apps.apple.com/us/developer/dmytro-gladkyi/id578141466)**
>
> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**
> Press `SUPER + SHIFT + Q` — notes and AI terminals slide in from the screen edges. Press it again — everything disappears and your normal desktop is completely clean. All sessions keep running in background tmux.

![SUPER DESKTOP open above a browser: Claude Code and Codex terminal cards with status badges, and the top bar with the folder field and harness launchers](docs/screenshots/readme-overlay.webp)

<p align="center">
  <img src="docs/screenshots/desktop-toggle.gif" width="720" alt="SUPER + SHIFT + Q brings the terminal cards into view over a browser, and the same shortcut hides them again">
</p>

---

## Quick Install

On an **Omarchy desktop**, open a terminal and run this command as your normal user (not with `sudo`):

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash
```

The installer sets everything up automatically:
- installs missing Arch packages with `pacman` (GTK 4, gtk4-layer-shell, VTE, Qt Quick, tmux, wl-clipboard, libnotify, sqlite, avahi, bubblewrap, poppler) and Rust 1.92+ via rustup if needed;
- downloads and builds the newest [release](https://github.com/gladimdim/super-desktop/releases) under `~/.local/share/super-desktop/source` (keep this folder — the installed command runs binaries from it);
- configures the launcher, shortcut, autostart, and Omarchy theme integration.

Press **SUPER + SHIFT + Q** (or run `super-desktop toggle`) to open the overlay, pick a project folder, and click a harness launcher. Replay the intro anytime from **⚙ Settings → Getting started guide** or `super-desktop tour`. Update later via **⚙ Settings → Updates** or by re-running the install command.

For Arch packages, version pinning, protocol compatibility, charger sleep lock, and headless boot, see [Installation, Packages, Versions & Power](guides/INSTALL.md). Installing via an AI agent? Follow [For AI Assistants & Project Layout](guides/AI_INSTALL.md).

---

## Guides

| Guide | What it covers |
| --- | --- |
| **[How to SETUP Telegram Bot + Group](guides/TELEGRAM.md)** | Create your own `@BotFather` bot, pair your account, link a 2-member Forum Topics group (1 card = 1 topic), and configure permissions. |
| **[Installation, Packages, Versions & Power](guides/INSTALL.md)** | Source/clone updates, Arch packages (`super-desktop-setup`), version pinning, protocol compatibility table, sleep lock, headless login launch. |
| **[Android App, Free APK & Phone Bridge](guides/MOBILE.md)** | Free Android APK download & changelog, LAN/Tailscale phone bridge, TV code pairing, mobile scrollback, voice/photo/file attachments. |
| **[Supported AI Harnesses, Terminals & SD Editor](guides/HARNESSES.md)** | 19 supported harnesses, native adapters, custom & cloned launchers, launch parameters, terminal copy/paste, built-in SD Editor. |
| **[Use Another PC's Harnesses](guides/REMOTE_PCS.md)** | Pair two PCs, stream live remote workspaces, zoom/pan controls, and troubleshooting. |
| **[Plugins (QML Surfaces)](guides/PLUGINS.md)** | Build QML surfaces with AI, discover/install community plugins, edit locally, and publish. |
| **[Local MCP Access & CLI Quick Reference](guides/MCP_AND_CLI.md)** | Local stdio MCP server, structured CLI commands, idempotency receipts, and [complete CLI reference](CLI.md). |
| **[For AI Assistants & Project Layout](guides/AI_INSTALL.md)** | Exact Omarchy install rules for AI assistants, troubleshooting table, and repository layout. |
| **[Security](SECURITY.md)** | Security boundaries for the phone bridge, PC-to-PC sync, Telegram BYOB worker, MCP, and plugins. |

---

## What you get

![The top bar: the This PC selector, the working directory field, New note, the Claude, Codex, OpenCode, Grok and Shell launchers, and Arrange, Settings and Hide on the right](docs/screenshots/readme-toolbar.webp)

- **📝 Sticky notes** — autosaved notes with Omarchy theme colors, drag & drop, edge resize, and color tags.
- **💻 AI terminals** — live VTE4 terminal cards backed by tmux for Claude Code, Codex, OpenCode, Antigravity CLI, Grok, Pi, and more; folder-based tag colors, ghost outlines for buried cards, and **Alt+0 … Alt+9** quick switcher. See [Supported AI Harnesses, Terminals & SD Editor](guides/HARNESSES.md).
- **📎 Referenced files & attachments** — preview images, GIFs, sandboxed PDFs, and text/code directly from terminal output, or stage up to 20 files/images per card to insert into prompts.
- **✏️ Built-in SD Editor & 🧩 QML Plugins** — lightweight tree editor for the active folder plus live-reloading Qt Quick plugins you can generate or install with AI. See [Plugins](guides/PLUGINS.md).
- **✈ Telegram bot & Forum Topics** — control cards remotely using your own `@BotFather` bot over outbound HTTPS long-polling (no relay, no open ports), including 1-card-1-topic Forum supergroups, alerts, and granular permissions. Read **[How to SETUP Telegram Bot + Group](guides/TELEGRAM.md)**.
- **📱 Android app & 🖥 Remote PCs** — control cards from the [free Android APK](guides/MOBILE.md) or [Google Play app](https://play.google.com/store/apps/details?id=com.gladimdim.superdesktop), or stream and control another Linux PC's workspace in [Use Another PC's Harnesses](guides/REMOTE_PCS.md).
- **⌨️ CLI & Local MCP** — structured JSON inspection, guarded card launches, and a 25-tool stdio MCP server. See [Local MCP Access & CLI Quick Reference](guides/MCP_AND_CLI.md) and the [CLI reference](CLI.md).
- **🎨 Native Omarchy theming** — colors, fonts, and terminal palette follow active Omarchy theme switches live:

![The same workspace in the Catppuccin Latte, Rose Pine, Everforest and Gruvbox Omarchy themes](docs/screenshots/readme-themes.webp)

---

### Keybindings

| Combo | Action |
|---|---|
| `SUPER + SHIFT + Q` | **Show / hide SUPER DESKTOP** — works in EN + UK layouts. Change it in ⚙ Settings → *Show / hide shortcut* |
| `Esc` | Hide SUPER DESKTOP |
| Pointer in the top-left corner, held 2s | Show / hide SUPER DESKTOP (8×8 px zone, no keyboard involved) |
| `Ctrl + N` | New note |
| `Ctrl + T` | New terminal |
| Double-click terminal/header | Expand to 80% / collapse back |
| Click the 📁 folder field | Choose the working directory for new harness cards (`Enter` = home, `▾` = folders used before) |

---

### Local MCP access

Configure an MCP client to launch the local stdio server (supports MCP `2025-11-25` and `2025-03-26`):

```json
{
  "mcpServers": {
    "super-desktop": {
      "command": "super-desktop",
      "args": ["mcp", "serve"]
    }
  }
}
```

Open **⚙ Settings → MCP** to enable access and choose per-capability switches (terminal output, launches, prompts, control keys, closing, sticky notes), or copy ready-made client configuration. See [Local MCP Access & CLI Quick Reference](guides/MCP_AND_CLI.md) and [the MCP tool reference](CLI.md#local-mcp-tools).

---

## License

Copyright (c) 2026 Dmytro Gladkyi.

SUPER DESKTOP is licensed under the GNU General Public License, version 3 only
(`GPL-3.0-only`). You may redistribute and modify it under those terms. It is
provided without warranty; see [LICENSE](LICENSE) for the full terms.

Third-party dependencies and assets retain their own licenses. See the
[asset license notices](assets/logos/LICENSES.md) and
[attribution](assets/logos/ATTRIBUTION.md) for bundled logos and icons.
