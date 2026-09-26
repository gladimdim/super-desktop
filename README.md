# ⚡ SUPER DESKTOP

> 🌐 **Live site: [gladimdim.github.io/super-desktop](https://gladimdim.github.io/super-desktop/)**
>
> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**

> **A hidden overlay workspace for Hyprland / Omarchy with sticky notes and mini AI-terminal windows.**
> Press `SUPER + SHIFT + Q` — notes and AI terminals slide in from the screen edges. Press it again — everything disappears and your normal desktop is completely clean. All sessions keep running in background tmux.

---

## 👤 For humans

### Step 1 — Install

On an **Omarchy desktop**, open a terminal and run this command as your normal
user (not with `sudo`):

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash
```

The installer needs nothing set up beforehand. It:

- installs the system packages it needs that are missing, with `pacman` (it
  asks for your password once): the build tools, GTK 4, gtk4-layer-shell, VTE,
  tmux, wl-clipboard, libnotify, sqlite, avahi, bubblewrap and poppler;
- installs Rust with the official rustup into `~/.cargo` and `~/.rustup` when
  there is no Rust 1.92 or newer, without changing your shell profile;
- downloads the source to `~/.local/share/super-desktop/source` and builds it
  (the first build takes several minutes);
- adds the app to your application launcher, and configures the shortcut,
  startup and Omarchy theme integration.

Keep the source folder: the installed command runs the binaries built there.

Then press **SUPER + SHIFT + Q** (SUPER is usually the Windows key), or run
`super-desktop toggle`, to open the overlay. Choose a project folder and click
an installed harness in the top bar. Install and sign in to your preferred AI
CLI separately; SUPER DESKTOP does not install agents or provide their accounts.

To update later, open **⚙ Settings → Updates**, or run the same command
again. Settings → Updates compares this build's version with the newest on
GitHub (every commit raises the version) and lists what a newer version
brings. **Update** fast-forwards the source and rebuilds it; SUPER DESKTOP
restarts on the new build only once the build succeeds, and a notification
says how it went (the log is `~/.local/state/super-desktop/update.log`). The
install command also rebuilds and restarts SUPER DESKTOP if it is running.
Either way your settings, notes and running harnesses are kept, and a clone
with uncommitted changes or commits of its own is not updated.

**Installing from a clone.** If you work on the code, clone the repository and
run `./install.sh` inside it. The installer then builds and installs that clone
and leaves its git state alone; update it with `git pull && ./rebuild.sh`.
Running the one-line command later updates the clone your install runs from,
as long as it has no local changes.

### What you get

SUPER DESKTOP is a second, invisible desktop that lives on top of your Omarchy workspace:

- **📝 Sticky notes** — click any note and type. Notes follow your Omarchy theme, autosave to disk, support drag & drop, resize from any edge or corner, and group color tags.
- **💻 AI terminals** — small live terminal cards (VTE4) running your AI coding agents as real interactive sessions: type, scroll, and work with the agent right inside the overlay, no fullscreen needed. Cards iconify to 128×128, resize from any edge or corner with a ghost preview, expand to 80% of the screen, and can be double-clicked, dragged, and color-tagged. Each project folder gets its own label color: a new harness takes the color of its folder, so Claude and Codex working in the same folder match, and a new folder gets a color no other folder uses yet. Pick another color on any card's dot and later harnesses in that folder use it. Drop a card on another one and the buried terminal keeps a **dotted ghost outline** of itself — it disappears only while you are working in that terminal or in the card that covers it, so a stacked desk never hides a session you forgot about.
- **📎 Referenced files** — each terminal's **Files** button opens PNG/JPEG/WebP images, animated GIFs, PDF pages, Markdown, and text/code. Files are discovered on demand from terminal output; **Add** accepts a workspace-relative path when a reference is missing. No recursive folder scan, HTML viewer, or localhost proxy. PDF previews require `bubblewrap` and `poppler` on Linux and fail closed if the sandbox is unavailable.
- **🪄 Overlay, not windows** — when hidden, nothing occupies Hyprland workspaces. Cards animate in from the nearest screen edge with background blur.
- **🎯 Hot corner** — park the pointer in the very top-left corner for two seconds and the overlay toggles, without touching the keyboard. Hidden while nothing of ours is on screen: it stays a pointer gesture, never a key grab.
- **🔢 Keyboard terminal picker** — hold **Alt for 30 ms** to dim terminal output and show dotted borders with centered digits. Press **Alt+0 … Alt+9** to raise and focus that terminal immediately, even before the preview appears; compact cards restore first. Each terminal gets the first free digit when created or restored (starting at **0**), and keeps it while open. Closing a card frees its digit without renumbering the others. Up to ten terminals per local or remote PC view get shortcuts; extra cards remain mouse-accessible. Release Alt or press Escape to dismiss. Assignments are rebuilt when cards are restored after restarting the app.
- **⌨ Your own shortcut** — `SUPER + SHIFT + Q` out of the box. Open ⚙ Settings, click **Record**, press any combination you like — `SUPER`/`CTRL`/`ALT` plus a key, or an `F1`–`F12` key on its own: it is captured, remembered in `state.json` and written into Hyprland's config (plus its `code:` form, so a layout switch does not break it).
- **⚙ Movable settings** — drag the ⚙ Settings card by its header, like a terminal card, to see what is under it. It keeps its size, stays on screen below the top bar, and opens where you left it.
- **🎨 Native Omarchy theming** — colors, fonts, and terminal palette are read from the active Omarchy theme and update instantly when you switch themes (no restart).
- **📁 Workspace folder** — the text field right after the brand is the directory every **new** harness card starts in (`~` by default). Click it and a list of the folders you used before drops down — pick one, or type a path (`~/GitHub/proj`, `GitHub/proj`, or just `proj`) and press `Enter`; each row has its own ✕ to forget it. Clicking the folder also gives the harness its own project: harnesses scope their history to the working directory, and Reasonix keys its workspace write lease on it, so cards started in `~` block each other ("another session is writing to this workspace") while cards started in their own project do not. Existing cards keep the folder they were created in.
- **🔌 Phone bridge (optional)** — encrypted HTTPS/WSS over LAN or Tailscale (port 8759, mDNS `_omarchy-harness._tcp`). It starts automatically with the SUPER DESKTOP daemon; stop or restart it from the ⚙ gear → *Connections*. A background supervisor checks every five seconds and recovers the bridge after three consecutive failed checks, even with the overlay hidden or screen locked. Stop pauses recovery until Start (or the next daemon launch). Failed starts are retried, and `bridge.previous.log` preserves the previous run beside `bridge.log`. To stay reachable while unattended on external power, enable *Settings → Sleep lock*; recovery cannot run while the PC is suspended. Desktop QR verification, explicit approval, and per-phone revocation keep access under your control. Tailscale is optional. Protocol v3 requires re-pairing older phones. See [Security](SECURITY.md).
- **🖥 Other PCs** — pair two PCs and open the other one's workspace from the top-left **This PC** selector: its consoles stream live and every action runs on that PC. See [Use another PC's harnesses](#use-another-pcs-harnesses).

### Use another PC's harnesses

Pair two PCs running SUPER DESKTOP, and either one can open the other's
workspace from the top-left **This PC** selector. A remote workspace uses the
same top bar and console cards as a local one, and everything in it acts *on
that PC*:
- Its consoles stream live, in colour, at that PC's own positions, sizes and
  stacking order.
- Typing, paste and Ctrl+C reach the focused session there.
- Dragging, edge-resizing, minimizing, maximizing and closing a card do the
  same on that PC.
- Its harness buttons launch harnesses there, and the folder field lists the
  folders that PC offers.

Changes made on that PC appear as soon as it publishes them. Pairing is one-way:
approving a PC lets it open this one, not the reverse. Install the same current
build on both PCs (run the install command again, or `git pull && ./rebuild.sh`
in a clone; both also restart the separate bridge process).

**Pair two PCs**

1. On the PC you want to open (the host), go to ⚙ Settings → **Connections** →
   **Add a device** → **Share this PC**. You can also use **This PC** → **Add a
   PC** → *I want this PC's harnesses to be available on another PC*. It checks
   the bridge, the firewall rule for port 8759/tcp and the network, then shows a
   single-use link that expires in five minutes. Click **Copy link**, then send
   it to the other PC over a channel you trust.
2. On the viewing PC, go to **This PC** → **Add a PC** → *I want to connect to
   another PC and view its harnesses*, or Settings → Connections → Add a device
   → **View another PC**. Paste the link and click **Connect to PC**.
3. The host's **connection request panel** opens. If the overlay is hidden,
   click the notification to open it. Click **Approve** only if its six-digit
   code matches the one on the viewing PC. **Reject** refuses the request and
   blocks that PC until you remove it from Settings → Connections → **Rejected
   devices**.

The viewing PC then selects the host and draws its workspace. To withdraw
access, go to the host's Settings → Connections → **PCs** → *Can open this PC*
and click **Revoke**. The viewer's streams close within a second.

**Viewing a remote workspace**

- **Fit / 100%** (left of Hide) chooses how the workspace is drawn. **Fit**
  shows all of it, scaled down and never enlarged. **100%** shows one host pixel
  per logical pixel. To pan, drag empty canvas, use the scrollbars, or scroll.
  To zoom (25–300%), use Ctrl+scroll or pinch; click the button to return to
  100%. Terminals are redrawn at the new font size rather than scaled. The view
  is never sent to the host or saved.
- An outline in the theme's accent colour marks the edges of that PC's screen.
  **Fit** centres it with a margin on every side; at **100%** you can scroll a
  margin past each edge, so the outline stays visible at both ends. A card you
  drag stops at that edge, so it always stays whole on that PC's screen.
- Each action reports its result in a short line under the card's header, or
  under the top bar for launches and folder picks. The line clears itself.
  - *Changed on that PC · showing its layout*: someone changed that card there
    first, so the card moves back to that PC's layout.
  - *Cannot reach that PC · change not applied*: the request never arrived.
  - *Result unknown · check before retrying*: the request was sent but no
    answer came back. It is never resent automatically.
- While a PC is connecting or cannot be reached, its workspace shows why and
  what to do, above a checklist of each link between the two PCs: this PC's
  network (Wi-Fi name or Ethernet, and address), Tailscale (on or off, and
  whether that PC is online in your tailnet), the route to that PC, and
  whether its port 8759 answers. **Retry now** tries again at once and
  repeats the checks; **Pair again** appears when only a new pairing helps.
- Switching back to **This PC**, hiding the overlay, or restarting either PC
  leaves every harness running.

**Not available for a remote PC yet:** sticky notes (a remote workspace shows
only that PC's consoles, and Ctrl+N makes no note there), the **Files & links**
button, and removing a saved PC from the GUI. To remove a saved PC, run
`super-desktop peer-forget MACHINE_ID`; `super-desktop peer-list` shows the IDs.

| You see | Do this |
| --- | --- |
| *Update SUPER DESKTOP on the host* (or *on that PC*) | That PC runs an older build: run the install command there, or `git pull && ./rebuild.sh` in its clone. |
| *… needs pairing again* or *Pairing required · Add this PC again* | The host revoked this PC, or its 90-day credential expired. Click **Pair again** and use a new link. |
| *… is not answering*, *Cannot reach this PC* or *Reconnecting…* | Check that the host is on, SUPER DESKTOP is running there, and port 8759/tcp is reachable over LAN or Tailscale. The checklist under the message shows which link fails. Consoles reconnect on their own. |
| *Tailscale is off on this PC* | That PC was paired at its Tailscale address. Run `tailscale up` on this PC. |
| *… is on another network* | That PC's LAN address is outside this PC's network. Join its network, or pair the two PCs over Tailscale. |
| *That PC restarted · change not applied* | Its SUPER DESKTOP restarted before the change arrived, and the view has refreshed. Repeat the action if you still want it. |
| *The other PC rejected this PC earlier…* | On the host, remove this PC in Settings → Connections → Rejected devices, then use a new link. |
| *The other PC already has a request from this address* | Wait two minutes for that request to expire, then use a new link. |
| The host answers 404 for `/api/v1/desktop/capabilities` | An old bridge is still running there. Running the install command (or `./rebuild.sh`) on the host restarts both processes. |

### Sleep lock on charger

In **Settings → Sleep lock**, enable **Prevent sleep while plugged in** to keep
the bridge and AI harnesses reachable while SUPER DESKTOP runs on mains or charger
power, including with the laptop lid closed. Recognized desktop chassis without
power-supply entries are treated as mains-powered. The setting persists across app restarts and defaults off.
It releases the lock within a few seconds of unplugging, disabling the setting,
or losing power-source information. Screen locking and display power saving
continue normally. Disable the setting before manually suspending.

This uses systemd-logind's `idle:sleep:handle-lid-switch` block inhibitor; no root
access, logind configuration edits, or helper service is required. The settings
page reports active, battery, unknown-power, or permission/service errors rather
than claiming protection when the lock could not be acquired. Forced sleep and
other components that bypass logind inhibitors are outside its control.

### Terminal copy and paste

Hold **Ctrl** and hover a web link (http/https) in a terminal card: it is underlined with a hand cursor, and **Ctrl+click** opens it in your default browser. A link that tmux wrapped onto several rows is only found on its first row.

In local and remote PC terminal cards, select text and press **Ctrl+Shift+C**
to copy, or **Ctrl+Shift+V** to paste the desktop clipboard. **Ctrl+C** keeps
its normal terminal interrupt behavior. In local cards a plain mouse drag (or a
double/triple click) is tmux's selection: on release it is copied to the desktop
clipboard and the card briefly shows "Copied to clipboard"; the highlight then
clears, as usual in tmux. Copies an app makes itself over OSC 52 (for example
fullscreen Claude Code's selection) reach the clipboard too. Both need
`wl-copy` and apply to SUPER DESKTOP sessions only; other tmux sessions keep
their own bindings. To keep a highlight and copy by keyboard instead, hold
**Shift** while dragging, then press **Ctrl+Shift+C**. As in Omarchy's own
terminals, **Ctrl+Insert** copies and **Shift+Insert** pastes the clipboard, so
Omarchy's **Super+C** and **Super+V** work in cards too. Paste uses VTE's
native handling, including bracketed paste when enabled by the running app. In
harness cards **Ctrl+V** also pastes text when the clipboard offers it,
including Chrome selections with both plain text and HTML; an image-only
clipboard still sends Ctrl+V to the harness, which attaches the image. Shell
cards keep their normal Ctrl+V (Vim's block selection, the shell's quoted
insert). The shortcuts work with a non-Latin keyboard layout active too.

Scroll back in a local card and **↓ Jump to newest** appears at its bottom:
click it to leave the history and follow new output again (scrolling back to
the end, or pressing **q**, leaves it too). The scrolling is tmux's copy mode,
so apps that draw their own full screen or read the mouse themselves scroll on
their own and show no button.

### Terminal history on mobile

New default launches use native scrollback: OpenCode `--mini`, Codex
`--no-alt-screen`, Pi `--tui-mode regular`, and Hermes `--cli`. Explicit custom
mode flags are retained. Existing sessions are not restarted. Crush stays in
its fullscreen UI; Android’s keyboard panel provides Tab to focus its history
and Page Up/Page Down to navigate older output. These keys move the remote
viewport rather than creating a local transcript.

### Grok on mobile

Default Grok launches use `--minimal` so finalized output enters native terminal
scrollback and is visible to the Android snapshot viewer. Fullscreen Grok keeps
its history inside the alternate-screen UI, exposing only its current viewport.
For an existing conversation, use `/minimal` to switch in place without restarting.
With an older launcher, set `[ui] screen_mode = "minimal"` in Grok's config.
Requires a Grok version with minimal mode. The bridge still sends a bounded
history tail; this does not turn it into an unlimited transcript viewer.

### Harness logos

Linux launchers, settings and terminal cards use bundled SVG product marks,
shared with Android's harness views and widgets. They scale with the UI and
use the active theme’s accent and background colors, including live theme changes. Custom icons remain user-selected;
Herder retains its fallback until a verified SVG is available. See
[logo sources and hashes](assets/logos/ATTRIBUTION.md).

### Supported AI harnesses

Claude Code, OpenCode and Pi now have scoped native metadata adapters for new
terminal sessions. They report conversation names where available, submitted
prompts, working/idle, permission waits and errors. Codex uses its own rollout
events and conversation index. OpenClaw TUI is discoverable after installation;
its status and titles come from a SUPER DESKTOP gateway plugin. **Settings →
Harness launchers** shows under the OpenClaw row whether that plugin is connected,
with **Connect** and **Restart gateway** buttons to set it up, and a new OpenClaw
card without it shows a one-time "OpenClaw status needs setup" hint. Until a native
adapter reports, a card uses the prompt typed into it and the on-screen status, so
it is never left blank. Launcher availability alone does not
mean a harness has a verified native adapter. T3 Code is no longer offered (it runs
a web server, not a terminal harness); an existing T3 card reopens as a plain
terminal running its saved command.

PC card title fallbacks and Android prompt labels share the last submitted input for each
tmux session. New input sent through the desktop, remote desktop, or Android
updates that value; response output and unsent drafts cannot replace it. Codex
rollout user messages and the owned OpenCode session provide fallbacks for
older sessions. Unsupported terminal editing (such as history recall) keeps the
previous tracked prompt rather than guessing from the response screen.

Regular terminal cards use the command that launched the task, never output
lines. Newly created plain Bash terminals record the submitted shell command,
including history recall and Tab completion, and keep it after the task finishes.
Input sent to the running task cannot replace this title. Existing terminals and
other shells use the foreground process's arguments when available, then tracked
input; exact original quoting, aliases, and pipelines require the Bash hook.
Titles remain shortened to fit the card. This also applies to remote PC and
Android labels; AI harness prompt titles are unchanged.

Android's per-terminal bell can report explicit Codex, Claude Code, Pi and OpenCode response completion,
including while the phone UI is hidden using an opt-in foreground monitor. Pi
requires a new launch with the updated desktop extension and updated Android app.
Other harnesses do not yet have verified completion adapters.

Android can send images and files with a prompt to any idle harness: **＋**
picks an image or a file or pastes an image from the clipboard (pasting into
the text field and dropping a file on a terminal work too). **Share → SUPER
DESKTOP** in any other Android app asks which harness gets the files or text,
and adds them to its draft. Up to 4 attachments and 16 MB go with one prompt.
The PC keeps them privately in `~/.local/state/super-desktop/uploads/`,
outside every workspace. Codex, Claude
Code, Grok and OpenCode's full interface receive images as their own image
attachments; other harnesses, and all other files, get the file's path in the
prompt, and a shell gets the quoted path after the command.

| | |
|---|---|
| 🌌 Antigravity CLI | `agy` / `antigravity` — install with `mise use -g agy` or `curl -fsSL https://antigravity.google/cli/install.sh \| bash`, sign in once by running `agy`. For phone scrollback set `"altScreenMode": "never"` in `~/.gemini/antigravity-cli/settings.json` (there is no per-launch flag). |
| ⚡ Claude Code | `claude` |
| 🤖 OpenAI Codex | `codex` |
| 🔮 OpenCode | `opencode` |
| 🚀 Grok CLI | `grok` |
| 🧭 Reasonix | `reasonix code` (or `npx -y reasonix code`) |
| 🐋 DeepSeek Harness | `dsh-tui` / `dst`, the community terminal UI (public beta) for DeepSeek's `dsh`. Install with `npm install -g @deepseek-ai/dsh @deepseek-harness-tui/dsh-tui`. The first run sets up its `dsh-tui` profile, which needs pnpm, and it needs `DEEPSEEK_API_KEY`. Permissions come from the dsh profile, so no flag is added. For phone scrollback turn off **Fullscreen mode** in dsh-tui's `/settings` (there is no per-launch flag). Cards restored after a restart start a new conversation, because `dsh-tui --resume` reopens one global last session, not the card's own. |
| 🧠 Aider | `aider` |
| ✦ Gemini CLI | `gemini` — no longer maintained upstream; still launches, but fresh installs do not add it to the top bar and an existing Gemini button switches to Antigravity once `agy` is installed (turn Gemini back on in ⚙ Settings to keep it). |
| 🪽 Hermes Agent | `hermes` |
| 🥧 Pi | `pi` |
| 🪿 Goose | `goose session` |
| 🌟 Qwen Code | `qwen` |
| 💘 Crush | `crush` |
| 🌙 Kimi Code | `kimi` |
| 🧰 Kiro CLI | `kiro-cli` |
| 🎯 Cursor Agent | `cursor-agent` |
| 🐑 Herder worker | `herder worker` (job supervisor, not an interactive chat) |
| 💻 Shell | `bash` / `zsh` / `fish` |

Fresh installations show the first three detected harnesses in the toolbar (or
fewer if fewer are installed). Settings → Harness launchers lets you enable more;
existing installations keep their selection. Arrange, Settings, and Hide stay at
the display's right edge, with horizontal scrolling for toolbar groups that do
not fit on smaller or scaled screens. Narrow displays hide the brand label and
shortcut hint to leave room for controls; Hide's tooltip still shows the shortcut.

Only launchers actually installed on the machine can be started. Settings →
Harness launchers lists both detected tools and supported tools that are not
installed yet. Press **Rescan** after installing a CLI; it checks the current
`PATH` and common user bin directories, then updates the top bar immediately.
On a fresh installation or after customizing which launchers are visible, turn on a newly
detected launcher with its toggle. An installation in another, nonstandard
directory may need to be added as a custom launcher.

To add another CLI, open **Settings → Harness launchers → Add a harness**. Its
own page lets you pick one of eight icons, enter the absolute path to its
executable, and add optional arguments (quote arguments containing spaces).
The name defaults to the executable filename if you leave it blank. Save it to
add a button to this PC's top bar. You can edit, hide, or remove it from the
same list. Its command runs in the selected workspace
folder; a connected PC can launch it only while this PC offers it.

Every installed built-in harness, Terminal included, has a **Parameters**
button in the same list. Its page shows the built-in defaults (Claude Code's
`--dangerously-skip-permissions`, Codex's `--no-alt-screen
--dangerously-bypass-approvals-and-sandbox`, …) and previews the command a new
card will run. Add flags such as `--model opus`, or remove ones you do not want:
saved parameters replace the built-in defaults, an empty field starts the
harness with none, and **Use built-in default** brings them back. Quote words
that contain spaces; `~` and `$VARIABLES` are passed as typed, not expanded.
New cards started from the top bar, the phone or another PC use these
parameters. Open cards keep their command, and restored cards keep theirs while
gaining parameters you add. Keep display-mode defaults such as Codex's
`--no-alt-screen` if you follow that harness from the phone: they are what puts
its history into scrollback.

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
super-desktop add-term claude # see Settings → Harness launchers for installed types
super-desktop reload-theme    # re-read Omarchy theme colors
super-desktop kill            # stop the background daemon
super-desktop harnesses       # one-shot JSON dump of sessions (debug)
super-desktop desktop-workspace # local card layout + epoch/revisions as JSON (debug)
```

State lives in `~/.config/super-desktop/state.json` (notes, cards, the workspace folder and the folders used before). Update with the install command, or with `./rebuild.sh` in a clone.


---

## 🤖 For AI assistants — installing this on someone's Omarchy

> You are an AI with shell access on an **Omarchy** machine (Arch Linux + Hyprland).
> Follow these steps exactly. Do not improvise the install — use the scripts below.

### 0. Preconditions (verify, do not assume)

- OS is Omarchy/Arch, Wayland Hyprland session with a display (`echo $WAYLAND_DISPLAY` must be non-empty for the overlay; build works headless, showing does not).
- You run commands as the **user** (never root for build/install steps).
- Nothing else needs to be installed first: the installer adds the missing Arch packages (build tools, `gtk4`, `gtk4-layer-shell`, `vte4`, `tmux`, `wl-clipboard`, `libnotify`, `sqlite`, `avahi`, `bubblewrap`, `poppler`) and, when there is no Rust 1.92 or newer, installs Rust with the official rustup into `~/.cargo`/`~/.rustup` (shell profiles untouched; `rebuild.sh` finds it there).
- Installing packages runs `sudo pacman`, which reads the user's password from the terminal. Run the installer in a terminal the user can type into. Without one, install the missing packages first (the installer prints the `pacman` command).

### 1. Install (the only supported way)

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | bash
```

From an existing clone, `./install.sh` installs that clone instead and does not touch its git state.

`install.sh` does, in order:
1. Refuses to run as root; checks for Omarchy and `pacman`.
2. Installs missing Arch packages with one `sudo pacman -S --needed` call.
3. Uses a working Rust 1.92+; otherwise selects or updates rustup's stable toolchain, or downloads the official `rustup-init`, checks its SHA-256 and installs stable with `--no-modify-path`.
4. Picks the source: the clone holding the script; else `$SUPER_DESKTOP_DIR`; else the clone `~/.local/bin/super-desktop` already links into; else clones `~/.local/share/super-desktop/source`. An existing clone is fast-forwarded only when it has no local changes.
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
| `Daemon not running` after reboot/login | Start it: `super-desktop daemon` runs hidden (autostart line in `bindings.lua` covers future logins) |
| Overlay shows but immediately hides / toggle misbehaves | Two daemons are running — `pkill -f 'super-desktop.*daemon'`, remove stale socket, start exactly one |
| The hot corner does nothing | `hyprctl layers | grep sd-hotcorner` — the 8x8 corner surface must be there, on the overlay layer. It is an input zone, not a visible one: it paints 1/255 black, which is what keeps GTK from treating it as click-through (see `src/hotcorner.rs`). |
| The toggle shortcut does nothing | `grep -A4 'super-desktop shortcut' ~/.config/hypr/bindings.lua` — the managed block must hold the combination you expect; re-run the installer; `hyprctl reload`; `hyprctl configerrors` |
| A shortcut recorded in ⚙ Settings does nothing | Opening ⚙ Settings → *Record* parks Hyprland in a throwaway submap, so any combination is captured even if a bind already owns it; the new bind then needs a reload. Check `hyprctl configerrors`, then re-record. |
| Global shortcuts stopped working after using ⚙ Settings → *Record* | The recorder releases the submap on Esc, on Cancel and after 10s. If it was killed mid-recording: `hyprctl dispatch 'hl.dsp.submap("reset")'` |
| Installer stops at the package step | `sudo pacman -Syu` if the package database is out of date, then `sudo pacman -S --needed` with the packages it printed, then run the installer again |
| Build fails: `vte-2.91-gtk4` / GTK headers missing | `omarchy pkg add vte4 gtk4 gtk4-layer-shell pkgconf`, then `./rebuild.sh` |
| Installer says rustup's default is too old or not working | `rustup default stable`, then run the installer again |
| Theme changes don't restyle the overlay | Check `~/.config/omarchy/hooks/theme-set.d/super-desktop` is executable; run `super-desktop reload-theme` manually |
| Card shows another card's prompt in its title | Fixed in current code (owned-session resolution + self-heal). Update to latest (run the installer again); titles correct themselves within ~30s of the overlay being visible |
| Need daemon logs | `/tmp/super-desktop-daemon.log` |
| Nuclear reset of overlay state | Back up then delete `~/.config/super-desktop/state.json`; `super-desktop kill`; start daemon (cards/notes start fresh; tmux sessions are recreated on demand) |

### 5. Updating an existing install

Run the install command again, then `super-desktop status`. It fast-forwards the clone the install runs from, rebuilds, and restarts the daemon and bridge when run inside the Hyprland session. A clone with local changes is left as it is: there, use `git pull && ./rebuild.sh`. The user can do the same from ⚙ Settings → Updates, which checks the clone's upstream branch on GitHub, fast-forwards it and runs `rebuild.sh` (log: `~/.local/state/super-desktop/update.log`).

---

## 🏗️ Project layout (for contributors)

PC-to-PC workspaces (user guide: [Use another PC's harnesses](#use-another-pcs-harnesses)):
a remote workspace is drawn
by the same widgets a local one is (`harness_bar.rs`, `mini_terminal.rs`) with
only the source swapped (`card_source.rs`); every remote control is one typed
command carrying the card revision this view drew. The
peer CLI pairs PCs, fetches remote layouts, streams one console (`peer-attach`) or its
events (`peer-events`) and sends one command (`peer-command`);
`python3 tests/two_pc_matrix.py` runs the simulated two-PC regression matrix.

```
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
├── .githooks/pre-commit     # raises the patch version on every commit (git config core.hooksPath .githooks)
├── assets/                  # vendored toolbar logos → ~/.config/super-desktop/assets/
├── Cargo.toml               # gtk4, gtk4-layer-shell, vte4, serde, serde_json, chrono, libc
├── install.sh               # one-command install and update (see §1 above)
├── rebuild.sh               # rebuild + Hyprland reload + daemon restart (see §3 above)
└── super-desktop.desktop    # desktop entry template
```

Key invariants for contributors: one daemon per machine; one tmux client per card with per-session `detach-on-destroy on`; card titles may only ever reflect the prompt typed **into that card's own harness** (see `resolve_own_opencode_id`); the toggle shortcut is only ever written inside the marked block in `bindings.lua` (see `src/shortcut.rs`); the hot corner must stay a pointer gesture — never a key grab, and never more than a few pixels wide (see `src/hotcorner.rs`); run `cargo test` before every rebuild.
