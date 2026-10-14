---
name: super-desktop-plugin
description: >
  Create, change or fix a SUPER DESKTOP plugin: a folder of QML (Qt Quick) that
  adds docks at the screen edges, items in the top bar, floating panels,
  popups (menus, quick forms), cards on the desktop (games, tools, terminals),
  full-screen views and Settings sections to the SUPER DESKTOP overlay on Linux (Hyprland/Omarchy). Covers
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

This copy of the skill comes from the SUPER DESKTOP installed on this PC,
which puts it in the plugin folder and refreshes it after every update: all
it describes works here, whatever version number SUPER DESKTOP reports.

## The loop (do this every time)

1. Edit the QML or `plugin.json`.
2. `super-desktop plugin check <id> --json` → fix every error. Errors give the
   file, line and column. Warnings are bugs too (an undefined property is
   usually a typo).
3. `super-desktop plugin snapshot <id> --surface <surface id> --out /tmp/<id>.png --json`
   for **each** surface → **open the PNG and look at it.** `paintedFraction: 0`
   means nothing visible was drawn. Use `--width`, `--height` and `--scale 2`
   to see other sizes (a `toolbar.item` renders 36 px tall; the bar's items
   are 26–50 px, so try `--height 26` and `--height 50`). Snapshots use the
   user's current theme and saved values;
   `--values sample.json` (one JSON object, as `sd.values` sees it) shows other
   states (a full list, an empty one, an alarm going off), and `--wait 120`
   catches an animation part-way. What the QML saves during a snapshot is
   thrown away.
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
| `surfaces[].multiple` | Optional, `card` only: the user can open several copies (`card.new`), e.g. several terminals. Each copy is its own QML program with its own `sd.instanceId` and place. |
| `surfaces[].tooltip` | Optional, ≤ 120 characters, docks and `toolbar.item` only: the text shown when the pointer rests on the surface, e.g. `"Click to add · right-click for the list"`. Default: the plugin name. Use it instead of a QML `ToolTip` there. |
| `shortcuts` | Optional, up to 8: `{"keys": "Ctrl+Alt+T", "action": "card.toggle", "surface": "term"}`. Work while SUPER DESKTOP is shown, before any card gets the key. `keys`: Super, or two of Ctrl/Alt/Shift, plus one key (a letter, digit, punctuation, `F1`–`F12`, `Space`, `Return`, `Tab`, `Up`…). `action`: `panel`, `card` or `fullscreen` + `.open`/`.close`/`.toggle`, or `card.new`, for one of this plugin's surfaces of that kind. The first plugin (by id) wins a combination two plugins share. |

### Slots

| Slot | What it is | `size` | `height` |
| --- | --- | --- | --- |
| `dock.left`, `dock.right` | A vertical toolbar at that edge, from below the top bar to the bottom of the screen. Several docks on one edge sit side by side, ordered by plugin id. Arrange keeps cards clear of them. | 32–480, default 64 | the screen's |
| `toolbar.item` | An item in the top bar, after the agent launchers. The bar's height (about 26–50 px, it follows the user's top-bar size). Keep it short: the launcher row scrolls when full. | 24–240, default 96 | the bar's |
| `panel` | A floating card the user can move, resize and close (✕). Not shown until opened with `sd.call("panel.open"/"panel.toggle", {surface})`, usually from a `toolbar.item` or dock button, or from Settings → Plugins. Runs only while open. | 200–1600, default 480 | 120–1200, default 360 |
| `settings.page` | A section under the plugin in Settings → Plugins, as wide as the page. Runs only while that page is shown. Use it for options, saved with `sd.setValue`. | ignored | 60–1200, default 240 |
| `card` | A card on the desktop next to notes and terminals, with a header (title, ✕). The user drags and resizes it; Arrange places it; it slides in and out with the other cards. Opened with `sd.call("card.open"/"card.toggle", {surface})` or from Settings → Plugins; reopens where it was after a restart if it was open. Games, tools and terminals go here. | 200–4000, default 640 | 120–3000, default 420 |
| `fullscreen` | Covers the whole overlay until **Esc** (or `fullscreen.close`). Opened with `sd.call("fullscreen.open", {surface})`. One at a time. Games, screensavers, presentations. | the screen's | the screen's |
| `popup` | A small box with no header, like a menu: below the `toolbar.item` that opened it, beside a dock (inside the screen, flipped when there is no room), or below the point the caller gives. Opened with `sd.call("popup.open"/"popup.toggle", {surface})`. It opens at the size in plugin.json; `popup.resize` fits it to its content. It gets the keyboard at once; a click outside it, **Esc**, hiding the overlay or `popup.close` closes it. One popup at a time in the whole overlay. Runs only while open. Menus, pickers, quick forms, lists. | 120–800, default 280 | 40–800, default 240 |

