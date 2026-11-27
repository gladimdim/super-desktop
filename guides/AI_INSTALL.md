[← Back to README](../README.md)

# For AI Assistants & Project Layout

## 🤖 For AI assistants — installing this on someone's Omarchy

> You are an AI with shell access on an **Omarchy** machine (Arch Linux + Hyprland).
> Follow these steps exactly. Do not improvise the install — use the scripts below.

### 0. Preconditions (verify, do not assume)

- OS is Omarchy/Arch, Wayland Hyprland session with a display (`echo $WAYLAND_DISPLAY` must be non-empty for the overlay; build works headless, showing does not).
- You run commands as the **user** (never root for build/install steps).
- Nothing else needs to be installed first: the installer adds the missing Arch packages (build tools, `gtk4`, `gtk4-layer-shell`, `vte4`, `qt6-base`, `qt6-declarative`, `tmux`, `wl-clipboard`, `libnotify`, `sqlite`, `avahi`, `bubblewrap`, `poppler`, `gst-plugins-good`, `gst-libav`) and, when there is no Rust 1.92 or newer, installs Rust with the official rustup into `~/.cargo`/`~/.rustup` (shell profiles untouched; `rebuild.sh` finds it there).
- Run the installer in a terminal the user can type into so `sudo` can prompt when installing packages. The installer does not run a full system upgrade.

