---
name: super-desktop-plugin
description: >
  Create, extend, debug or publish a plugin for SUPER DESKTOP, the Hyprland/Omarchy
  overlay with AI terminal cards. Covers the manifest (super-desktop-plugin.json),
  permissions and sandbox, the JSON-RPC host API, toolbar items, keyboard shortcuts,
  popups/panels, settings pages, card header buttons, replacing card window controls,
  changing card titles, WebAssembly window renderers (card layout and animation),
  adding new AI harnesses, using the user's LLM provider, testing and releasing on
  GitHub. Use when a repository contains super-desktop-plugin.json, or when asked to
  write, port, fix or review a SUPER DESKTOP plugin or extension.
---

# Writing a SUPER DESKTOP plugin

SUPER DESKTOP is a full-screen overlay for Hyprland (Omarchy) with sticky notes
and terminal cards that run AI coding harnesses (Claude Code, Codex, …) in
tmux. A plugin extends it **by describing contributions**; the host draws them,
runs them, and removes all of them when the plugin is turned off.

Follow this skill exactly. The schemas in `schemas/` are the contract; when this
text and a schema disagree, the schema wins.

## 0. Check the host first

```sh
super-desktop plugin describe --json
```

It prints the plugin API version, contribution points, permissions, limits and
the schema paths. If the command is unknown, the installed SUPER DESKTOP has no
plugin support: stop and tell the user to update (⚙ Settings → Updates).
Never guess an API that `describe` does not list: `contributionPoints.supported`,
`methods.implemented` and `commands` say what this build really has. When
`new` or `test` is not in `commands`, copy the closest example instead of
scaffolding, and test with `plugin link`, `plugin run` and `plugin logs`.

## 1. Mental model

- **The host draws, the plugin describes.** No GTK, no HTML, no CSS, no access
  to SUPER DESKTOP's files (`~/.config/super-desktop/*`, Hyprland config). You
  declare items in the manifest and change their state through the API.
- **Two kinds of code.**
  - *Process* (`main`): any language, JSON-RPC 2.0 over stdin/stdout, one
    JSON object per line. For commands, popups, settings, titles, card
    buttons, harness adapters, git, network, LLM calls.
  - *Renderer* (`contributes.renderer`): a WebAssembly module called every
    frame to decide where and how big cards are drawn. Pure computation, no
    I/O, fixed byte layout.
- **Turning a plugin off restores everything.** The host removes your toolbar
  items, buttons, controls, titles, shortcuts, views, renderer and journaled
  config files, kills your process group and animates cards back. You do not
  write undo code; you must simply not create state the host cannot see.
- **The phone and other PCs are unaffected.** Plugins never change what the
  phone app or another PC receives. There is no bridge API. Do not try.

## 2. Workflow

1. **Pick the contribution points** for the goal (table below) and the
   fewest permissions they need.
2. **Scaffold**: `super-desktop plugin new <id> --kind process|renderer|both --lang python|node|rust`,
   or copy the closest example from `examples/`.
3. **Write the manifest** (`references/manifest.md`). Add the `$schema` line.
4. **Validate**: `super-desktop plugin validate . --json`. Fix every error; each
   one has a `hint` and a `docs` pointer into this skill.
5. **Implement** with the SDK (`sdk/python/sd_plugin.py`) or your own JSON-RPC
   loop (`references/host-api.md`); a renderer per `references/renderer-abi.md`.
6. **Test**: `super-desktop plugin test .` with scenarios in `tests/`
   (`references/testing.md`). Renderers also get native unit tests.
7. **Try it for real**: `super-desktop plugin link .`, then
   `super-desktop plugin reload <id>` after each edit, `super-desktop plugin logs <id> --follow`.
   Drive panels without a mouse: `plugin run`, then `plugin views <id> --json` to
   read them and `plugin interact <id> <node> click` to press buttons.