Each surface is its own QML program: surfaces of one plugin share data only
through `sd.values` (and files or programs you run).

## QML rules

1. **The root is an `Item`** (`Rectangle`, `Item`, `ColumnLayout`…), never a
   `Window` or `ApplicationWindow`. SUPER DESKTOP gives it its place and size.
   Use anchors and layouts that follow the root's `width` and `height`.
2. **No windows or system dialogs**: no `Window`, `FileDialog`,
   `MessageDialog`, `Qt.labs.platform`. `Popup`, `Menu` and `ToolTip` from
   `QtQuick.Controls` work only inside the surface's area: a `toolbar.item`
   is about 40 px tall, so there they have no room and cover the item itself
   (a hover `ToolTip` then swallows its clicks). For a menu or a form that
   opens from a top-bar item or a dock, use a `popup` surface instead, and
   for hover text the surface's `tooltip` in plugin.json.
3. **Use the theme**: colors from `sd.theme` (`background`, `foreground`,
   `accent`, `muted`, `surface`, `selection`, `red`, `green`, `yellow`,
   `blue`, `magenta`, `cyan`, `orange`), font from `sd.theme.fontFamily`.
   They change live when the user switches the Omarchy theme. **`muted` is
   for borders and dividers, not text**: on its own background it can be
   nearly invisible. Some themes also make `red` or `yellow` a shade of blue,
   or too dark to read. Write text in `foreground` (secondary text: it faded
   toward `background`), and check any other text color against
   `background` (see "Pitfalls and recipes"). Standard
   `QtQuick.Controls` (Button, CheckBox, SpinBox, Slider, TextField…) already
   use the theme's colors: `Button { highlighted: true }` (and checked
   switches, sliders, progress bars) take the accent with readable text. A transparent root shows the desktop through.
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
| `sd.pluginId`, `sd.surfaceId`, `sd.instanceId`, `sd.pluginDir`, `sd.apiVersion` | Who and where you are. `instanceId` is `""` for the first copy of a card, then `"2"`, `"3"`… |
| `sd.connected` | `false` during `check` and `snapshot` (calls then fail with `not_connected`). |
| `sd.call(method, params, function(result, error) {})` | Ask SUPER DESKTOP to do something (table below). |
| `sd.exec([program, args…], function(r) {})` | Run a program; `r.code`, `r.stdout`, `r.stderr`. No shell: use `["sh", "-c", "…"]` when you need one. Working folder: the plugin folder. |
| `sd.run([program, args…])` | Start a program and don't wait for it. |
| `sd.log(text)` | Write to the plugin log. |
| `sd.values` | The plugin's saved values (a plain JS object; lists are JS arrays), shared by all its surfaces and kept across restarts. Bind to it: `checked: sd.values.dark === true`. Updates live (`sd.valuesChanged`) when any surface saves. Do not change it in place: copy, change the copy, `sd.setValue` it. Kept in `~/.local/state/super-desktop/plugins/<id>.data.json`. |
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
| `sd.visible` | Whether SUPER DESKTOP is on screen. Bind animations to it (`running: ringing && sd.visible`): surfaces are not drawn while it is hidden. |
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
| `llm.complete` | `{prompt, system?, maxTokens? (default 1024), json? (answer is one JSON document), fast? (a smaller, quicker model)}`; prompt ≤ 64 KiB | `{text, provider, model}`, through the user's own Claude Code (else Codex) with all tools off. Takes seconds; at most two at once (`busy`); errors `unavailable`, `timeout`. **Send only what the task needs**: never secrets, keys or `.env` contents, and say in the plugin's description what it sends. |
| `panel.open`, `panel.close`, `panel.toggle` | `{surface}`: one of **this plugin's** `panel` surfaces | `{open}`: whether it is open now. |
| `card.open`, `card.close`, `card.toggle` | `{surface, instance?}`: one of this plugin's `card` surfaces; `instance` picks a copy of a `multiple` card (default: the caller's own copy when the caller is that card, else the first) | `{open}` |
| `card.new` | `{surface}` of a `multiple` card | `{}`; opens one more copy. |
| `fullscreen.open`, `fullscreen.close`, `fullscreen.toggle` | `{surface}`: one of this plugin's `fullscreen` surfaces | `{open}` |
| `popup.resize` | `{surface, width?, height?}` of this plugin's **open** popup, usually from the popup itself: `onImplicitHeightChanged: sd.call("popup.resize", {surface: sd.surfaceId, height: implicitHeight})` on its layout. Clamped to the slot's limits. | `{width, height}` |
| `popup.open`, `popup.close`, `popup.toggle` | `{surface, x?, y?, width?, height?}`: one of this plugin's `popup` surfaces. It opens next to the caller: below a `toolbar.item`, beside a dock. `x, y` (and `width, height`) are a point or box in the **caller's** coordinates to open below instead, e.g. `var p = button.mapToItem(null, 0, 0)` then `{surface, x: p.x, y: p.y, width: button.width, height: button.height}` for one button of a dock. Opening one closes any other popup. Use `toggle` on the button: a click on the button that opened it closes it and its toggle then leaves it closed. A popup closes itself with `{surface: sd.surfaceId}`. | `{open}` |

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
| `start()`, `stop()`, `send(text)` (type into it), `paste(text)`, `feed(text)` (draw as if printed), `lineText(row)`, `cursorPosition()`, `cellAt(row, col)`, `selectedText()`, `select(r0, c0, r1, c1)`, `clearSelection()`, `copy()` | Methods. |
| `exited(code)`, `bell()` | Signals. |

