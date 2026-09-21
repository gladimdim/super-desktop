# ⚡ SUPER DESKTOP

> 🌐 **Live site: [gladimdim.github.io/super-desktop](https://gladimdim.github.io/super-desktop/)**
>
> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**

> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**
> Press `SUPER + SHIFT + Q` — notes and AI terminals slide in from the screen edges. Press it again — everything disappears and your normal desktop is completely clean. All sessions keep running in background tmux.

---

## 👤 For humans — what is this?

SUPER DESKTOP is a second, invisible desktop that lives on top of your Omarchy workspace:

- **📝 Sticky notes** — click any note and type. Notes follow your Omarchy theme, autosave to disk, support drag & drop, resize from any edge or corner, and group color tags.
- **💻 AI terminals** — small live terminal cards (VTE4) running your AI coding agents as real interactive sessions: type, scroll, and work with the agent right inside the overlay, no fullscreen needed. Cards iconify to 128×128, resize from any edge or corner with a ghost preview, expand to 80% of the screen, and can be double-clicked, dragged, and color-tagged.
- **📎 Referenced files** — each terminal's **Files** button opens PNG/JPEG/WebP images, animated GIFs, PDF pages, Markdown, and text/code. Files are discovered on demand from terminal output; **Add** accepts a workspace-relative path when a reference is missing. No recursive folder scan, HTML viewer, or localhost proxy. PDF previews require `bubblewrap` and `poppler` on Linux and fail closed if the sandbox is unavailable. [File preview details](docs/FILE_ASSETS.md).
- **🪄 Overlay, not windows** — when hidden, nothing occupies Hyprland workspaces. Cards animate in from the nearest screen edge with background blur.
- **🎯 Hot corner** — park the pointer in the very top-left corner for two seconds and the overlay toggles, without touching the keyboard. Hidden while nothing of ours is on screen: it stays a pointer gesture, never a key grab.
- **⌨ Your own shortcut** — `SUPER + SHIFT + Q` out of the box. Open ⚙ Settings, click **Record**, press any combination you like — `SUPER`/`CTRL`/`ALT` plus a key, or an `F1`–`F12` key on its own: it is captured, remembered in `state.json` and written into Hyprland's config (plus its `code:` form, so a layout switch does not break it).
- **🎨 Native Omarchy theming** — colors, fonts, and terminal palette are read from the active Omarchy theme and update instantly when you switch themes (no restart).
- **📁 Workspace folder** — the text field right after the brand is the directory every **new** harness card starts in (`~` by default). Click it and a list of the folders you used before drops down — pick one, or type a path (`~/GitHub/proj`, `GitHub/proj`, or just `proj`) and press `Enter`; each row has its own ✕ to forget it. Clicking the folder also gives the harness its own project: harnesses scope their history to the working directory, and Reasonix keys its workspace write lease on it, so cards started in `~` block each other ("another session is writing to this workspace") while cards started in their own project do not. Existing cards keep the folder they were created in.
- **🔌 Phone bridge (optional)** — encrypted HTTPS/WSS over LAN or Tailscale (port 8759, mDNS `_omarchy-harness._tcp`). It starts automatically with the SUPER DESKTOP daemon; stop or restart it from the ⚙ gear → *Launcher connection*. Desktop QR verification, explicit approval, and per-phone revocation keep access under your control. Tailscale is optional. Protocol v3 requires re-pairing older phones. See [Security](SECURITY.md).

### Sleep lock on charger

In **Settings → Sleep lock**, enable **Prevent sleep while plugged in** to keep
AI harnesses reachable while SUPER DESKTOP runs on charger power, including with
the laptop lid closed. The setting persists across app restarts and defaults off.
It releases the lock within a few seconds of unplugging, disabling the setting,
or losing power-source information. Screen locking and display power saving
continue normally. Disable the setting before manually suspending.

This uses systemd-logind's `sleep:handle-lid-switch` block inhibitor; no root
access, logind configuration edits, or helper service is required. The settings
page reports active, battery, unknown-power, or permission/service errors rather
than claiming protection when the lock could not be acquired. Forced sleep and
other components that bypass logind inhibitors are outside its control.

### Supported AI harnesses

Android's per-terminal bell can report explicit Codex response completion, including
while the phone UI is hidden using an opt-in foreground monitor. Other harnesses
do not yet have verified completion adapters. See [completion alerts and delivery limits](docs/COMPLETION_NOTIFICATIONS.md).

Android can also attach an image using **＋** and send it together with a prompt
to an idle Codex terminal. See [image prompts, compatibility and upload limits](docs/IMAGE_PROMPTS.md).

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
| `SUPER + SHIFT + Q` | **Show / hide SUPER DESKTOP** — works in EN + UK layouts. Change it in ⚙ Settings → *Show / hide shortcut* |
| `Esc` | Hide SUPER DESKTOP |
| Pointer in the top-left corner, held 2s | Show / hide SUPER DESKTOP (8×8 px zone, no keyboard involved) |
| `Ctrl + N` | New note |
| `Ctrl + T` | New terminal |
| Double-click terminal/header | Expand to 80% / collapse back |
| Click the 📁 folder field | Choose the working directory for new harness cards (`Enter` = home, `▾` = folders used before) |

### CLI quick reference

```bash
super-desktop toggle          # show / hide
super-desktop status          # visible? how many notes / terminals
super-desktop add-note "Buy milk"
super-desktop add-term claude # claude | antigravity | codex | opencode | grok | reasonix | aider | shell
super-desktop reload-theme    # re-read Omarchy theme colors
super-desktop kill            # stop the background daemon
super-desktop harnesses       # one-shot JSON dump of sessions (debug)
super-desktop desktop-workspace # local card layout + epoch/revisions as JSON (debug)
```

State lives in `~/.config/super-desktop/state.json` (notes, cards, the workspace folder and the folders used before). Rebuild after updates with `./rebuild.sh`.

See [Linux performance](PERFORMANCE.md) for startup changes, measured command latency, and opt-in startup profiling.

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
3. Symlinks `~/.local/bin/super-desktop` → `target/release/super-desktop-client`, a lightweight native IPC client. Existing-daemon commands avoid loading GTK/VTE; cold starts delegate to the main binary with layer-shell preloaded. The repository's `bin/super-desktop` remains a build-on-demand fallback.
4. Copies `assets/` → `~/.config/super-desktop/assets/`.
5. Installs the desktop entry `~/.local/share/applications/super-desktop.desktop`.
6. Writes the toggle binding into a marked block in `~/.config/hypr/bindings.lua` — `hl.unbind` + `o.bind("SUPER + SHIFT + Q", …)` plus the `code:24` form that keeps it working on the UK/Cyrillic layouts — and `o.exec_on_start("super-desktop daemon")`. Migrates and skips what is already there, so re-running is safe. Everything between the two markers belongs to the app: the ⚙ Settings panel rewrites that block when the user records another shortcut (see `src/shortcut.rs`).
7. Appends the blur rule to `~/.config/hypr/hyprland.lua`: `hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true })`.
8. Installs the theme hook `~/.config/omarchy/hooks/theme-set.d/super-desktop` (runs `super-desktop reload-theme` on theme switch).
9. `hyprctl reload` + `hyprctl configerrors` validation.

### 2. Verify the install (all must pass)

```bash
super-desktop status        # expect: Hidden/Visible + Notes/Terminals counts
super-desktop harnesses     # expect: valid JSON (likely empty list on fresh install)
hyprctl configerrors        # expect: ok (no errors)
```

Then ask the user (or use the GUI) to press `SUPER + SHIFT + Q` — the overlay must appear. Create one shell card (`super-desktop add-term shell`) and toggle twice. If the user wants another shortcut, do not hand-edit it for them: tell them to open ⚙ Settings → *Show / hide shortcut* → **Record** and press it (that also rewrites the managed block).

### 3. Hard rules — do NOT break the user's system

- **Never edit anything under `/usr/share/omarchy/`** (package-owned; reading is fine). User config lives only in `~/.config/`.
- **Hyprland bindings**: only append via `o.bind(...)` / `o.rebind(...)` in `~/.config/hypr/bindings.lua`, and leave the marked super-desktop shortcut block alone — `install.sh` writes it and the ⚙ Settings recorder rewrites it (`src/shortcut.rs` holds the markers). After any Hyprland Lua change run `hyprctl reload` + `hyprctl configerrors` until clean.
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
| The hot corner does nothing | `hyprctl layers | grep sd-hotcorner` — the 8x8 corner surface must be there, on the overlay layer. It is an input zone, not a visible one: it paints 1/255 black, which is what keeps GTK from treating it as click-through (see `src/hotcorner.rs`). |
| The toggle shortcut does nothing | `grep -A4 'super-desktop shortcut' ~/.config/hypr/bindings.lua` — the managed block must hold the combination you expect; re-run `./install.sh`; `hyprctl reload`; `hyprctl configerrors` |
| A shortcut recorded in ⚙ Settings does nothing | Opening ⚙ Settings → *Record* parks Hyprland in a throwaway submap, so any combination is captured even if a bind already owns it; the new bind then needs a reload. Check `hyprctl configerrors`, then re-record. |
| Global shortcuts stopped working after using ⚙ Settings → *Record* | The recorder releases the submap on Esc, on Cancel and after 10s. If it was killed mid-recording: `hyprctl dispatch 'hl.dsp.submap("reset")'` |
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

PC-to-PC workspace switching: [implementation plan](docs/REMOTE_DESKTOP_PLAN.md).
Use **Add a PC** in the top-left machine selector to pair another computer and
view its read-only layout preview. The panel can either create a one-time
connection link for another PC or accept one from it; approval stays on the
host PC. Update and run `./rebuild.sh` on both PCs, since the rebuild also
restarts the separate bridge process. The [peer CLI](docs/REMOTE_DESKTOP_PROTOCOL.md#outgoing-pc-pairing-cli-increment)
is also available for pairing and remote layout retrieval.
Interactive remote consoles are still pending.

```
~/GitHub/super-desktop/
├── bin/super-desktop        # Launcher: LD_PRELOAD layer-shell, execs target/release/super-desktop (auto-builds if missing)
├── src/
│   ├── main.rs              # CLI dispatcher + Unix-socket IPC server/client (singleton daemon guard)
│   ├── window.rs            # LayerShell window, animations, HUD, 1s status refresh while visible
│   ├── mini_terminal.rs     # AI-terminal card (iconify/resize/expand, VTE attach, title from OWN session only)
│   ├── tmux.rs              # tmux session lifecycle + owned-session resolution (flag → claims-aware match → persisted)
│   ├── state.rs             # state.json persistence (notes, terminals, agent_session_id per card)
│   ├── shortcut.rs          # toggle-shortcut recorder: combo spelling, capture guard, bindings.lua block
│   ├── hotcorner.rs         # top-left hot corner: 8x8 pointer-only surface + dwell detector
│   ├── bridge.rs            # harness-bridge (LAN/Tailscale JSON for the phone app) + harnesses dump
│   ├── theme.rs / styles.rs # Omarchy theme parsing + GTK4 CSS generation
│   ├── sticky_note.rs       # Sticky note widget
│   ├── tag.rs / brand.rs    # group color tags, agent brand assets
│   ├── harness_settings.rs / launcher_settings.rs  # ⚙ settings card (shortcut + top bar + 📱 launcher connection page)
│   ├── usage.rs / ws.rs     # usage stats, misc helpers
│   └── crashlog.rs          # panic hook (release builds abort; crashes leave a trace)
├── assets/                  # vendored toolbar logos → ~/.config/super-desktop/assets/
├── Cargo.toml               # gtk4, gtk4-layer-shell, vte4, serde, serde_json, chrono, libc
├── install.sh               # fresh install (see §1 above)
├── rebuild.sh               # rebuild + Hyprland reload + daemon restart (see §3 above)
└── super-desktop.desktop    # desktop entry template
```

Key invariants for contributors: one daemon per machine; one tmux client per card with per-session `detach-on-destroy on`; card titles may only ever reflect the prompt typed **into that card's own harness** (see `resolve_own_opencode_id`); the toggle shortcut is only ever written inside the marked block in `bindings.lua` (see `src/shortcut.rs`); the hot corner must stay a pointer gesture — never a key grab, and never more than a few pixels wide (see `src/hotcorner.rs`); run `cargo test` before every rebuild.
