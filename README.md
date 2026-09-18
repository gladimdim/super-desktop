# ⚡ SUPER DESKTOP

> 🌐 **Live site: [gladimdim.github.io/super-desktop](https://gladimdim.github.io/super-desktop/)**
>
> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**

> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**
> Press `SUPER + SHIFT + Q` — notes and AI terminals slide in from the screen edges. Press it again — everything disappears and your normal desktop is completely clean. All sessions keep running in background tmux.

---

## 👤 For humans — what is this?

SUPER DESKTOP is a second, invisible desktop that lives on top of your Omarchy workspace:

- **📝 Sticky notes** — click any note and type. Notes follow your Omarchy theme, autosave to disk, support drag & drop, resize, and group color tags.
- **💻 AI terminals** — small live terminal cards (VTE4) running your AI coding agents as real interactive sessions: type, scroll, and work with the agent right inside the overlay, no fullscreen needed. Cards iconify to 128×128, resize freely with ghost preview, expand to 80% of the screen, and can be double-clicked, dragged, and color-tagged.
- **🪄 Overlay, not windows** — when hidden, nothing occupies Hyprland workspaces. Cards animate in from the nearest screen edge with background blur.
- **🎨 Native Omarchy theming** — colors, fonts, and terminal palette are read from the active Omarchy theme and update instantly when you switch themes (no restart).
- **🔌 Phone bridge (optional)** — exposes your harness sessions over LAN/Tailscale (port 8759, mDNS `_omarchy-harness._tcp`) for the OmarchyAILauncher Android app.

### Supported AI harnesses

| | |
|---|---|
| 🌌 Antigravity CLI | `agy` / `antigravity` |
| ⚡ Claude Code | `claude` |
| 🤖 OpenAI Codex | `codex` |
| 🔮 OpenCode | `opencode` |
| 🚀 Grok CLI | `grok` |
| 🧭 Reasonix | `reasonix code` (or `npx -y reasonix code`) |
| 🧠 Aider | `aider` |
| 💻 Shell | `bash` / `zsh` / `fish` |

Only harnesses actually installed on the machine are offered (⚙ Settings panel).

### Keybindings

| Combo | Action |
|---|---|
| `SUPER + SHIFT + Q` | **Show / hide SUPER DESKTOP** (works in EN + UK layouts) |
| `Esc` | Hide SUPER DESKTOP |
| `Ctrl + N` | New note |
| `Ctrl + T` | New terminal |
| Double-click terminal/header | Expand to 80% / collapse back |

### CLI quick reference

```bash
super-desktop toggle          # show / hide
super-desktop status          # visible? how many notes / terminals
super-desktop add-note "Buy milk"
super-desktop add-term claude # claude | antigravity | codex | opencode | grok | reasonix | aider | shell
super-desktop reload-theme    # re-read Omarchy theme colors
super-desktop kill            # stop the background daemon
super-desktop harnesses       # one-shot JSON dump of sessions (debug)
```

State lives in `~/.config/super-desktop/state.json`. Rebuild after updates with `./rebuild.sh`.

---

## 🤖 For AI assistants — installing this on someone's Omarchy

> You are an AI with shell access on an **Omarchy** machine (Arch Linux + Hyprland).
> Follow these steps exactly. Do not improvise the install — use the scripts below.

### 0. Preconditions (verify, do not assume)

- OS is Omarchy/Arch, Wayland Hyprland session with a display (`echo $WAYLAND_DISPLAY` must be non-empty for the overlay; build works headless, showing does not).
- You run commands as the **user** (never root for build/install steps).
- `git`, `tmux`, `pkg-config`, `sqlite3` should exist. If anything is missing: `omarchy pkg add <name>` (e.g. `omarchy pkg add tmux sqlite3`). `sqlite3` is optional but recommended — without it, opencode card titles fall back to plain labels.
- GTK4 system libs are preinstalled on Omarchy (`gtk4`, `gtk-layer-shell`, `vte4`). If the build fails on missing `vte-2.91-gtk4` (pkg-config) or GTK headers, install them first: `omarchy pkg add vte4 gtk4 gtk-layer-shell pkg-config`.
- Rust toolchain: the installer offers to add `rustup` via `omarchy pkg add rustup` when run in a terminal. Prefer running the installer **from a real terminal** (it needs a TTY for package prompts).

### 1. Clone and install (the only supported way)

```bash
git clone https://github.com/gladimdim/super-desktop.git ~/GitHub/super-desktop
cd ~/GitHub/super-desktop
./install.sh
```

`install.sh` does, in order:
1. Ensures `cargo` and `vte4` (warns instead of prompting when no TTY).
2. `cargo build --release`.
3. Symlinks `~/.local/bin/super-desktop` → `bin/super-desktop` (a launcher that `LD_PRELOAD`s `libgtk4-layer-shell.so` and execs `target/release/super-desktop`; it auto-builds if the binary is missing).
4. Copies `assets/` → `~/.config/super-desktop/assets/`.
5. Installs the desktop entry `~/.local/share/applications/super-desktop.desktop`.
6. Appends to `~/.config/hypr/bindings.lua`: `o.bind("SUPER + SHIFT + Q", …)` (+ UK-layout variants) and `o.exec_on_start("super-desktop daemon")`. Skips lines that already exist — safe to re-run.
7. Appends the blur rule to `~/.config/hypr/hyprland.lua`: `hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true })`.
8. Installs the theme hook `~/.config/omarchy/hooks/theme-set.d/super-desktop` (runs `super-desktop reload-theme` on theme switch).
9. `hyprctl reload` + `hyprctl configerrors` validation.

### 2. Verify the install (all must pass)

```bash
super-desktop status        # expect: Hidden/Visible + Notes/Terminals counts
super-desktop harnesses     # expect: valid JSON (likely empty list on fresh install)
hyprctl configerrors        # expect: ok (no errors)
```

Then ask the user (or use the GUI) to press `SUPER + SHIFT + Q` — the overlay must appear. Create one shell card (`super-desktop add-term shell`) and toggle twice.

### 3. Hard rules — do NOT break the user's system

- **Never edit anything under `/usr/share/omarchy/`** (package-owned; reading is fine). User config lives only in `~/.config/`.
- **Hyprland bindings**: only append via `o.bind(...)` / `o.rebind(...)` in `~/.config/hypr/bindings.lua`. After any Hyprland Lua change run `hyprctl reload` + `hyprctl configerrors` until clean.
- **Singleton daemon**: exactly one `super-desktop daemon` may run (Unix socket `$XDG_RUNTIME_DIR/super-desktop.sock`). Never start a second one. To restart: `super-desktop kill`, then start one daemon (or just run `./rebuild.sh`, which handles kill → build → start → verify).
- **Stale socket**: if `status` says "Daemon not running" but a socket file lingers with no process behind it, remove the file and start the daemon. (`rebuild.sh` does this automatically.)
- **Rebuilding after code changes**: always use `./rebuild.sh` from the repo (release build + Hyprland reload + daemon restart + status check). Use `./rebuild.sh --no-daemon` for build-only.
- **Per-card sessions**: each terminal card owns one tmux session named `sd_term_*`. Never `tmux kill-server` and never reuse those names. Closing a card via its ✕ button kills its session cleanly.
- **Do not commit** `target/`, `~/.config/super-desktop/state.json`, or machine-local files. Check `git status` before any commit.

### 4. Troubleshooting

| Symptom | Fix |
|---|---|
| `Daemon not running` after reboot/login | Start it: `super-desktop daemon` runs hidden (autostart line in `bindings.lua` covers future logins) |
| Overlay shows but immediately hides / toggle misbehaves | Two daemons are running — `pkill -f 'super-desktop.*daemon'`, remove stale socket, start exactly one |
| `SUPER + SHIFT + Q` does nothing | `grep super-desktop ~/.config/hypr/bindings.lua`; re-run `./install.sh`; `hyprctl reload`; `hyprctl configerrors` |
| Build fails: `vte-2.91-gtk4` / GTK headers missing | `omarchy pkg add vte4 gtk4 gtk-layer-shell pkg-config`, then `./rebuild.sh` |
| Build fails: no `cargo` | `omarchy pkg add rustup`, open a new shell, then `./install.sh` |
| Theme changes don't restyle the overlay | Check `~/.config/omarchy/hooks/theme-set.d/super-desktop` is executable; run `super-desktop reload-theme` manually |
| Card shows another card's prompt in its title | Fixed in current code (owned-session resolution + self-heal). Update to latest (`git pull` + `./rebuild.sh`); titles correct themselves within ~30s of the overlay being visible |
| Need daemon logs | `/tmp/super-desktop-daemon.log` |
| Nuclear reset of overlay state | Back up then delete `~/.config/super-desktop/state.json`; `super-desktop kill`; start daemon (cards/notes start fresh; tmux sessions are recreated on demand) |

### 5. Updating an existing install

```bash
cd ~/GitHub/super-desktop
git pull
./rebuild.sh
super-desktop status
```

---

## 🏗️ Project layout (for contributors)

```
~/GitHub/super-desktop/
├── bin/super-desktop        # Launcher: LD_PRELOAD layer-shell, execs target/release/super-desktop (auto-builds if missing)
├── src/
│   ├── main.rs              # CLI dispatcher + Unix-socket IPC server/client (singleton daemon guard)
│   ├── window.rs            # LayerShell window, animations, HUD, 1s status refresh while visible
│   ├── mini_terminal.rs     # AI-terminal card (iconify/resize/expand, VTE attach, title from OWN session only)
│   ├── tmux.rs              # tmux session lifecycle + owned-session resolution (flag → claims-aware match → persisted)
│   ├── state.rs             # state.json persistence (notes, terminals, agent_session_id per card)
│   ├── bridge.rs            # harness-bridge (LAN/Tailscale JSON for the phone app) + harnesses dump
│   ├── theme.rs / styles.rs # Omarchy theme parsing + GTK4 CSS generation
│   ├── sticky_note.rs       # Sticky note widget
│   ├── tag.rs / brand.rs    # group color tags, agent brand assets
│   ├── harness_settings.rs / launcher_settings.rs  # settings panels
│   ├── usage.rs / ws.rs     # usage stats, misc helpers
│   └── crashlog.rs          # panic hook (release builds abort; crashes leave a trace)
├── assets/                  # vendored toolbar logos → ~/.config/super-desktop/assets/
├── Cargo.toml               # gtk4, gtk4-layer-shell, vte4, serde, serde_json, chrono, libc
├── install.sh               # fresh install (see §1 above)
├── rebuild.sh               # rebuild + Hyprland reload + daemon restart (see §3 above)
└── super-desktop.desktop    # desktop entry template
```

Key invariants for contributors: one daemon per machine; one tmux client per card with per-session `detach-on-destroy on`; card titles may only ever reflect the prompt typed **into that card's own harness** (see `resolve_own_opencode_id`); run `cargo test` before every rebuild.