Click it to type. Drag to select (it becomes the primary selection),
Ctrl+Shift+C copies, Ctrl+Shift+V or Shift+Insert pastes, middle click pastes
the primary selection (these need `wl-clipboard`). Shift+PageUp/PageDown and
the wheel scroll back. With no program running, keys go to your own `Keys`
handlers (for example Enter to `start()` again). Wide characters (CJK,
emoji) take one cell.

## Recipes

Full working examples, copy the closest one:

- `examples/clock-dock/`: a dock with clock, date and battery (`sd.exec`).
- `examples/launcher-dock/`: a dock with a button per agent
  (`harness.list`, `harness.launch`), hover states, tooltips, a component file.
- `examples/pomodoro/`: a `toolbar.item` that toggles a `panel`, and a
  `settings.page`, sharing state through `sd.values`.
- `examples/agents-menu/`: a `toolbar.item` whose click opens a `popup`
  below it, listing the agents (`harness.list`); a click on one opens its
  card (`harness.launch`) and closes the popup (`popup.close`).
- `examples/reminder/`: a bell in the top bar. Click: a `popup` form (when,
  what; quick presets, text fields, Enter/Esc); right click: a `popup` list
  that removes what you click. A list saved in `sd.values`, a `Timer` that
  checks what is due (in the `toolbar.item`, which keeps running), a
  notification, a sound, a shaking icon over a moving theme gradient, the
  `tooltip`, and text colors checked for contrast (`reminders.js`).
