---
name: super-desktop-plugin
description: >
  Create, change or fix a SUPER DESKTOP plugin: a folder of QML (Qt Quick) that
  adds docks at the screen edges, items in the top bar, floating panels,
  popups (menus, quick forms), cards on the desktop (games, tools, terminals),
  full-screen views, Settings sections and buttons on terminal cards to the
  SUPER DESKTOP overlay on Linux (Hyprland/Omarchy), and moves, sizes,
  minimizes or closes the terminal cards. Covers
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
   usually a typo). `lint` lists QML that loads but is wrong here (`error`:
   fails the check, e.g. a `ToolTip` in a top-bar item) or usually unwanted
   (`advice`: fixed colors, text in `muted`, a timer running while hidden).
   Advice you have weighed can be kept out with a comment on that line or
   the line above: `// sd-lint: ignore hidden-timer` (the rule's name).
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
4. **Use it like a user**: `--do` clicks, types and checks before the shot
   (see "Testing like a user"). Do it for every button and form you made:
   it shows what each click asked SUPER DESKTOP for and what got saved.
5. When the user has SUPER DESKTOP open, the plugin is already live. Runtime
   errors and `sd.log` output: `super-desktop plugin logs <id>`.
6. Repeat until check is clean, the steps pass and the snapshot looks right.

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
| `version` | Optional: the plugin's version, `"1.0.0"`. Shown in Settings → Plugins; raise it with each release. |
| `minHost` | Optional: the oldest SUPER DESKTOP it works with, `"1.6.0"`. An older SUPER DESKTOP says so instead of failing on the QML. Needed to publish. |
| `categories` | Optional, up to 5 lowercase words: `["productivity", "time"]`. Discover groups plugins by them. |
| `enabled` | Optional, default `true`. `false` keeps the folder but shows nothing. |
| `replacesLaunchers` | Optional, default `false`. `true`: while the plugin runs, the top bar hides SUPER DESKTOP's own agent buttons, because this plugin launches the agents (`harness.list`, `harness.launch`) from its `toolbar.item` or dock, which it must have. They come back when the user turns the plugin off, removes it, or its item fails to load. Older SUPER DESKTOP versions ignore it and keep the buttons. |
| `surfaces` | 1–8 surfaces. |
| `surfaces[].id` | Lowercase letters, digits, `-`; unique in the plugin. |
| `surfaces[].slot` | Where it goes (below). |
| `surfaces[].qml` | A `.qml` file relative to the plugin folder. |
| `surfaces[].size` | Width in logical pixels; range and default per slot (below). |
| `surfaces[].height` | Height in logical pixels for `panel` and `settings.page`. |
| `surfaces[].title` | Optional, ≤ 60 characters: the panel's header or the Settings section's title. Default: the plugin name. |
| `surfaces[].multiple` | Optional, `card` only: the user can open several copies (`card.new`), e.g. several terminals. Each copy is its own QML program with its own `sd.instanceId` and place. |
| `surfaces[].tooltip` | Optional, ≤ 120 characters, docks and `toolbar.item` only: the text shown when the pointer rests on the surface, e.g. `"Click to add · right-click for the list"`. Default: the plugin name. Use it instead of a QML `ToolTip` there. |
| `cardButtons` | Optional, up to 4: buttons SUPER DESKTOP adds to the header of every terminal card (see "Card buttons"). `{"id": "left", "icon": "◧", "tooltip": "Move to the left half", "cards": "all"}`: `id` like a surface id; `icon` one or two symbols or one emoji; `tooltip` 1–60 characters, the hover text and the button's name in the ⋯ menu; `cards` `all` (default), `agents` (cards running an AI agent) or `shells` (plain terminals). Needs a `toolbar.item` or dock surface: it receives the clicks. |
| `shortcuts` | Optional, up to 8: `{"keys": "Ctrl+Alt+T", "action": "card.toggle", "surface": "term"}`. Work while SUPER DESKTOP is shown, before any card gets the key. `keys`: Super, or two of Ctrl/Alt/Shift, plus one key (a letter, digit, punctuation, `F1`–`F12`, `Space`, `Return`, `Tab`, `Up`…). `action`: `panel`, `card` or `fullscreen` + `.open`/`.close`/`.toggle`, or `card.new`, for one of this plugin's surfaces of that kind. The first plugin (by id) wins a combination two plugins share. |

### Slots

| Slot | What it is | `size` | `height` |
| --- | --- | --- | --- |
| `dock.left`, `dock.right` | A vertical toolbar at that edge, from below the top bar to the bottom of the screen. Several docks on one edge sit side by side, ordered by plugin id. Arrange keeps cards clear of them. | 32–480, default 64 | the screen's |
| `toolbar.item` | An item in the top bar, after the agent launchers. The bar's height (about 26–50 px, it follows the user's top-bar size). Keep it short: the launcher row scrolls when full. The user drags toolbar items to rearrange them: a press that moves more than 8 px belongs to the bar, and the item sees the pointer move off it and the press released there, so it is no click. Use clicks, not drags, in a toolbar item. | 24–240, default 96 | the bar's |
| `panel` | A floating card the user can move, resize and close (✕). Not shown until opened with `sd.call("panel.open"/"panel.toggle", {surface})`, usually from a `toolbar.item` or dock button, or from Settings → Plugins. Runs only while open; it stays open while SUPER DESKTOP is hidden, until the user closes it. | 200–1600, default 480 | 120–1200, default 360 |
| `settings.page` | A section under the plugin in Settings → Plugins, as wide as the page. Runs only while that page is shown. Use it for options, saved with `sd.setValue`. | ignored | 60–1200, default 240 |
| `card` | A card on the desktop next to notes and terminals, with a header (title, ✕). The user drags and resizes it; Arrange places it; it slides in and out with the other cards. Opened with `sd.call("card.open"/"card.toggle", {surface})` or from Settings → Plugins; reopens where it was after a restart if it was open. Games, tools and terminals go here. | 200–4000, default 640 | 120–3000, default 420 |
| `fullscreen` | Covers the whole overlay until **Esc** (or `fullscreen.close`). Opened with `sd.call("fullscreen.open", {surface})`. One at a time. Games, screensavers, presentations. | the screen's | the screen's |
| `popup` | A small box with no header, like a menu: below the `toolbar.item` that opened it, beside a dock (inside the screen, flipped when there is no room), or below the point the caller gives. Opened with `sd.call("popup.open"/"popup.toggle", {surface})`. It opens at the size in plugin.json; `popup.resize` fits it to its content. It gets the keyboard at once; a click outside it, **Esc**, hiding the overlay or `popup.close` closes it. One popup at a time in the whole overlay. Runs only while open. Menus, pickers, quick forms, lists. | 120–800, default 280 | 40–800, default 240 |

