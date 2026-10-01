# Host API 1: JSON-RPC between SUPER DESKTOP and a plugin process

Normative definition: `schemas/host-api.openrpc.json`. This file explains it
with examples. Ask a running host what it supports with `host.describe`, or
from a shell with `super-desktop plugin describe --json`.

## Transport

- The host starts `main.command` in the plugin directory (inside the sandbox).
- JSON-RPC 2.0, UTF-8, **one JSON object per line** on stdin (host → plugin)
  and stdout (plugin → host). At most 1 MiB per line.
- **stdout is protocol only.** A stray `print` corrupts the stream. Log to
  stderr (kept in `super-desktop plugin logs <id>`) or call `log`.
- Both sides send requests (with `id`) and notifications (without `id`). Answer
  every request that has an `id`. Ignore fields you do not know: the API only
  grows within version 1.
- Never block your reader loop on a call to the host. Handle each incoming
  message on a worker, or use async I/O. `sdk/python/sd_plugin.py` does this.

```
host → plugin  {"jsonrpc":"2.0","id":1,"method":"activate","params":{"apiVersion":1,"hostVersion":"1.2.0","pluginId":"git-flush","pluginDir":"/home/u/.local/share/super-desktop/plugins/git-flush/current","dataDir":"/home/u/.local/state/super-desktop/plugins/git-flush","settings":{"repositories":["/home/u/src/app"],"pollSeconds":60},"permissions":["ui.toolbar","ui.popup","ui.notify","shortcuts.global","llm"]}}
plugin → host  {"jsonrpc":"2.0","id":1,"result":{}}
plugin → host  {"jsonrpc":"2.0","id":1,"method":"contrib.update","params":{"id":"git-flush.button","badge":"3"}}
host → plugin  {"jsonrpc":"2.0","id":1,"result":{}}
host → plugin  {"jsonrpc":"2.0","method":"command","params":{"command":"git-flush.open","context":{"source":"shortcut"}}}
```

## Lifecycle

1. **Start**: on the first activation event (`onStartup`, `onOverlayShown`,
   `onCommand:…`). Static contributions (toolbar items, buttons, shortcuts)
   are already drawn from the manifest before the process starts.
2. **`activate`** (request, answer within 10 s). Save the parameters. You may
   call host methods once you have received it. Do not do slow work before
   answering; start it on a thread.
3. **Events**: `command`, `view.event`, `view.closed`, `settings.changed`,
   `title.inputs`, `workspace.changed`, `theme.changed`, `overlay.shown`,
   `overlay.hidden`.
4. **`deactivate`** (request, answer within 1 s). Stop timers and child
   processes. **Do not undo anything in the host**: the host removes every
   contribution, title, bind, view and journaled file itself. After the answer
   (or 1 s) the process group gets SIGTERM, then SIGKILL 2 s later.
5. **Crash**: restarted after 1 s, 5 s, 30 s; after 3 crashes in 5 minutes the
   plugin is marked failed and its items are disabled. Do not rely on a
   restart to recover state; keep what matters in `storage`.

## Errors

Host errors look like:

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32001,"message":"permission_denied",
 "data":{"reason":"terminal.send needs the terminal.write permission",
         "hint":"Add \"terminal.write\" to permissions in super-desktop-plugin.json; users will be asked again on update.",
         "docs":"references/manifest.md#permissions"}}}