- `examples/agents-dock/`: a dock listing the agent cards with their status
  (`cards.list`), a click jumps to one (`cards.focus`), and ✦ summarizes what
  an agent is doing (`cards.text` → `llm.complete`).
- `examples/snake/`: a game on a `card`: a `Timer` game loop, arrow keys,
  focus handling, the best score in `sd.values`.
- `examples/terminal/`: terminal cards made of `TerminalView` (a `multiple`
  card: each click or Ctrl+Alt+T opens another), with the program's title and
  Enter to restart.

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

## Pitfalls and recipes

**What runs when.** Docks and `toolbar.item` surfaces run as long as the
plugin is loaded, also while SUPER DESKTOP is hidden: their `Timer`s keep
going, but nothing is drawn. Panels, popups, cards and settings pages run only
while shown, so work that must happen in the background (checking a clock,
polling) belongs in a dock or toolbar item. Animations: `running: sd.visible`.

**Saving a list.** Read, copy, change, save the whole key; all surfaces see it:

```js
function add(item) {
    var list = (sd.values.items || []).slice()   // a copy
    list.push(item)
    sd.setValue("items", list)
}
function remove(id) {
    sd.setValue("items", (sd.values.items || []).filter(i => i.id !== id))
}
```

Two surfaces that save the same key at the same moment: the last one wins.
Let one surface own a key, or save small keys. One surface adding and another
removing (a history the toolbar item appends to and a popup prunes) is fine:
the user cannot do both at the same instant, and each reads the latest
`sd.values` before it saves. To look at what is saved:
`cat ~/.local/state/super-desktop/plugins/<id>.data.json`.

**Testing what happens on an event** (a timer ending, a reminder due): in
`check` and `snapshot`, `sd.connected` is false and `sd.call`/`sd.exec` fail,
so keep the decision in plain functions (what is due, what to save) and the
side effects (notify, sound) in a few lines guarded by `sd.connected`.
Snapshot the states with `--values`: times are epoch milliseconds, so write
the sample just before the snapshot (`N=$(date +%s%3N)`, then
`"endsAt": $((N + 60000))`). To see it live, ask the user first (it makes a
sound), then use a very short length (0.1 minutes).

**Adding files**: write a new QML file before plugin.json names it. Every
save reloads the plugin on screen, and a surface whose file is missing shows
an error until it exists.

**Readable colors** in every theme (light and dark): compare with
`background` and fall back to `foreground`. In a `.pragma library` JS file:

```js
function lum(c) {
    var s = String(c); if (s.length === 9) s = "#" + s.slice(3)
    var v = [1, 3, 5].map(i => parseInt(s.substr(i, 2), 16) / 255)
        .map(x => x <= 0.03928 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4))
    return 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]
}
function contrast(a, b) { var x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05) }
// The first theme color that reads on the background, else the foreground.
function readable(theme, names) {
    for (var i = 0; i < names.length; i++)
        if (theme[names[i]] && contrast(theme[names[i]], theme.background) >= 4.5) return theme[names[i]]
    return theme.foreground
}
```

Then `color: R.readable(sd.theme, ["accent", "blue", "cyan"])`; secondary
text: `Qt.alpha(sd.theme.foreground, 0.7)`. `examples/reminder/reminders.js`
has the full version (`soft`, `mix`).

**A sound**: put a `.wav` in the plugin folder and play it on the event,
with whatever player the PC has:

```js
sd.exec(["sh", "-c", "pw-play \"$1\" 2>/dev/null || paplay \"$1\" 2>/dev/null || aplay -q \"$1\"",
         "sh", sd.pluginDir + "/ding.wav"], function(r) { if (r.code !== 0) sd.log(r.stderr) })
```