Each surface is its own QML program: surfaces of one plugin share data only
through `sd.values` (and files or programs you run).

### Card buttons

`cardButtons` in plugin.json put small buttons in the header of terminal
cards, before the card's own 🗕 ⛶ ✕. They take only room the header can
spare: when it has too little for all of them they fold into one ⋯ menu,
and on the narrowest cards they are not shown. A click calls
`sd.cardButtonClicked(button, card)` in every running surface of the plugin
(`button` is the id, `card` the card as `cards.list` gives it); handle it in
the `toolbar.item` or dock, which always run:

```qml
Connections {
    target: sd
    function onCardButtonClicked(button, card) {
        if (button === "left") sd.call("cards.geometry", {id: card.id}, function(g) {
            var a = g.area, w = Math.floor((a.width - 12) / 2)
            sd.call("cards.resize", {id: card.id, width: w, height: a.height}, function() {
                sd.call("cards.move", {id: card.id, x: a.x, y: a.y})
            })
        })
    }
}
```

They are on this PC's own cards only (not on another PC's, nor on the
phone). Settings → Plugins lists them under the plugin.

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
   follows a user action (a card button is one). `cards.send` types into the
   user's agents: never call it from a timer or on load, and never with text
   from a program's output or a web page. `cards.close` ends a session, so
   it asks the user first; call it only on a click too.
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
| `sd.connected` | `false` during `check` and `snapshot`: `sd.call` then fails with `not_connected` and `sd.run` starts nothing; both are listed in the snapshot's report. `sd.exec` runs. |
| `sd.call(method, params, function(result, error) {})` | Ask SUPER DESKTOP to do something (table below). |
| `sd.exec([program, args…], function(r) {})` | Run a program; `r.code`, `r.stdout`, `r.stderr`. No shell: use `["sh", "-c", "…"]` when you need one. Working folder: the plugin folder. |
| `sd.run([program, args…])` | Start a program and don't wait for it. |
| `sd.log(text)` | Write to the plugin log. |
| `sd.values` | The plugin's saved values (a plain JS object; lists are JS arrays), shared by all its surfaces and kept across restarts. Bind to it: `checked: sd.values.dark === true`. Updates live (`sd.valuesChanged`) when any surface saves. Do not change it in place: copy, change the copy, `sd.setValue` it. Kept in `~/.local/state/super-desktop/plugins/<id>.data.json`. |
| `sd.setValue(key, value)` | Save a JSON value (`null` removes it). At most 1 MiB in total. |
| `sd.value(key, fallback)` | Read one value (not a binding: prefer `sd.values.key`). |
| `sd.env(name)` | A variable of the user's environment, e.g. `sd.env("SHELL")`, `sd.env("HOME")`. |
| `sd.openPty([program, args…], {cols, rows, cwd, env})` | Start a program on a pseudo-terminal (for terminal plugins); returns a **Pty** or `null`. At most 8 per surface; they end with the surface. |
| `sd.visible` | Whether SUPER DESKTOP is on screen. Bind animations to it (`running: ringing && sd.visible`): surfaces are not drawn while it is hidden. |
| `sd.visibleChanged(visible)` | Signal: the overlay was shown or hidden (`Connections { target: sd; function onVisibleChanged(v) {} }`). |
| `sd.cardButtonClicked(button, card)` | Signal: one of the plugin's `cardButtons` was clicked on a terminal card (`function onCardButtonClicked(button, card) {}`); `card` is `{id, kind, title, harness, folder, status, minimized}`. A click of the user: the plugin may act on that card. |