### 1. Install (the only supported way)

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash
```

From an existing clone, `./install.sh` installs that clone instead and does not touch its git state.

`install.sh` does, in order:
1. Refuses to run as root; checks for Omarchy and `pacman`.
2. Checks whether pacman can install the missing packages without upgrading unrelated system packages. If a required library conflicts with an older installed package, it stops with pacman's error so the user can run `omarchy update` (or `sudo pacman -Syu`) on their own terms first.
3. Uses a working Rust 1.92+; otherwise selects or updates rustup's stable toolchain, or downloads the official `rustup-init`, checks its SHA-256 and installs stable with `--no-modify-path`.
4. Picks the source: the clone holding the script; else `$SUPER_DESKTOP_DIR`; else the clone `~/.local/bin/super-desktop` already links into; else clones `~/.local/share/super-desktop/source`. It checks out the newest release tag (`vX.Y.Z`), or the release in `SUPER_DESKTOP_VERSION`, and records the choice in the clone's git config (`superdesktop.channel`); a pinned clone stays on its release, and a clone with local changes or commits of its own is installed as it is.
5. `cargo build --release --locked`.
6. Symlinks `~/.local/bin/super-desktop` → `target/release/super-desktop-client`, a lightweight native IPC client. Existing-daemon commands avoid loading GTK/VTE; cold starts delegate to the main binary with layer-shell preloaded. The repository's `bin/super-desktop` remains a build-on-demand fallback.
7. Copies `assets/` → `~/.config/super-desktop/assets/`.
8. Installs the desktop entry `~/.local/share/applications/super-desktop.desktop`.
9. Writes the toggle binding into a marked block in `~/.config/hypr/bindings.lua` — `hl.unbind` + `o.bind("SUPER + SHIFT + Q", …)` plus the `code:24` form that keeps it working on the UK/Cyrillic layouts — and `o.exec_on_start("super-desktop daemon")`. Migrates and skips what is already there, so re-running is safe. Everything between the two markers belongs to the app: the ⚙ Settings panel rewrites that block when the user records another shortcut (see `src/shortcut.rs`).
10. Appends the layer rule to `~/.config/hypr/hyprland.lua`: `hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true, no_anim = true, animation = "none" })`.
11. Installs the theme hook `~/.config/omarchy/hooks/theme-set.d/super-desktop` (runs `super-desktop reload-theme` on theme switch).
12. If a daemon is running and the script runs inside the Hyprland session, `rebuild.sh` restarts it and its bridge on the new build; otherwise `hyprctl reload` + `hyprctl configerrors` validation.

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
| `Daemon not running` after reboot/login | Start it: `super-desktop daemon` runs hidden (autostart line in `autostart.lua` covers future logins) |
| Overlay shows but immediately hides / toggle misbehaves | Two daemons are running — `pkill -f 'super-desktop.*daemon'`, remove stale socket, start exactly one |
| The hot corner does nothing | `hyprctl layers | grep sd-hotcorner` — the 8x8 corner surface must be there, on the overlay layer. It is an input zone, not a visible one: it paints 1/255 black, which is what keeps GTK from treating it as click-through (see `src/hotcorner.rs`). |
| The toggle shortcut does nothing | `grep -A4 'super-desktop shortcut' ~/.config/hypr/bindings.lua` — the managed block must hold the combination you expect; re-run the installer; `hyprctl reload`; `hyprctl configerrors` |
| A shortcut recorded in ⚙ Settings does nothing | Opening ⚙ Settings → *Record* parks Hyprland in a throwaway submap, so any combination is captured even if a bind already owns it; the new bind then needs a reload. Check `hyprctl configerrors`, then re-record. |
| Global shortcuts stopped working after using ⚙ Settings → *Record* | The recorder releases the submap on Esc, on Cancel and after 10s. If it was killed mid-recording: `hyprctl dispatch 'hl.dsp.submap("reset")'` |
| Installer stops saying a system upgrade is needed | The installer does not run a full system upgrade automatically. Have the user run `omarchy update` (or `sudo pacman -Syu`) first, then re-run the install command. |
| Installer stops at the package step | Read the pacman or Omarchy update error above the installer's message. Once the underlying issue is resolved, the same install command can be run again. |
| Build fails: `vte-2.91-gtk4` / GTK headers missing | `omarchy pkg add vte4 gtk4 gtk4-layer-shell pkgconf`, then `./rebuild.sh` |
| Installer says rustup's default is too old or not working | `rustup default stable`, then run the installer again |
| Theme changes don't restyle the overlay | Check `~/.config/omarchy/hooks/theme-set.d/super-desktop` is executable; run `super-desktop reload-theme` manually |
| Card shows another card's prompt in its title | Fixed in current code (owned-session resolution + self-heal). Update to latest (run the installer again); titles correct themselves within ~30s of the overlay being visible |
| Need daemon logs | `/tmp/super-desktop-daemon.log` |
| Nuclear reset of overlay state | Back up then delete `~/.config/super-desktop/state.json`; `super-desktop kill`; start daemon (cards/notes start fresh; tmux sessions are recreated on demand) |

### 5. Updating an existing install

Run the install command again, then `super-desktop status`. It moves the clone the install runs from to the newest release tag (a pinned clone stays on its release), rebuilds, and restarts the daemon and bridge when run inside the Hyprland session. A clone with local changes or commits of its own is left as it is: there, use `git pull && ./rebuild.sh`. The user can do the same from ⚙ Settings → Updates, which checks GitHub for a newer version and runs `rebuild.sh` (log: `~/.local/state/super-desktop/update.log`).

---

## 🏗️ Project layout (for contributors)

PC-to-PC workspaces (user guide: [Use another PC's harnesses](REMOTE_PCS.md)):
a remote workspace is drawn
by the same widgets a local one is (`harness_bar.rs`, `mini_terminal.rs`) with
only the source swapped (`card_source.rs`); every remote control is one typed
command carrying the card revision this view drew. The
peer CLI pairs PCs, fetches remote layouts, streams one console (`peer-attach`) or its
events (`peer-events`) and sends one command (`peer-command`);
`python3 tests/two_pc_matrix.py` runs the simulated two-PC regression matrix.

```text
~/GitHub/super-desktop/
├── bin/super-desktop        # Launcher: LD_PRELOAD layer-shell, execs target/release/super-desktop (auto-builds if missing)
├── src/
│   ├── main.rs              # CLI dispatcher + Unix-socket IPC server/client (singleton daemon guard)
│   ├── window.rs            # LayerShell window, animations, HUD, 1s status refresh while visible
│   ├── mini_terminal.rs     # AI-terminal card (iconify/resize/expand, VTE attach, title from OWN session only)
│   ├── overlap_ghost.rs     # dotted outlines for terminals buried under another card (≥70% covered, neither in use)
│   ├── tmux.rs              # tmux session lifecycle + owned-session resolution (flag → claims-aware match → persisted)
│   ├── state.rs             # state.json persistence (notes, terminals, agent_session_id per card)
│   ├── shortcut.rs          # toggle-shortcut recorder: combo spelling, capture guard, bindings.lua block
│   ├── hotcorner.rs         # top-left hot corner: 8x8 pointer-only surface + dwell detector
│   ├── bridge.rs            # harness-bridge (LAN/Tailscale JSON for the phone app) + harnesses dump
│   ├── theme.rs / styles.rs # Omarchy theme parsing + GTK4 CSS generation
│   ├── sticky_note.rs       # Sticky note widget
│   ├── tag.rs / brand.rs    # group color tags, agent brand assets
│   ├── harness_settings.rs / launcher_settings.rs  # ⚙ settings card (shortcut + top bar + 📱 launcher connection page)
│   ├── updates.rs           # ⚙ Settings → Updates: compare with GitHub, fast-forward, rebuild.sh
│   ├── usage.rs / ws.rs     # usage stats, misc helpers
│   └── crashlog.rs          # panic hook (release builds abort; crashes leave a trace)
├── .githooks/pre-commit     # refuses a commit whose Cargo.toml and Cargo.lock versions differ (git config core.hooksPath .githooks)
├── assets/                  # vendored toolbar logos → ~/.config/super-desktop/assets/
├── Cargo.toml               # gtk4, gtk4-layer-shell, vte4, serde, serde_json, chrono, libc
├── install.sh               # one-command install and update (see §1 above)
├── rebuild.sh               # rebuild + Hyprland reload + daemon restart (see §3 above)
└── super-desktop.desktop    # desktop entry template
```

Key invariants for contributors: one daemon per machine; one tmux client per card with per-session `detach-on-destroy on`; card titles may only ever reflect the prompt typed **into that card's own harness** (see `resolve_own_opencode_id`); the toggle shortcut is only ever written inside the marked block in `bindings.lua` (see `src/shortcut.rs`); the hot corner must stay a pointer gesture — never a key grab, and never more than a few pixels wide (see `src/hotcorner.rs`); run `cargo test` before every rebuild.
