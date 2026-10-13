---
name: super-desktop-plugin
description: >
  Create, change or fix a SUPER DESKTOP plugin: a folder of QML (Qt Quick) that
  adds docks at the screen edges, items in the top bar, floating panels, cards
  on the desktop (games, tools, terminals), full-screen views and Settings
  sections to the SUPER DESKTOP overlay on Linux (Hyprland/Omarchy). Covers
  plugin.json, the `sd` object in QML (theme, saved values, calls into SUPER
  DESKTOP, programs and pseudo-terminals), and the check/snapshot loop that
  lets you see your work. Use when the folder has a plugin.json for
  SUPER DESKTOP, or when asked for a SUPER DESKTOP widget, dock, toolbar or
  plugin.
---

# Writing a SUPER DESKTOP plugin

SUPER DESKTOP is a full-screen overlay for Hyprland (Omarchy) with notes and
terminal cards that run AI coding agents. A **plugin is a folder** with a
`plugin.json` and QML files. SUPER DESKTOP watches the folder: every time you
save a file, the plugin reloads on screen. There is nothing to build or
install.

Plugins live in `~/.config/super-desktop/plugins/<id>/` (`super-desktop plugin
path` prints the folder). The folder name is the plugin id.

## The loop (do this every time)

1. Edit the QML or `plugin.json`.
2. `super-desktop plugin check <id> --json` → fix every error. Errors give the
   file, line and column. Warnings are bugs too (an undefined property is
   usually a typo).
3. `super-desktop plugin snapshot <id> --surface <surface id> --out /tmp/<id>.png --json`
   for **each** surface → **open the PNG and look at it.** `paintedFraction: 0`
   means nothing visible was drawn. Use `--width`, `--height` and `--scale 2`
   to see other sizes. Snapshots use the user's current theme and saved values.
4. When the user has SUPER DESKTOP open, the plugin is already live. Runtime
   errors and `sd.log` output: `super-desktop plugin logs <id>`.
5. Repeat until check is clean and the snapshot looks right.

Never ask the user to restart SUPER DESKTOP for a plugin change.

## plugin.json

```json
{
  "id": "clock-dock",
  "name": "Clock dock",
  "description": "A clock and launchers on the right edge.",
  "api": 1,
  "surfaces": [
    { "id": "main", "slot": "dock.right", "qml": "Dock.qml", "size": 64 }
  ]
}
```

| Field | Rule |
| --- | --- |
| `id` | Lowercase letters, digits, `-`; 1–48 characters; **equal to the folder name**. |
| `api` | `1`. |
| `name`, `description` | Shown to the user. |
| `enabled` | Optional, default `true`. `false` keeps the folder but shows nothing. |
| `surfaces` | 1–8 surfaces. |
| `surfaces[].id` | Lowercase letters, digits, `-`; unique in the plugin. |
| `surfaces[].slot` | Where it goes (below). |
| `surfaces[].qml` | A `.qml` file relative to the plugin folder. |
| `surfaces[].size` | Width in logical pixels; range and default per slot (below). |
| `surfaces[].height` | Height in logical pixels for `panel` and `settings.page`. |
| `surfaces[].title` | Optional, ≤ 60 characters: the panel's header or the Settings section's title. Default: the plugin name. |
| `shortcuts` | Optional, up to 8: `{"keys": "Ctrl+Alt+T", "action": "card.toggle", "surface": "term"}`. Work while SUPER DESKTOP is shown, before any card gets the key. `keys`: Super, or two of Ctrl/Alt/Shift, plus one key (a letter, digit, punctuation, `F1`–`F12`, `Space`, `Return`, `Tab`, `Up`…). `action`: `panel`, `card` or `fullscreen` + `.open`/`.close`/`.toggle`, for one of this plugin's surfaces of that kind. The first plugin (by id) wins a combination two plugins share. |

### Slots

