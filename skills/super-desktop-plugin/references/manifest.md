# Manifest: `super-desktop-plugin.json`

The manifest sits at the root of the plugin repository. The host reads it
without starting the plugin, to show the install consent, draw static
contributions and decide when to start the process.

The schema is normative: `schemas/manifest.schema.json`. Put
`"$schema": "https://raw.githubusercontent.com/gladimdim/super-desktop/master/skills/super-desktop-plugin/schemas/manifest.schema.json"`
at the top so editors validate it, and run `super-desktop plugin validate .`
before every commit. The validator also checks the rules that a JSON schema
cannot express; they are listed at the end of this file.

## Top-level fields

| Field | Required | Notes |
| --- | --- | --- |
| `manifestVersion` | yes | `1`. |
| `id` | yes | `^[a-z][a-z0-9-]{1,38}[a-z0-9]$`. Never change it after publishing: settings, storage and the user's shortcuts are keyed by it. |
| `name` | yes | Up to 48 characters, shown in Settings → Plugins. |
| `version` | yes | Semver. The release tag is `v<version>`. |
| `description` | yes | Up to 200 characters, one sentence of what the user gets. |
| `author`, `license`, `repository` | no | `repository` is the `https://github.com/owner/repo` URL. |
| `engines` | yes | `{"superDesktop": ">=1.2.0", "pluginApi": "1"}`. Use the lowest SUPER DESKTOP version that has every method and contribution you use. |
| `main` | if there is process code | `{"command": ["python3", "main.py"], "env": {...}}`. `command[0]` is an interpreter on PATH (`python3`, `node`, `bun`, `deno`, `bash`, `sh`) or a path inside the plugin. Working directory: the plugin directory. |
| `assets` | no | Files downloaded at install: `{"bin/tool": {"url": "https://…", "sha256": "…", "arch": "x86_64"}}`. |
| `activation` | no | `onStartup`, `onOverlayShown`, `onCommand:<command id>`. Without it the process starts on the first command. |
| `permissions` | yes | Every permission the plugin uses; the user sees them at install. Ask for the fewest. |
| `sandbox` | no | OS sandbox for the process; see below. Default: `{"network": false}` with no extra paths. |
| `contributes` | yes | What the plugin adds; see below. May be `{}` only for a pure harness or renderer plugin that declares them there. |

## Permissions

| Permission | Allows | Consent wording |
| --- | --- | --- |
| `ui.toolbar` | `toolbar`, `toolbarHide` | adds buttons to the top bar |
| `ui.cardButtons` | `cardButtons` | adds buttons to terminal cards |
| `ui.cardControls` | `cardControls` | replaces the minimize/expand/close buttons |
| `ui.titles` | `titles`, `title.set`, `title.clear` | changes card titles shown on this PC |
| `ui.popup` | `views`, `ui.open/patch/close` | opens its own panels |
| `ui.notify` | `ui.notify` | shows desktop notifications |
| `shortcuts.global` | `shortcuts` with `scope: global` | adds keyboard shortcuts that work everywhere |
| `shortcuts.overlay` | `shortcuts` with `scope: overlay` | adds keyboard shortcuts inside SUPER DESKTOP |
| `cards.read` | `workspace.cards` | sees your cards, their folders and prompts |
| `cards.control` | `card.*` | moves, resizes, minimizes and closes cards |
| `terminal.read` | `terminal.text` | reads what your terminals show |
| `terminal.write` | `terminal.send` | **types into your terminals** |
| `harness.provide` | `harnesses`, `harness.writeConfig` | adds AI harnesses and their config files |
| `harness.launch` | `harness.launch` | starts harnesses in new cards |
| `layout.renderer` | `renderer` | replaces how cards are laid out |
| `llm` | `llm.complete` | uses your AI provider |

## Sandbox

When `bubblewrap` is installed, the process runs inside it:

- the plugin directory and its data directory are always available
  (read-only and read-write);
