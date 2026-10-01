# Testing a plugin

Never test against the user's desktop by injecting keys or taking
screenshots. Use the host's tools; they run the plugin against a private,
invisible host.

## Commands

| Command | What it does |
| --- | --- |
| `super-desktop plugin describe --json` | API version, contribution points, permissions, limits and the schema paths of the installed SUPER DESKTOP. If the command is unknown, that build has no plugin support. |
| `super-desktop plugin validate <dir> --json` | Schema + extra rules (`manifest.md`). Output: `{"ok": bool, "errors": [{path, message, hint, docs}], "warnings": [...]}`. Fix every error before anything else. |
| `super-desktop plugin new <id> --kind process\|renderer\|both --lang python\|node\|rust` | A starter repository with manifest, entry file, SDK, tests, AGENTS.md and the skill. Only when `describe` lists `new`. |
| `super-desktop plugin test <dir> [scenario]` | Starts a headless host with a fake workspace, activates the plugin, plays the scenarios in `tests/*.json`, then deactivates it and checks that nothing is left behind. Only when `describe` lists `test`. |
| `super-desktop plugin views <id> [--json]` | Every open view of the plugin and each node's state: `text`, `label`, `value`, `visible`, `enabled`. Read it instead of guessing what the panel shows. |
| `super-desktop plugin interact <id> <node> <event> [value] [--view=<view>]` | Operates a node's real widget as a person would: `click` a button, `change` a checkbox/toggle (`true`), entry/textArea (`"text"`) or select (`"value"`), `submit` an entry. Hidden or disabled nodes refuse, as they would for a person. Text changes reach the plugin after 300 ms. |
| `super-desktop plugin cards [--json]` | This PC's cards and what plugins did to them: the saved `rect`, where a renderer draws it (`drawn`, null for the built-in layout), the drawn and the published title, chips, plugin buttons, the controls shown (header and icon). |
| `super-desktop plugin press <card> <control>` | Presses a card's plugin button or control by its contribution id, or a built-in one (`builtin:iconify`, `builtin:restore`, `builtin:expand`, `builtin:close`), through the real widget. A control not shown in the card's current form (header or icon) refuses. |
| `super-desktop plugin link <dir>` | Installs the folder in place for real use (asks for consent once). |
| `super-desktop plugin reload <id>` | Deactivate + activate after an edit. |
| `super-desktop plugin logs <id> [--follow]` | stderr, `log` calls, host errors with hints. |
| `super-desktop plugin run <id> <command> [json]` | Runs a command as if clicked; `json` arrives as `context.args`. |

## Driving a panel from a shell

This is how an agent tests a plugin with a real SUPER DESKTOP (after `link` and
`activate`), without clicking anything:

```sh
super-desktop plugin run git-flush git-flush.open          # as the shortcut would
super-desktop plugin views git-flush --json                 # what the panel shows
super-desktop plugin interact git-flush flush click         # press "Flush all"
super-desktop plugin views git-flush --json | jq '.[0].nodes.commit.visible'
super-desktop plugin interact git-flush msg:0 change '"Fix the login form"'
super-desktop plugin logs git-flush                         # host errors come with hints
```

## Scenarios

`tests/<name>.json` drives the headless host:

```json
{
  "settings": { "repositories": ["${fixture}/repo-a"] },
  "llm": { "reply": "{\"subject\": \"Fix login form validation\", \"body\": \"\"}" },
  "steps": [
    { "run": "git-flush.open" },
    { "expect": { "view": "git-flush.repos", "node": "where:0", "props": { "text": "main → origin/main" } } },
    { "click": { "view": "git-flush.repos", "node": "flush" } },
    { "expect": { "call": "llm.complete", "count": 1 } },
    { "expect": { "contrib": "git-flush.button", "badge": "1" } }
  ]
}
```

- `${fixture}` is `tests/fixtures/` copied to a temporary directory.
- `setup` (optional) is a shell command run in that copy before the plugin
  starts, with `$FIXTURE` set to it: create git repositories, files, and so
  on. A failing setup fails the scenario.
- `settings` are saved (and checked against the manifest) before the plugin starts.
- `llm.reply` (or `llm.replies: [...]`, one per call, the last repeating)
  answers `llm.complete` without a real provider; `llm.unavailable: true`
  makes it fail with `-32004`.
- Steps, in order:
  - `{"run": "<command>", "args": {...}}` runs a command (`context.source` is `cli`);
  - `{"click": {"view", "node"}}`, `{"change": {"view", "node", "value"}}`,
    `{"submit": {"view", "node", "value"?}}` act on a node as a person would:
    a hidden or disabled node (or one inside a hidden parent) fails the step;
  - `{"event": "<notification>", "params": {...}}` sends any host notification,
    e.g. `title.inputs`;
  - `{"wait": ms}` (at most 5000);
  - `{"expect": …}` waits up to 5 s for one of:
    `{"view", "node"?, "props"?, "propsContain"?}` (the node has these props;
    `propsContain` values are substrings of its text props),
    `{"call": "<method>", "count"?, "params"?, "paramsContain"?}` (the plugin
    called the host so; `llm.complete` counts scenario answers),
    `{"contrib": "<id>", "badge"?, "label"?, …}` (the last `contrib.update`
    values), `{"notify": {"title"?, "body"?, "urgency"?}}`.
- The headless host simulates `contrib.update`, `ui.open/patch/close` and
  `ui.notify` (recorded, never shown). `harness.launch` is recorded and
  answers a made-up card id (no harness starts), and `workspace.cards` answers
  an empty workspace. Other desktop methods answer `unavailable` there; test those on a real desktop with `plugin link`,
  `plugin views` and `plugin interact`.
- After the last step, the plugin is turned off and the run fails if any of
  its processes is still running. Settings, data and the log of a test run
  live in a temporary directory, never the user's.

## Renderers

`plugin validate` loads the `.wasm` with the same checks the desktop makes
(imports, exports, ABI version, buffers). On a real desktop, `plugin cards`
shows where each card is drawn.

Test the pure `present` function natively (`cargo test`) with frames built in
the test, like `examples/center-magnify/src/lib.rs`: centre, edges, drop in
an edge band, many cards, NaN/garbage input, convergence over frames. Then
`super-desktop plugin test` loads the `.wasm`, checks the exports and the
budget with 128 cards, and replays a drag.

## Before publishing

- [ ] `validate` has no errors and no warnings you cannot explain.
- [ ] `test` passes, including the deactivation check.
- [ ] Manual run with `plugin link`: every button, shortcut and view works
      with the overlay shown and hidden, on a narrow and a wide screen.
- [ ] Turn it off and on twice; the desktop looks exactly as before each time.
- [ ] `plugin logs` shows no errors during normal use.