`/usr/share/sounds/freedesktop/stereo/` (`.oga`, for `pw-play`/`paplay`) has
standard sounds. Play only for something the user asked for (a reminder,
a finished task), and not again and again without end.

**Popups from a button**: `sd.call("popup.toggle", {surface})` on the click.
Inside the popup, `sd.call("popup.close", {surface: sd.surfaceId})` after a
choice. A dock with several buttons passes the button's box
(`var p = button.mapToItem(null, 0, 0)`; `{surface, x: p.x, y: p.y, width:
button.width, height: button.height}`) so the popup opens next to it.

**A preview picture.** Put `preview.png` at the top of the plugin folder:
1280×640 (2:1), at most 1.5 MiB, the plugin in use with sample data, on the
theme's background, no text around it. SUPER DESKTOP shows it on the
plugin's card in Settings → Plugins → Discover and next to it under
Installed (cropped to fill, so keep the middle meaningful), and GitHub can
use it as the repository's social preview. Make it from snapshots of the
real surfaces with `--values` and `--scale 2`, for example the
`toolbar.item` and, below it, the popup or panel it opens; with ImageMagick:

```sh
super-desktop plugin snapshot <id> --surface main --scale 2.5 --values sample.json --out /tmp/button.png
super-desktop plugin snapshot <id> --surface menu --scale 1.75 --values sample.json --out /tmp/popup.png
magick -size 1280x640 xc:'#121212' -fill '#1e1e1e' -draw 'rectangle 0,0,1280,112' \
  /tmp/button.png -gravity north -geometry +0+11 -composite \
  /tmp/popup.png -gravity north -geometry +0+128 -composite preview.png
```

Use the colors of `~/.local/state/super-desktop/plugins/theme.json`
(`background`, `surface`), then open preview.png and look at it.

**Publishing.** Push the plugin folder as its own git repository
(`plugin.json` and `preview.png` at the top, a README that says what it does on the user's PC:
programs it runs, files it writes, anything it sends; `.claude/skills/` in
`.gitignore`). Check it installs: `super-desktop plugin install <url>` on a
fresh folder (`SD_PLUGINS_DIR=/tmp/x/plugins SD_PLUGINS_STATE_DIR=/tmp/x/state`).
Then list it in the public registry, github.com/gladimdim/super-desktop-plugins:
a pull request adding to `registry.json`, in id order,
`{"id", "name", "description", "repo": "owner/name", "tags", "author",
"api": 1, "minHost": "<lowest SUPER DESKTOP version>", "slots": [sorted]}`
(`id`, `name`, `api` and `slots` must match `plugin.json`; its
`validate.py` checks).

## Other commands

- `super-desktop plugin new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page|card|fullscreen|popup] [--name "…"]`:
  a working plugin to start from (`panel`, `card`, `fullscreen` and `popup` also make
  the top-bar item that opens it).
- `super-desktop plugin list [--json]`: every plugin, whether it is valid and
  whether it is running.
- `super-desktop plugin doctor`: when nothing appears at all, this says what
  is missing (the QML host, Qt Quick, the overlay) and how to fix it.
- Sharing: a plugin is shared as a git repository with `plugin.json` at its
  top level (the folder itself). Others run
  `super-desktop plugin install <git url>`, or `install <id>` for one in the
  public list (`super-desktop plugin search [text]`; Settings → Plugins →
  Get plugins); it does not run until they approve
  it (Settings → Plugins or `super-desktop plugin approve <id>`).
  `super-desktop plugin update <id>` fast-forwards it; `remove` moves it to the
  trash. Keep the id stable: it is the folder name on every PC. Do not commit
  `.claude/skills/` (SUPER DESKTOP adds the current skill itself).
- To turn a plugin off: `"enabled": false` in `plugin.json` (the user has an
  ON/OFF switch in Settings → Plugins), or move the folder out of the plugins
  folder.