A **Pty** has `output(text)` and `exited(code)` signals, `write(text)` (keys
as the program expects them: `"\r"` for Enter, `"\x7f"` for Backspace,
`"\x1b[A"` for Up, Ctrl+C is `"\x03"`), `resize(cols, rows)`,
`terminate(signal)` and the `running` and `pid` properties. `TERM` is
`xterm-256color`: the output holds escape sequences. **To show a terminal,
use `TerminalView` (below)** instead of handling them yourself. Programs started with
`sd.run`, `sd.exec` and `sd.openPty` get the user's environment, display
included, so GUI apps open normally.

### `sd.call` methods

| Method | Params | Result |
| --- | --- | --- |
| `overlay.show` | none | `{}` once SUPER DESKTOP is on screen; e.g. from a notification's action, then `popup.open`. |
| `overlay.hide` | none | `{}` |
| `harness.list` | none | `[{id, name, icon}]`: the agents offered in the top bar. `icon` is a file path or empty. |
| `harness.launch` | `{harness, folder?}` | `{}`; opens a new terminal card for that agent, in `folder` or the folder chosen in the top bar. |
| `workspace.folder` | none | `{folder, recent}`: the folder chosen in the top bar (the user's project: new agent cards start there) and the folders chosen before, newest first. Absolute paths. Start editors, file browsers and git tools there (see "Programs in the user's project"). |
| `editor.open` | `{folder?, file?}`: a folder (default: the top bar's) and a file in it or an absolute path | `{folder}`; opens SD Editor, SUPER DESKTOP's own text editor card, on that folder (its tree on the left, files in tabs, syntax colors, find and replace, Save, Save as; images, sound and video in previews), or brings the one already there to the front, and opens `file` in it. Only on the user's click. |
| `cards.list` | none | `[{id, kind, title, harness, folder, status, minimized}]`; `kind` is `terminal` or `note`; `status` (terminals) is `working`, `waiting`, `finished`, `idle`, `error`, `exited`, `unknown` or `remote`. Poll it (every few seconds, only while `sd.visibleChanged` says shown). |
| `cards.focus` | `{id}` from `cards.list` | `{}`; brings that card to the front with the keyboard (restores a minimized one). |
| `cards.text` | `{id, lines?}` of a terminal card (default 60, at most 300) | `{text}`: the end of its terminal, plain text. |
| `cards.send` | `{id, text, enter?}` of a terminal card | `{}`; types `text` into it as if the user did (`enter: true` presses Enter after it; at most 64 KiB). **Only on the user's click**, and show what will be typed first. |
| `cards.geometry` | `{id}` of a terminal card | `{id, mode, x, y, width, height, area: {x, y, width, height}, minWidth, minHeight}`: `mode` is `normal`, `minimized` (its icon's place) or `expanded`; `area` is where cards may go (below the top bar, inside the docks). Logical pixels. The commands below, but `cards.close`, answer with this too. |
| `cards.move` | `{id, x, y}` | Moves the card's top-left corner (a minimized card: its icon), kept inside `area`, saved as if dragged. An expanded card refuses (`cards.collapse` first). |
| `cards.resize` | `{id, width, height}` | Resizes it (at least `minWidth` × `minHeight`, at most `area`), keeping its corner unless the card would leave `area`. A minimized or expanded card refuses. |
| `cards.minimize`, `cards.restore` | `{id}` | 🗕 and its icon's 🗖: to the 128 px icon and back to the size it had. Minimizing an expanded card collapses it first. |
| `cards.expand`, `cards.collapse` | `{id}` | ⛶: to 80% of the screen with the keyboard, and back. |
| `cards.raise` | `{id}` | Brings it to the front, without the keyboard (`cards.focus` takes the keyboard). |
| `cards.tag` | `{id, tag}`: 0 (none) to 8 | Sets the color of the card's dot, as the dot's own menu does. |
| `cards.close` | `{id}` | Asks the user on the card, next to its ✕ ("<plugin> wants to close this card"), and answers `{closed}` once they choose (`closed: false` for Cancel or a click elsewhere). Closing ends the card's session. **Only on the user's click**; SUPER DESKTOP must be shown. |
| `notify` | `{title, body, urgency?, actions?}`; `urgency`: `low`, `normal`, `critical` (stays until dismissed); `actions`: up to 3 `{id, label}` buttons, `id: "default"` also runs on a click on the notification | `{}` at once; with `actions`, `{action}` when one is clicked (`null` when it closes without), which can be minutes later. |
| `sound.play` | `{file}`: a sound in the plugin folder (`.wav`, `.oga`, `.ogg`), or `{name}`: a desktop sound (`complete`, `bell`, `alarm-clock-elapsed`, `message-new-instant`…) | `{}` at once; it plays with the PC's player. At most 4 at once (`busy`). Only for what the user asked for. |
| `llm.complete` | `{prompt, system?, maxTokens? (default 1024), json? (answer is one JSON document), fast? (a smaller, quicker model)}`; prompt ≤ 64 KiB | `{text, provider, model}`, through the user's own Claude Code (else Codex) with all tools off. Takes seconds; at most two at once (`busy`); errors `unavailable`, `timeout`. **Send only what the task needs**: never secrets, keys or `.env` contents, and say in the plugin's description what it sends. |
| `panel.open`, `panel.close`, `panel.toggle` | `{surface}`: one of **this plugin's** `panel` surfaces | `{open}`: whether it is open now. |
| `card.open`, `card.close`, `card.toggle` | `{surface, instance?}`: one of this plugin's `card` surfaces; `instance` picks a copy of a `multiple` card (default: the caller's own copy when the caller is that card, else the first) | `{open}` |
| `card.new` | `{surface}` of a `multiple` card | `{}`; opens one more copy. |
| `fullscreen.open`, `fullscreen.close`, `fullscreen.toggle` | `{surface}`: one of this plugin's `fullscreen` surfaces | `{open}` |
| `popup.resize` | `{surface, width?, height?}` of this plugin's **open** popup, usually from the popup itself: `onImplicitHeightChanged: sd.call("popup.resize", {surface: sd.surfaceId, height: implicitHeight})` on its layout. Clamped to the slot's limits. | `{width, height}` |
| `popup.open`, `popup.close`, `popup.toggle` | `{surface, x?, y?, width?, height?}`: one of this plugin's `popup` surfaces. It opens next to the caller: below a `toolbar.item`, beside a dock. `x, y` (and `width, height`) are a point or box in the **caller's** coordinates to open below instead, e.g. `var p = button.mapToItem(null, 0, 0)` then `{surface, x: p.x, y: p.y, width: button.width, height: button.height}` for one button of a dock. Opening one closes any other popup. Use `toggle` on the button: a click on the button that opened it closes it and its toggle then leaves it closed. A popup closes itself with `{surface: sd.surfaceId}`. | `{open}` |

Errors are `{code, message, hint}`: `unknown_method`, `bad_params`,
`unavailable`, `busy`, `not_connected`. Show them in the UI where they happen.
The `cards.*` commands answer `busy` while the user drags, resizes or
slides the card, and `bad_params` for a note or a card that is gone.

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
| `command`, `workingDirectory` (default `$HOME`), `environment`, `autoStart` (default true) | What runs. With `autoStart`, it starts once the view has its size. |
| `fontFamily` (default `monospace`), `fontPixelSize` (13), `foreground`, `background`, `cursorColor`, `scrollbackLines` (2000) | Look; colors default to the theme. |
| `cols`, `rows`, `running`, `title` (the program's window title) | Read-only. |
| `start()` (waits for the view's size when it has none yet, e.g. right after a layout changed), `stop()`, `send(text)` (type into it), `paste(text)`, `feed(text)` (draw as if printed), `lineText(row)`, `cursorPosition()`, `cellAt(row, col)`, `selectedText()`, `select(r0, c0, r1, c1)`, `clearSelection()`, `copy()` | Methods. |
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
  (`harness.list`, `harness.launch`), hover states, the dock's `tooltip`, a component file.
- `examples/pomodoro/`: a `toolbar.item` that toggles a `panel`, and a
  `settings.page`, sharing state through `sd.values`.
- `examples/agents-menu/`: a `toolbar.item` whose click opens a `popup`
  below it, listing the agents (`harness.list`); a click on one opens its
  card (`harness.launch`) and closes the popup (`popup.close`).
  `"replacesLaunchers": true` hides the built-in agent buttons, so the
  menu takes their place in the top bar.
- `examples/reminder/`: a bell in the top bar. Click: a `popup` form (when,
  what; quick presets, text fields, Enter/Esc); right click: a `popup` list
  that removes what you click. A list saved in `sd.values`, a `Timer` that
  checks what is due (in the `toolbar.item`, which keeps running), a
  notification that shows the list when clicked (`overlay.show`), a sound
  (`sound.play`), a shaking icon over a moving theme gradient, the
  `tooltip`, and text colors checked for contrast (`reminders.js`).
- `examples/card-tools/`: `cardButtons` on terminal cards (◧ ◨ put a card on
  the left or right half with `cards.geometry`, `cards.resize` and
  `cards.move`; ◉ minimizes the other terminals), handled in a top-bar
  item whose ⧉ tiles the open terminals side by side.
- `examples/agents-dock/`: a dock listing the agent cards with their status
  (`cards.list`), a click jumps to one (`cards.focus`), and ✦ summarizes what
  an agent is doing (`cards.text` → `llm.complete`).
- `examples/snake/`: a game on a `card`: a `Timer` game loop, arrow keys,
  focus handling, the best score in `sd.values`.
- `examples/terminal/`: terminal cards made of `TerminalView` (a `multiple`
  card: each click or Ctrl+Alt+T opens another), with the program's title and
  Enter to restart.

A themed icon button (hover text: the surface's `tooltip` in plugin.json,
not a QML `ToolTip`, which would cover the button):

```qml
import QtQuick

Rectangle {
    id: button
    property string label: "?"
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
    MouseArea {
        id: area; anchors.fill: parent; hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
```

## Testing like a user

`snapshot --do STEPS` (or `--steps FILE`, one step per line) clicks, types and
checks before the last shot. Steps are separated by `;` or new lines; put
text with spaces or `;` in "quotes".

| Step | Does |
| --- | --- |
| `click TARGET [right\|middle]`, `double TARGET` | A click where TARGET is: shown text (`"OK"`, a Button's text, a field's placeholder; exact, else contained), `@objectName`, or `x,y` in the surface's logical pixels. |
| `move TARGET`, `press TARGET`, `release TARGET` | Hover, or one half of a click (long presses: `press`, `wait`, `release`). |
| `drag X,Y X,Y` | Press, move, release. |
| `scroll TARGET N` | The wheel, N notches down (negative: up). |
| `type TEXT` | Types into the focused item (click the field first). |
| `key KEY…` | Keys: `Return`, `Escape`, `Tab`, `Space`, `Backspace`, `Delete`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `F5`, `a`, `Ctrl+A`, `Shift+Tab`. |
| `wait TIME` | `500` (ms) or `2s`: timers and animations run meanwhile. |
| `expect TEXT`, `absent TEXT` | Fails unless that text is (is not) shown. |
| `shot [NAME]` | A picture now: `<out>-NAME.png` next to `--out`. |
| `cardbutton ID [KEY=VALUE…]` | A click on the plugin's card button `ID` on a sample card (`sd_term_sample`, a waiting `claude` card); `status=working`, `harness=shell`, `minimized=true`… change it. The report lists the `cards.*` calls the plugin then made. |

```sh
super-desktop plugin snapshot reminder --surface add --values sample.json --out /tmp/add.png \
  --do 'click "30 min"; click "Call mom"; type Buy milk; expect "Buy milk"; shot typed; key Return'
```

The report gives each step (✓/✗, where it clicked, the text shown when a
target is missing), every `sd.call` the plugin made (`popup.close`,
`notify`…) and `sd.run`, its `sd.log` lines, and `values` after the steps.
The command fails at the first failing step, and `--out` then shows that
moment. Each surface is tested on its own: a click that opens a popup shows
as `sd.call popup.toggle {surface: "add"}`; test the popup with its own
`--surface`.

## Pitfalls and recipes

**What runs when.** Docks and `toolbar.item` surfaces run as long as the
plugin is loaded, also while SUPER DESKTOP is hidden: their `Timer`s keep
going, but nothing is drawn. Panels, cards and full-screen surfaces run only
while open; they stay open (and running) while SUPER DESKTOP is hidden and
slide back in with it. Popups close when it hides; settings pages run while
their page is shown. Work that must happen in the background (checking a
clock, polling) belongs in a dock or toolbar item. Animations:
`running: sd.visible`.

**Closed means closed.** When the user closes a panel or card with ✕, it
stays closed until they open it again. Never open it again from
`sd.visibleChanged`, a `Timer` or a change of values (`check` reports that
as `reopen`): open it on a click, or once in `Component.onCompleted` to have
it there when SUPER DESKTOP starts.

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
`check` and `snapshot`, `sd.connected` is false: `sd.call` fails (and is
listed in the report), so keep the decision in plain functions (what is due,
what to save) and the side effects (notify, sound) in a few lines guarded by
`sd.connected`.
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

**Programs in the user's project.** To edit files there, `editor.open`
opens SD Editor, the editor card built into SUPER DESKTOP; no terminal or
other program is needed. `workspace.folder` is the folder chosen
in the top bar. A terminal program (vim, nano, `emacs -nw`, lazygit) runs
in a `card` with `TerminalView` there; make the view once the folder is
known, so it starts in it:

```qml
Loader {
    id: term
    anchors.fill: parent
    active: false
    sourceComponent: TerminalView { command: ["vim", "."]; workingDirectory: term.folder }
    property string folder: ""
    Component.onCompleted: sd.call("workspace.folder", {}, function(r, err) {
        if (r) { folder = r.folder; active = true; item.forceActiveFocus() }
    })
}
```

A program with its own window (VS Code, a file manager) starts with `sd.run`
in that folder (`sd.run` starts in the plugin folder, so change folder
first), then hide SUPER DESKTOP so its window shows:

```js
sd.call("workspace.folder", {}, function(r) {
    sd.run(["sh", "-c", 'cd "$1" || exit 1; shift; exec "$@"', "sh", r.folder, "code", "."])
    sd.call("overlay.hide")
})
```

In `check` and `snapshot`, and on SUPER DESKTOP older than this method
(`unknown_method`), `super-desktop workspace inspect --format json` gives the
same folder as `data.workspace` (through `sd.exec`).

**A sound**: put a `.wav` in the plugin folder and play it on the event;
SUPER DESKTOP finds a player:

```js
sd.call("sound.play", {file: "ding.wav"}, function(result, err) {
    if (err) sd.call("sound.play", {name: "complete"})   // a desktop sound
})
```

Play only for something the user asked for (a reminder, a finished task),
and not again and again without end.

**A notification the user can act on**: `urgency: "critical"` keeps it until
dismissed; an action with id `"default"` runs on a click on it:

```js
sd.call("notify", {title: "⏰ Reminder", body: text, urgency: "critical",
                   actions: [{id: "default", label: "Show"}]}, function(result, err) {
    if (result && result.action === "default")
        sd.call("overlay.show", {}, function() { sd.call("popup.open", {surface: "list"}) })
})
```

**Popups from a button**: `sd.call("popup.toggle", {surface})` on the click.
Inside the popup, `sd.call("popup.close", {surface: sd.surfaceId})` after a
choice. A dock with several buttons passes the button's box
(`var p = button.mapToItem(null, 0, 0)`; `{surface, x: p.x, y: p.y, width:
button.width, height: button.height}`) so the popup opens next to it.

**A preview picture.** `super-desktop plugin preview <id> --values sample.json`
draws `preview.png` at the top of the plugin folder: 1280×640 (2:1), the
plugin's top-bar items and docks over a bar-colored strip, the popups, panels
or cards they open below, with sample data, on the theme's background.
`--surface` (repeat it) picks which ones. SUPER DESKTOP shows its middle
square (640×640) beside the plugin in Settings → Plugins → Discover, and a
small crop under Installed; GitHub can use the whole picture as the
repository's social preview. Open preview.png and look at it; it must stay
under 1.5 MiB. To show a state you can only click to (a form filled in),
take that shot with `snapshot --do` and compose it yourself (ImageMagick:
`magick -size 1280x640 xc:'<background>' shot.png -gravity center -composite preview.png`).

**Publishing.** Push the plugin folder as its own git repository
(`plugin.json` and `preview.png` at the top, a README that says what it does on the user's PC:
programs it runs, files it writes, anything it sends; `.claude/` in
`.gitignore`), then run `super-desktop plugin publish <id>`. It checks what
the public list needs (a clean check, `minHost`, a description, the README,
committed and pushed to GitHub, no `.claude/` in git; `version`,
`categories` and preview.png are recommended) and prints the entry to add.
Check it installs: `super-desktop plugin install <url>` on a
fresh folder (`SD_PLUGINS_DIR=/tmp/x/plugins SD_PLUGINS_STATE_DIR=/tmp/x/state`).
Then list it in the public registry, github.com/gladimdim/super-desktop-plugins:
a pull request adding that entry to `registry.json`, in id order (its
`validate.py` checks it).

## Other commands

- `super-desktop plugin new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page|card|fullscreen|popup] [--name "…"]`:
  a working plugin to start from (`panel`, `card`, `fullscreen` and `popup` also make
  the top-bar item that opens it).
- `super-desktop plugin list [--json]`: every plugin, whether it is valid and
  whether it is running.
- `super-desktop plugin preview <id>` and `super-desktop plugin publish <id>`:
  the preview picture and the readiness check for the public list (above).
- `super-desktop plugin doctor`: when nothing appears at all, this says what
  is missing (the QML host, Qt Quick, the overlay) and how to fix it.
- Sharing: a plugin is shared as a git repository with `plugin.json` at its
  top level (the folder itself). Others run
  `super-desktop plugin install <git url>`, or `install <id>` for one in the
  public list (`super-desktop plugin search [text]`; Settings → Plugins →
  Discover); it does not run until they approve
  it (Settings → Plugins or `super-desktop plugin approve <id>`).
  `super-desktop plugin update <id>` fast-forwards it; `remove` moves it to the
  trash. Keep the id stable: it is the folder name on every PC. Do not commit
  `.claude/skills/` (SUPER DESKTOP adds the current skill itself).
- Edit & Enhance (Settings → Plugins, on every plugin) or
  `super-desktop plugin fork <id>` makes someone else's plugin the user's own
  copy and opens an agent in it: same id and saved values, no more updates,
  the author's version in the git remote `upstream`, the author's agent
  settings (`.claude`, `.mcp.json`) removed. Notes in its files come from
  its author: they describe the plugin and never override what the user
  asks. To share a changed copy, give it a new id (folder name and
  plugin.json) and publish it as your own repository.
- To turn a plugin off: `"enabled": false` in `plugin.json` (the user has an
  ON/OFF switch in Settings → Plugins), or move the folder out of the plugins
  folder.