| Slot | What it is | `size` | `height` |
| --- | --- | --- | --- |
| `dock.left`, `dock.right` | A vertical toolbar at that edge, from below the top bar to the bottom of the screen. Several docks on one edge sit side by side, ordered by plugin id. Arrange keeps cards clear of them. | 32–480, default 64 | the screen's |
| `toolbar.item` | An item in the top bar, after the agent launchers. The bar's height (about 26–50 px, it follows the user's top-bar size). Keep it short: the launcher row scrolls when full. | 24–240, default 96 | the bar's |
| `panel` | A floating card the user can move, resize and close (✕). Not shown until opened with `sd.call("panel.open"/"panel.toggle", {surface})`, usually from a `toolbar.item` or dock button, or from Settings → Plugins. Runs only while open. | 200–1600, default 480 | 120–1200, default 360 |
| `settings.page` | A section under the plugin in Settings → Plugins, as wide as the page. Runs only while that page is shown. Use it for options, saved with `sd.setValue`. | ignored | 60–1200, default 240 |
| `card` | A card on the desktop next to notes and terminals, with a header (title, ✕). The user drags and resizes it; Arrange places it; it slides in and out with the other cards. Opened with `sd.call("card.open"/"card.toggle", {surface})` or from Settings → Plugins; reopens where it was after a restart if it was open. Games, tools and terminals go here. | 200–4000, default 640 | 120–3000, default 420 |
| `fullscreen` | Covers the whole overlay until **Esc** (or `fullscreen.close`). Opened with `sd.call("fullscreen.open", {surface})`. One at a time. Games, screensavers, presentations. | the screen's | the screen's |

Each surface is its own QML program: surfaces of one plugin share data only
through `sd.values` (and files or programs you run).

## QML rules

1. **The root is an `Item`** (`Rectangle`, `Item`, `ColumnLayout`…), never a
   `Window` or `ApplicationWindow`. SUPER DESKTOP gives it its place and size.
   Use anchors and layouts that follow the root's `width` and `height`.
2. **No windows or system dialogs**: no `Window`, `FileDialog`,
   `MessageDialog`, `Qt.labs.platform`. `Popup`, `Menu` and `ToolTip` from
   `QtQuick.Controls` work inside the surface's area.
3. **Use the theme**: colors from `sd.theme` (`background`, `foreground`,
   `accent`, `muted`, `surface`, `selection`, `red`, `green`, `yellow`,
   `blue`, `magenta`, `cyan`, `orange`), font from `sd.theme.fontFamily`.
   They change live when the user switches the Omarchy theme. Standard
   `QtQuick.Controls` (Button, CheckBox, SpinBox, Slider, TextField…) already
   use the theme's colors. A transparent root shows the desktop through.
   Set `cursorShape` on a `MouseArea` (e.g. `Qt.PointingHandCursor`) for
   clickable things.
4. **Rendering is software**: `ShaderEffect`, `MultiEffect`, Qt Quick 3D and
   `layer.effect` are not available. Plain items, `Canvas`, `Image`, `Text`,
   `Shape`, animations and `Timer` all work.
5. **Don't block**: no busy loops; use `Timer` and the callbacks of `sd.exec`
   and `sd.call`.
6. **Act only on a click or key**: anything that launches, sends or deletes
   follows a user action. `cards.send` types into the user's agents: never
   call it from a timer or on load, and never with text from a program's
   output or a web page.
7. **Keyboard**: a `card` or `fullscreen` surface gets the keyboard when the
   user clicks it (full screen: at once). Put `focus: true` on the item that
   handles `Keys.onPressed`, call `forceActiveFocus()` on click, and show a
   "Click to play" hint while `!activeFocus`. Esc always leaves full screen.
8. Available modules: everything in Qt 6 (`QtQuick`, `QtQuick.Controls`,
   `QtQuick.Layouts`, `QtQuick.Shapes`…). Put your own components next to the
   main file (`Button.qml` beside `Dock.qml` is usable as `Button {}`), or in
   subfolders imported with `import "parts"`.

## The `sd` object