- `read` and `write` list more paths: absolute, `~/…`, `${settings.<key>}`
  (a `path` or `paths` setting; it follows the user's choice) and
  `${env:NAME}` (for example the SSH agent socket);
- `network: true` allows network access;
- `env` lists session variables to pass through (HOME, PATH and LANG always
  are).

`"sandbox": "none"` runs unsandboxed and is shown in red at install. Use it only
when the plugin's purpose needs the whole home directory, and say why in the
README. Git work needs `~/.gitconfig`, `~/.config/git`, `~/.ssh` (read),
`${env:SSH_AUTH_SOCK}` and `network: true` to push; see the Git Flush example.

## Contribution points

Ids are `<plugin id>.<name>` and unique within the manifest. Icons are one
emoji or a relative `.svg`/`.png` (at most 256 KiB).

| Key | Kind | Shape | Notes |
| --- | --- | --- | --- |
| `commands` | additive | `[{id, title, icon?, action?}]` | Everything clickable runs a command; also `super-desktop plugin run <plugin> <command> [json]`. A command with an `action` is carried out by the host; see [Commands](#commands). |
| `shortcuts` | additive | `[{id, command, default, scope}]` | `default` in Hyprland spelling: `"SUPER + SHIFT + G"`, or `"F7"`. `global` becomes a Hyprland bind in a block of `bindings.lua` the host owns (it needs Hyprland's config folder); it never unbinds anything, so a combination Hyprland already uses (the show/hide key included) is refused and the user is told. `overlay` works while SUPER DESKTOP has focus; when a terminal has focus only combinations with SUPER reach the plugin, so CTRL/ALT keys stay the shell's. Two plugins asking for one combination: the first keeps it. The user can turn off or rebind each shortcut in Settings → Plugins. At most 8. |
| `toolbar` | additive | `[{id, icon, tooltip, label?, command \| view}]` | Placed after the harness launchers, inside the scrolling part of the bar. Labels hide on narrow screens. At most 4. Update label/badge/visibility at run time with `contrib.update`. |
| `toolbarHide` | additive | `["brand", "shortcutHint", "newNote", "usage", "launcher:<key>"]` | Arrange, Settings, Hide and the PC selector cannot be hidden. |
| `cardButtons` | additive | `[{id, icon, tooltip, command, showInIcon?, when?}]` | Local cards only. `when: {agents, status, iconified}` filters cards. At most 2. The command's context carries the card. |
| `cardControls` | exclusive | `{id, controls, iconControls?}` | Replaces the window buttons on local cards (`iconControls`: on the icon form; without it the icon keeps restore and close). Entries are `builtin:iconify`, `builtin:restore`, `builtin:expand`, `builtin:close` (each exactly like the card's own button) or `{id, icon, tooltip, command}`; 1–6 each. A right-click on the card's header or icon bar always offers the built-in actions. Turning on a second plugin with card controls is refused until the first is off. |
| `titles` | additive | `true` | The plugin sets titles with `title.set`; needs `activation: ["onStartup"]`. |
| `renderer` | exclusive | `{id, wasm, params?}` | A WASM module; see `renderer-abi.md`. `params`: up to 16 keys of `number` or `bool` settings the renderer gets every frame ([Params](renderer-abi.md#params)). |
| `harnesses` | additive | `[{…}]` | See `harnesses.md`. At most 4. |
| `settings` | additive | `[{key, type, title, description?, default?, min?, max?, values?, kind?}]` | Types: `string`, `secret`, `bool`, `number`, `enum` (needs `values`), `path`/`paths` (needs `kind`: `file` or `directory`), `color`. The host draws and stores the page; read values with `settings.get`, get told of changes by `settings.changed`. |
| `views` | additive | `[{id, title, kind, width?, height?}]` | `panel` (movable card) or `popover` (anchored to a toolbar item or card button). Filled with `ui.open`. At most 8. |

**Exclusive** means one active plugin at a time; activating a second asks the
user which to keep.

## Commands

A command without `action` is sent to the plugin's process as a `command`
notification. A command with `action` is carried out by the host and needs
no process:

| `action` | Needs | Does |
| --- | --- | --- |
| `renderer.toggle` | `contributes.renderer` | Turns this plugin's renderer off (cards glide back to the saved layout) or on again. The plugin stays on. The choice is remembered across restarts and reloads. A toolbar item that runs it is drawn pressed while the renderer draws, and Settings → Plugins says when it is off. |

Toolbar items, shortcuts and `plugin run` reach it like any command. A
renderer plugin with a toggle, a toolbar item, a shortcut and settings that
are all `renderer.params` has no process at all; see `examples/center-magnify`.

## Rules the validator adds to the schema

1. Every contribution id starts with `<id>.`; ids are unique.
2. Every `command` referenced by a shortcut, toolbar item, card button or
   control is declared in `commands`; every `view` is declared in `views`.
3. Every contribution and host method used has its permission, and no
   permission is listed that nothing uses (warning).
4. `titles: true` requires `onStartup`. `main` is required when there is a
   command without `action`, a view, `titles`, or a setting that is not in
   `renderer.params`.
5. Every relative path exists, stays inside the repository after resolving
   symlinks, and is not a directory. Icons ≤ 256 KiB; `renderer.wasm` ≤ 4 MiB
   and passes the ABI checks in `renderer-abi.md`.
6. Settings keys are unique; defaults match their type and range.
7. `engines.superDesktop` is a valid range that includes at least one
   released version.
8. A command `action` is a known one and has what it needs (`renderer.toggle`:
   a renderer). Each `renderer.params` entry names a `number` or `bool`
   setting, once; at most 16.
