---
name: super-desktop-plugin
description: >
  Create, change or fix a SUPER DESKTOP plugin: a folder of QML (Qt Quick) that
  adds docks at the screen edges, items in the top bar, floating panels and
  Settings sections to the SUPER DESKTOP overlay on Linux (Hyprland/Omarchy).
  Covers plugin.json, the `sd` object in QML (theme, saved values, calls into
  SUPER DESKTOP, running programs), and the check/snapshot loop that lets you
  see your work. Use when the folder has a plugin.json for
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

### Slots

| Slot | What it is | `size` | `height` |
| --- | --- | --- | --- |
| `dock.left`, `dock.right` | A vertical toolbar at that edge, from below the top bar to the bottom of the screen. Several docks on one edge sit side by side, ordered by plugin id. Arrange keeps cards clear of them. | 32–480, default 64 | the screen's |
| `toolbar.item` | An item in the top bar, after the agent launchers. The bar's height (about 26–50 px, it follows the user's top-bar size). Keep it short: the launcher row scrolls when full. | 24–240, default 96 | the bar's |
| `panel` | A floating card the user can move, resize and close (✕). Not shown until opened with `sd.call("panel.open"/"panel.toggle", {surface})`, usually from a `toolbar.item` or dock button, or from Settings → Plugins. Runs only while open. | 200–1600, default 480 | 120–1200, default 360 |
| `settings.page` | A section under the plugin in Settings → Plugins, as wide as the page. Runs only while that page is shown. Use it for options, saved with `sd.setValue`. | ignored | 60–1200, default 240 |

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
   follows a user action.
7. Available modules: everything in Qt 6 (`QtQuick`, `QtQuick.Controls`,
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
| `sd.visibleChanged(visible)` | Signal: the overlay was shown or hidden (`Connections { target: sd; function onVisibleChanged(v) {} }`). |

### `sd.call` methods

| Method | Params | Result |
| --- | --- | --- |
| `overlay.hide` | none | `{}` |
| `harness.list` | none | `[{id, name, icon}]`: the agents offered in the top bar. `icon` is a file path or empty. |
| `harness.launch` | `{harness, folder?}` | `{}`; opens a new terminal card for that agent, in `folder` or the folder chosen in the top bar. |
| `cards.list` | none | `[{id, kind, title, harness, folder}]`, `kind` is `terminal` or `note`. |
| `notify` | `{title, body}` | `{}`; a desktop notification. |
| `panel.open`, `panel.close`, `panel.toggle` | `{surface}`: one of **this plugin's** `panel` surfaces | `{open}`: whether it is open now. |

Errors are `{code, message, hint}`: `unknown_method`, `bad_params`,
`unavailable`, `not_connected`. Show them in the UI where they happen.

## Recipes

Full working examples, copy the closest one:

- `examples/clock-dock/`: a dock with clock, date and battery (`sd.exec`).
- `examples/launcher-dock/`: a dock with a button per agent
  (`harness.list`, `harness.launch`), hover states, tooltips, a component file.
- `examples/pomodoro/`: a `toolbar.item` that toggles a `panel`, and a
  `settings.page`, sharing state through `sd.values`.

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

- `super-desktop plugin new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page] [--name "…"]`:
  a working plugin to start from (`panel` makes a top-bar item that opens a
  panel).
- `super-desktop plugin list [--json]`: every plugin, whether it is valid and
  whether it is running.
- To turn a plugin off: `"enabled": false` in `plugin.json` (the user has an
  ON/OFF switch in Settings → Plugins), or move the folder out of the plugins
  folder.