| Member | Meaning |
| --- | --- |
| `sd.theme` | Theme map (above). |
| `sd.pluginId`, `sd.surfaceId`, `sd.pluginDir`, `sd.apiVersion` | Who and where you are. |
| `sd.connected` | `false` during `check` and `snapshot` (calls then fail with `not_connected`). |
| `sd.call(method, params, function(result, error) {})` | Ask SUPER DESKTOP to do something (table below). |
| `sd.exec([program, args…], function(r) {})` | Run a program; `r.code`, `r.stdout`, `r.stderr`. No shell: use `["sh", "-c", "…"]` when you need one. Working folder: the plugin folder. |
| `sd.run([program, args…])` | Start a program and don't wait for it. |
| `sd.log(text)` | Write to the plugin log. |
| `sd.values` | The plugin's saved values (an object), shared by all its surfaces and kept across restarts. Bind to it: `checked: sd.values.dark === true`. Updates live (`sd.valuesChanged`) when any surface saves. |
| `sd.setValue(key, value)` | Save a JSON value (`null` removes it). At most 1 MiB in total. |
| `sd.value(key, fallback)` | Read one value (not a binding: prefer `sd.values.key`). |
| `sd.env(name)` | A variable of the user's environment, e.g. `sd.env("SHELL")`, `sd.env("HOME")`. |
| `sd.openPty([program, args…], {cols, rows, cwd, env})` | Start a program on a pseudo-terminal (for terminal plugins); returns a **Pty** or `null`. At most 8 per surface; they end with the surface. |

A **Pty** has `output(text)` and `exited(code)` signals, `write(text)` (keys
as the program expects them: `"\r"` for Enter, `"\x7f"` for Backspace,
`"\x1b[A"` for Up, Ctrl+C is `"\x03"`), `resize(cols, rows)`,
`terminate(signal)` and the `running` and `pid` properties. `TERM` is
`xterm-256color`: the output holds escape sequences. **To show a terminal,
use `TerminalView` (below)** instead of handling them yourself. Programs started with
`sd.run`, `sd.exec` and `sd.openPty` get the user's environment, display
included, so GUI apps open normally.
| `sd.visibleChanged(visible)` | Signal: the overlay was shown or hidden (`Connections { target: sd; function onVisibleChanged(v) {} }`). |

### `sd.call` methods

| Method | Params | Result |
| --- | --- | --- |
| `overlay.hide` | none | `{}` |
| `harness.list` | none | `[{id, name, icon}]`: the agents offered in the top bar. `icon` is a file path or empty. |
| `harness.launch` | `{harness, folder?}` | `{}`; opens a new terminal card for that agent, in `folder` or the folder chosen in the top bar. |
| `cards.list` | none | `[{id, kind, title, harness, folder, status, minimized}]`; `kind` is `terminal` or `note`; `status` (terminals) is `working`, `waiting`, `finished`, `idle`, `error`, `exited`, `unknown` or `remote`. Poll it (every few seconds, only while `sd.visibleChanged` says shown). |
| `cards.focus` | `{id}` from `cards.list` | `{}`; brings that card to the front with the keyboard (restores a minimized one). |
| `cards.text` | `{id, lines?}` of a terminal card (default 60, at most 300) | `{text}`: the end of its terminal, plain text. |
| `cards.send` | `{id, text, enter?}` of a terminal card | `{}`; types `text` into it as if the user did (`enter: true` presses Enter after it; at most 64 KiB). **Only on the user's click**, and show what will be typed first. |
| `notify` | `{title, body}` | `{}`; a desktop notification. |
| `panel.open`, `panel.close`, `panel.toggle` | `{surface}`: one of **this plugin's** `panel` surfaces | `{open}`: whether it is open now. |
| `card.open`, `card.close`, `card.toggle` | `{surface}`: one of this plugin's `card` surfaces | `{open}` |
| `fullscreen.open`, `fullscreen.close`, `fullscreen.toggle` | `{surface}`: one of this plugin's `fullscreen` surfaces | `{open}` |

Errors are `{code, message, hint}`: `unknown_method`, `bad_params`,
`unavailable`, `not_connected`. Show them in the UI where they happen.

## Terminals: `TerminalView`