```

| Code | Name | Typical fix |
| --- | --- | --- |
| -32601 | method_not_found | Check `host.describe().methods`; raise `engines.superDesktop`. |
| -32602 | invalid_params | Compare with the schema; `data.reason` names the field. |
| -32001 | permission_denied | Add the permission to the manifest. |
| -32002 | not_found | The card or view is gone; refresh with `workspace.cards`. |
| -32003 | limit_exceeded | Batch or slow down; see `host.describe().limits`. |
| -32004 | unavailable | Show `data.hint` to the user (e.g. "Choose an AI provider in Settings → Plugins"). |
| -32005 | timeout | Retry once, then report. |
| -32006 | cancelled | The plugin is being turned off; stop. |

Answer host requests you cannot handle with `-32601`; any other failure with
`-32000` and a short message.

## Methods the plugin calls

Each method names its permission. Limits are the defaults; read the real ones from
`host.describe().limits`.

### `host.describe`
`{}` → `{apiVersion, hostVersion, pluginId, permissions, methods, limits, llm}`.
Call it at activation if your behaviour depends on optional features.

### `log`
Notification `{level: debug|info|warn|error, message}`.

### `contrib.update`
`{id, label?, badge?, tooltip?, icon?, enabled?, visible?}` → `{}`. Changes a
contribution declared in the manifest. `badge` is up to 4 characters, `null`
removes it. Ids cannot be invented at run time.

### `settings.get` / `settings.set`
`settings.get {secret?}` → values (secrets only when named).
`settings.set {key, value}` → `{}`. The settings page is drawn by the host
from the manifest; you rarely need `settings.set`.

### `storage.get` / `storage.set`
`{key}` → `{value}`; `{key, value}` → `{}`. JSON values, 1 MiB per plugin,
`null` deletes. Kept on deactivation; removed on uninstall unless the user
keeps data. For larger files use `dataDir`.

### `ui.open` / `ui.patch` / `ui.close`
Permission: `ui.popup`.
`ui.open {view, model, anchor?}` → `{handle}`: shows a declared view with a
node tree (`ui.md`). It shows the overlay if it was hidden. Opening an open
view replaces its content with the new model, raises it and returns the same
handle. `ui.patch {handle, ops}` updates nodes by id; a `set` changes only the
props it names. `ui.close {handle}`.

### `ui.notify`
Permission: `ui.notify`. `{title, body?, urgency?}`; at most 6 per minute.

### `workspace.cards`
Permission: `cards.read`. `{}` → `{screen: {w, h, top}, cards: [CardInfo]}`, where CardInfo is
`{id, session, agent, folder, status, prompt, rect, icon, iconified, expanded, focused, local}`.
`prompt` is the user's vetted prompt (never terminal output, never another
plugin's title). Coordinates are logical pixels; cards live below `top`.

### `card.*`
Permission: `cards.control`. `card.iconify {card, at?}` (`at` sets the icon
spot), `card.restore {card}`, `card.expand {card}`, `card.collapse {card}`,
`card.focus {card}` (raises it, opens an icon, focuses its terminal),
`card.setRect {card, rect}` → `{rect}`, `card.close {card}` → `{closed}`.
They run the same path as the card's own buttons and gestures, so the result
is saved like a user's and stays after the plugin is off. `setRect` is
clamped like a drag (the card stays on screen below the bar) and answers
with the rectangle applied. An expanded card refuses `iconify`, `restore`
and `setRect` until `card.collapse`. `card.close` asks the person in a small
dialog on the card ("<plugin> wants to close this terminal"); the request
waits for the answer (up to 2 minutes) and `closed` says what they chose.

### `terminal.text` / `terminal.send`
Permissions: `terminal.read` / `terminal.write`. `terminal.text {card, lines?}` → `{text}`: up to 200 visible lines.
`terminal.send {card, text, enter?}` → `{}`: pastes into the session. Only
with an explicit user action; never type into a harness on a timer.

### `harness.launch`
Permission: `harness.launch`. `{agent, folder, prompt?}` → `{card}`. `agent` is
a built-in key (`claude`, `codex`, `opencode`, `gemini`, …) or a custom
launcher key; `folder` must exist. With `prompt`, the harness starts working on
it at once: it is passed one time on the harness's command line (`claude` and
`codex` take it as their prompt, `opencode` as `--prompt`, `gemini` as `-i`;
other harnesses answer `unavailable`). The prompt is never saved with the card,
so a restart does not run it again, and it is not shown to other devices. The
card is a normal, visible card: the person sees the agent work and answers
its permission questions there.

### `harness.writeConfig`
Permission: `harness.provide`. `{path, content}` → `{}`. Writes a config file your harness needs, under
`$HOME` but outside `~/.config/super-desktop` and `~/.config/hypr`. The host
records the previous content and restores it (or deletes the file) on
deactivation. Never write such files yourself.

### `llm.complete`
Permission: `llm`. `{prompt, system?, maxTokens?, json?, tier?}` → `{text, provider, model}`.
The user picks the provider once in Settings → Plugins → AI provider: a
signed-in harness CLI (Claude Code, Codex, OpenCode, Gemini) run without
tools, or an API key. You never see credentials. Prompt up to 64 KiB, 2 calls
at a time, 120 s timeout. `json: true` asks for a JSON document in `text`
(still parse defensively). `tier: "fast"` picks a smaller, quicker model.
`-32004 unavailable` means no provider is set up: show `data.hint`.

Rules: send only what the task needs; leave out secrets (`.env`, keys,
tokens); tell the user in your README what is sent.

### `title.set` / `title.clear`
Permission: `ui.titles`. `title.set {card, text?, chipsBefore?, chipsAfter?}` → `{}`.
`text` replaces the title this PC draws (one line, ≤ 200 characters, `null`
restores the built-in title). Chips: `{text ≤ 24, tone?, tooltip?}`, up to 3
per side. The tooltip always shows the real prompt. Titles are display-only:
they never become the prompt, never reach the phone or other PCs, and vanish
when the plugin is turned off. With several title plugins, chips combine and
the first plugin (by plugin id) that sets text wins. This build titles this
PC's own cards (`local: true`).

## Notifications the host sends

| Method | Params | When |
| --- | --- | --- |
| `command` | `{command, context: {source, card?, args?}}` | A declared command ran. `source`: shortcut, toolbar, cardButton, cardControl, cli, view. |
| `view.event` | `{handle, view, node, event, value?}` | `click` (button), `change` (checkbox, toggle, entry after 300 ms idle, textArea, select), `submit` (Enter in entry). |
| `view.opened` | `{handle, view, anchor?}` | The user clicked a toolbar item that names a view: the host opened it with a placeholder (`root` column with a spinner). Fill it with `ui.patch` (`replace` `root`). |
| `view.closed` | `{handle}` | The view closed (user, Hide, or `ui.close`). Drop the handle. |
| `settings.changed` | `{values}` | The user changed settings. |
| `title.inputs` | `{cards: [{id, agent, folder, status, prefix, prompt, local}]}` | After activate and whenever a card's inputs change; answer with `title.set`. |
| `workspace.changed` | `{}` | Cards changed; ≤ 4/s; not while hidden. |
| `theme.changed` | `{theme}` | The Omarchy theme changed. |
| `overlay.shown`, `overlay.hidden` | `{}` | The overlay was shown or hidden. Pause polling while hidden. |
