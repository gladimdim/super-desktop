[← Back to README](../README.md)

# Supported AI Harnesses, Terminals & SD Editor

## Supported AI harnesses

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

## Launchers, cloning & parameters

Fresh installations show the first three detected harnesses in the toolbar (or
fewer if fewer are installed). Settings → Harness launchers lets you enable more;
existing installations keep their selection. Arrange, Settings, and Hide stay at
the display's right edge. When the toolbar does not fit the display at full
size, it gives things up in order until it does: first the shortcut hint after
Hide (its tooltip still shows the shortcut), then the brand label shrinks to its
⚡ icon and the launchers show their logos without names (hover one for its
name). Everything comes back on a wider display or with fewer launchers turned
on, and whatever still does not fit scrolls horizontally.

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

Every launcher row also has a **Clone** button. It copies that launcher as a
custom launcher of its own — keeping its executable and current arguments — and
opens the edit form on the copy, so you can rename it and change its startup
arguments (for example a second OpenCode with `--model …`) without touching the
original. **Cancel** closes the form but keeps the copy in the list.

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

## Harness logos

Linux launchers, settings and terminal cards use bundled SVG product marks,
shared with Android's harness views and widgets. They scale with the UI and
use the active theme’s accent and background colors, including live theme changes. Custom icons remain user-selected;
Herder retains its fallback until a verified SVG is available. See
[logo sources and hashes](../assets/logos/ATTRIBUTION.md).

## Terminal copy and paste

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

## SD Editor

The lightning bolt with a pencil in the top bar, next to 📝, opens
**SD Editor**, the built-in text editor, as a card on the desktop. It opens on the folder chosen in the top
bar: the folder's files in a tree on the left (right-click for a new file or
folder, rename, or move to the trash) and open files in tabs on the right,
with syntax colors from your theme, line numbers, find and replace and undo.
The File menu and the usual shortcuts do New, Open, Save, Save as, Close,
Find, Replace and Go to line; questions such as "save changes?" are asked
inside the card. Images open in a preview with zoom (animated GIFs play), and
sound and video in a player. It drags, resizes, arranges and slides like the
other cards and comes back with its files after a restart.
**Settings → SD Editor** turns that button off (and opens the editor
without it). Plugins can open it too (`editor.open`).