`import SuperDesktop.Terminal` gives `TerminalView`, a complete terminal
(pty, escape sequences, 16/256/true colors, the alternate screen, scroll
regions, scrollback, keys, paste). vim, htop, btop and shells run in it.

```qml
import QtQuick
import SuperDesktop.Terminal

TerminalView {
    anchors.fill: parent
    command: ["btop"]          // empty (default): the user's $SHELL
    onExited: (code) => sd.log("btop ended: " + code)
}
```

| Member | Meaning |
| --- | --- |
| `command`, `workingDirectory` (default `$HOME`), `environment`, `autoStart` (default true) | What runs. |
| `fontFamily` (default `monospace`), `fontPixelSize` (13), `foreground`, `background`, `cursorColor`, `scrollbackLines` (2000) | Look; colors default to the theme. |
| `cols`, `rows`, `running`, `title` (the program's window title) | Read-only. |
| `start()`, `stop()`, `send(text)` (type into it), `paste(text)`, `feed(text)` (draw as if printed), `lineText(row)`, `cursorPosition()`, `cellAt(row, col)` | Methods. |
| `exited(code)`, `bell()` | Signals. |

Click it to type. Ctrl+Shift+V or Shift+Insert pastes (needs `wl-paste`),
middle click pastes the selection, Shift+PageUp/PageDown and the wheel
scroll back. With no program running, keys go to your own `Keys` handlers
(for example Enter to `start()` again). Wide characters (CJK, emoji) take
one cell. Selecting text with the mouse is not supported yet.

## Recipes

Full working examples, copy the closest one:

- `examples/clock-dock/`: a dock with clock, date and battery (`sd.exec`).
- `examples/launcher-dock/`: a dock with a button per agent
  (`harness.list`, `harness.launch`), hover states, tooltips, a component file.
- `examples/pomodoro/`: a `toolbar.item` that toggles a `panel`, and a
  `settings.page`, sharing state through `sd.values`.
- `examples/agents-dock/`: a dock listing the agent cards with their status
  (`cards.list`), a click jumps to one (`cards.focus`).
- `examples/snake/`: a game on a `card`: a `Timer` game loop, arrow keys,
  focus handling, the best score in `sd.values`.
- `examples/terminal/`: a terminal card made of `TerminalView`, with the
  program's title and Enter to restart.

A themed icon button:

```qml
import QtQuick
import QtQuick.Controls

Rectangle {
    id: button
    property string label: "?"
    property string tip: ""
    signal clicked()
    width: parent.width; height: width
    radius: 10
    color: area.containsMouse ? sd.theme.accent : sd.theme.surface
    Text {
        anchors.centerIn: parent
        text: button.label
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.pixelSize: 20
    }
    MouseArea { id: area; anchors.fill: parent; hoverEnabled: true; onClicked: button.clicked() }
    ToolTip.visible: area.containsMouse && tip.length > 0
    ToolTip.text: tip
}
```

## Other commands

- `super-desktop plugin new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page|card|fullscreen] [--name "…"]`:
  a working plugin to start from (`panel`, `card` and `fullscreen` also make
  the top-bar item that opens it).
- `super-desktop plugin list [--json]`: every plugin, whether it is valid and
  whether it is running.
- `super-desktop plugin doctor`: when nothing appears at all, this says what
  is missing (the QML host, Qt Quick, the overlay) and how to fix it.
- Sharing: a plugin is shared as a git repository with `plugin.json` at its
  top level (the folder itself). Others run
  `super-desktop plugin install <git url>`; it does not run until they approve
  it (Settings → Plugins or `super-desktop plugin approve <id>`).
  `super-desktop plugin update <id>` fast-forwards it; `remove` moves it to the
  trash. Keep the id stable: it is the folder name on every PC. Do not commit
  `.claude/skills/` (SUPER DESKTOP adds the current skill itself).
- To turn a plugin off: `"enabled": false` in `plugin.json` (the user has an
  ON/OFF switch in Settings → Plugins), or move the folder out of the plugins
  folder.