8. **Publish**: tag `v<version>`, GitHub Release, registry PR
   (`references/publishing.md`). Add `AGENTS.md` to the plugin repo.
9. **Self-review** with the `super-desktop-plugin-review` skill before a release.

## 3. Which contribution point

| Goal | Contribution | Permission | Read |
| --- | --- | --- | --- |
| Run an action from a key, a button or the CLI | `commands` | none | manifest.md |
| Global or in-overlay keyboard shortcut | `shortcuts` | `shortcuts.global` / `shortcuts.overlay` | manifest.md |
| Button (with badge) in the top bar | `toolbar` | `ui.toolbar` | manifest.md |
| Hide optional top-bar items | `toolbarHide` | `ui.toolbar` | manifest.md |
| A popup / panel with lists, buttons, text | `views` + `ui.open/patch` | `ui.popup` | ui.md |
| A settings page | `settings` | none | manifest.md |
| Extra button on each terminal card | `cardButtons` | `ui.cardButtons` | manifest.md |
| Replace minimize/expand/close (snap halves, to-edge…) | `cardControls` + `card.*` | `ui.cardControls`, `cards.control` | manifest.md, host-api.md |
| Change or decorate card titles | `titles` + `title.set` | `ui.titles` | host-api.md |
| New layout / animation of cards | `renderer` (WASM) | `layout.renderer` | renderer-abi.md |
| Support a new AI CLI | `harnesses` | `harness.provide` | harnesses.md |
| Ask an LLM | `llm.complete` | `llm` | host-api.md |
| Read cards, folders, prompts | `workspace.cards` | `cards.read` | host-api.md |
| Read / type into a terminal | `terminal.text` / `terminal.send` | `terminal.read` / `terminal.write` | host-api.md |
| Open a harness card with a prompt | `harness.launch` | `harness.launch` | host-api.md |
| Notify the user | `ui.notify` | `ui.notify` | host-api.md |

`cardControls` and `renderer` are **exclusive**: one active plugin each.

## 4. Hard rules

Breaking any of these fails validation, testing or review.

1. **stdout is the protocol.** Never print to it. Log to stderr or `log`.
2. **Never block.** Answer `activate` within 10 s and `deactivate` within 1 s;
   do slow work on threads. Handle incoming messages off the reader loop.
3. **Declare everything up front.** Ids are `<plugin id>.<name>`. Contribution
   ids cannot be created at run time; use `contrib.update` for state.
4. **Least privilege.** Ask only for the permissions you use; keep the sandbox
   tight. `terminal.write` and `"sandbox": "none"` need a reason in the README.
5. **Only the host changes host state.** Never edit SUPER DESKTOP's state,
   `~/.config/hypr/*`, or a harness's config files directly. Use `card.*`,
   `shortcuts`, `harness.writeConfig`. Anything else survives deactivation and
   breaks the "everything returns to normal" guarantee.
6. **Prompts are sacred, titles are display.** Never report terminal output or
   injected turns as a harness `prompt` (`harnesses.md`). `title.set` changes
   only what this PC draws; it is never the prompt.
7. **Act only on the user's request.** Anything that commits, pushes, sends,
   deletes, closes or types into a terminal follows an explicit click or
   command, and shows what it will do first when the effect is large (see the
   Git Flush review step). No destructive defaults (`--force`, `reset --hard`,
   `rm -rf`).
8. **Send the LLM only what the task needs.** Skip secrets (`.env*`, keys,
   tokens), cap sizes, say in the README what is sent.
9. **The toolbar must fit.** At most 4 toolbar items with short labels; they
   scroll with the launchers. Never assume a screen width.
10. **Renderers stay inside the ABI.** No imports beyond `env.sd_log`, fixed
    buffers (room for 128 cards and 16 params), finite numbers, same card
    count out as in, linear work per card. Give the effect a
    `renderer.toggle` command (toolbar item and shortcut) and its tuning as
    `renderer.params` settings; then the plugin needs no process.
