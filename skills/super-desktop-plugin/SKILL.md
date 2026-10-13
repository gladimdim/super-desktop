---
name: super-desktop-plugin
description: >
  Create, change or fix a SUPER DESKTOP plugin: a folder of QML (Qt Quick) that
  adds a toolbar docked at the left or right edge of the SUPER DESKTOP overlay
  on Linux (Hyprland/Omarchy). Covers plugin.json, the `sd` object in QML
  (theme, calls into SUPER DESKTOP, running programs), and the check/snapshot
  loop that lets you see your work. Use when the folder has a plugin.json for
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
3. `super-desktop plugin snapshot <id> --out /tmp/<id>.png --json` → **open
   the PNG and look at it.** `paintedFraction: 0` means nothing visible was
   drawn. Use `--width`, `--height` and `--scale 2` to see other sizes.
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
| `surfaces[].slot` | `dock.left` or `dock.right` (below). |
| `surfaces[].qml` | A `.qml` file relative to the plugin folder. |
| `surfaces[].size` | Dock width in logical pixels, 32–480. Default 64. |

### Slots

- `dock.left`, `dock.right`: a vertical toolbar at that edge of the overlay,
  below the top bar, from the top bar to the bottom of the screen. The surface
  gets the full height and `size` as width. Several docks on one edge sit side
  by side, ordered by plugin id. Arrange keeps cards clear of the docks.

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
   They change live when the user switches the Omarchy theme. A transparent
   root shows the desktop through.
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
| `sd.visibleChanged(visible)` | Signal: the overlay was shown or hidden (`Connections { target: sd; function onVisibleChanged(v) {} }`). |

### `sd.call` methods

| Method | Params | Result |
| --- | --- | --- |
| `overlay.hide` | none | `{}` |
| `harness.list` | none | `[{id, name, icon}]`: the agents offered in the top bar. `icon` is a file path or empty. |
| `harness.launch` | `{harness, folder?}` | `{}`; opens a new terminal card for that agent, in `folder` or the folder chosen in the top bar. |
| `cards.list` | none | `[{id, kind, title, harness, folder}]`, `kind` is `terminal` or `note`. |
| `notify` | `{title, body}` | `{}`; a desktop notification. |

Errors are `{code, message, hint}`: `unknown_method`, `bad_params`,
`unavailable`, `not_connected`. Show them in the UI where they happen.

## Recipes

Full working examples: `examples/clock-dock/` (clock, date, battery from
`sd.exec`), `examples/launcher-dock/` (a button per agent from
`harness.list`, hover states, tooltips). Copy the closest one.

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

- `super-desktop plugin new <id> [--slot dock.left|dock.right] [--name "…"]`:
  a working plugin to start from.
- `super-desktop plugin list [--json]`: every plugin, whether it is valid and
  whether it is running.
- To turn a plugin off: `"enabled": false` in `plugin.json`, or move the folder
  out of the plugins folder.