11. **Stay compatible.** Never rename the plugin id, setting keys or harness
    ids after publishing. Ignore unknown fields. Check
    `host.describe().methods` for optional features.
12. **Fail visibly and locally.** Show errors on the row or item they concern,
    use `data.hint` from host errors, keep working for the parts that still can.

## 5. Limits (defaults; read real values from `host.describe().limits`)

| Thing | Limit |
| --- | --- |
| JSON-RPC message | 1 MiB |
| `activate` / other host→plugin requests / `deactivate` | 10 s / 2 s / 1 s |
| Toolbar items / card buttons / shortcuts / views / harnesses | 4 / 2 / 8 / 8 / 4 |
| Card controls per set | 1–6 |
| Title text / chip text / chips per side | 200 / 24 / 3 |
| Badge | 4 characters |
| Storage | 1 MiB per plugin |
| `llm.complete` | 64 KiB prompt, 2 concurrent, 120 s |
| Notifications | 6 per minute |
| View | 2000 nodes, 500 ops per patch |
| Renderer | 1 000 000 fuel per frame, 128 cards, 16 params, 4 KiB state, 3 failures in 10 s turn it off |

## 6. Files in this skill

| Path | Use |
| --- | --- |
| `references/manifest.md` | Every manifest field, permission, sandbox and contribution point; validator rules. |
| `references/host-api.md` | Transport, lifecycle, errors, every method and notification, with examples. |
| `references/ui.md` | View nodes, events, patching, UI patterns. |
| `references/renderer-abi.md` | WASM exports and the exact input/output byte layout. |
| `references/harnesses.md` | Adding an AI harness: launch, adapter events, the prompt rule. |
| `references/testing.md` | `describe`, `validate`, `test` scenarios, `link`, `logs`; pre-release checklist. |
| `references/publishing.md` | Repository layout, releases, registries, AGENTS.md for plugin repos. |
| `schemas/manifest.schema.json` | Normative manifest schema (JSON Schema 2020-12). |
| `schemas/host-api.openrpc.json` | Normative API (OpenRPC 1.3): methods, params, results, errors. |
| `schemas/ui.schema.json` | Normative view nodes and patch operations. |
| `schemas/registry.schema.json` | Normative `registry.json` of a plugin registry. |
| `sdk/python/sd_plugin.py` | Python runtime: copy next to `main.py`. |
| `examples/git-flush/` | Process plugin: toolbar badge, global shortcut, panel, settings, LLM, review-then-act. |
| `examples/window-controls/` | Process plugin: `cardControls` with snap-left/right and to-edge icon. |
| `examples/center-magnify/` | Rust WASM renderer with settings (params), a toolbar/shortcut toggle and no process; native tests, renderer scenarios and `build.sh`. |

## 7. Minimal process plugin

`super-desktop-plugin.json`:

```json
{
  "$schema": "https://raw.githubusercontent.com/gladimdim/super-desktop/master/skills/super-desktop-plugin/schemas/manifest.schema.json",
  "manifestVersion": 1,
  "id": "hello",
  "name": "Hello",
  "version": "0.1.0",
  "description": "Says hello from the top bar.",
  "engines": { "superDesktop": ">=1.2.0", "pluginApi": "1" },
  "main": { "command": ["python3", "main.py"] },
  "permissions": ["ui.toolbar", "ui.notify"],
  "contributes": {
    "commands": [{ "id": "hello.say", "title": "Say hello" }],
    "toolbar": [{ "id": "hello.button", "icon": "👋", "tooltip": "Say hello", "command": "hello.say" }]
  }
}
```

`main.py` (with `sd_plugin.py` copied next to it):

```python
from sd_plugin import Plugin

plugin = Plugin()

@plugin.command("hello.say")
def say(context):
    plugin.call("ui.notify", title="Hello", body="from a SUPER DESKTOP plugin")

plugin.run()
```
