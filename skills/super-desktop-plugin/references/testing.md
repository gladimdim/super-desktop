# Testing a plugin

Never test against the user's desktop by injecting keys or taking
screenshots. Use the host's tools; they run the plugin against a private,
invisible host.

## Commands

| Command | What it does |
| --- | --- |
| `super-desktop plugin describe --json` | API version, contribution points, permissions, limits and the schema paths of the installed SUPER DESKTOP. If the command is unknown, that build has no plugin support. |
| `super-desktop plugin validate <dir> --json` | Schema + extra rules (`manifest.md`). Output: `{"ok": bool, "errors": [{path, message, hint, docs}], "warnings": [...]}`. Fix every error before anything else. |
| `super-desktop plugin new <id> --kind process\|renderer\|both --lang python\|node\|rust` | A starter repository with manifest, entry file, SDK, tests, AGENTS.md and the skill. |
| `super-desktop plugin test <dir> [scenario]` | Starts a headless host with a fake workspace, activates the plugin, plays the scenarios in `tests/*.json`, then deactivates it and checks that nothing is left behind. |
| `super-desktop plugin link <dir>` | Installs the folder in place for real use (asks for consent once). |
| `super-desktop plugin reload <id>` | Deactivate + activate after an edit. |
| `super-desktop plugin logs <id> [--follow]` | stderr, `log` calls, host errors with hints. |
| `super-desktop plugin run <id> <command> [json]` | Runs a command as if clicked; `json` arrives as `context.args`. |

## Scenarios

`tests/<name>.json` drives the headless host:

```json
{
  "settings": { "repositories": ["${fixture}/repo-a"] },
  "workspace": {
    "screen": { "w": 1920, "h": 1080, "top": 46 },
    "cards": [{ "id": "c1", "agent": "claude", "folder": "/tmp/x", "status": "idle",
                "prompt": "fix the login form", "rect": { "x": 300, "y": 200, "w": 640, "h": 480 } }]
  },
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
- `llm.reply` (or `llm.replies: [...]`) answers `llm.complete` without a real
  provider; `llm.unavailable: true` makes it fail with `-32004`.
- Steps: `run`, `click`, `change` (`{view, node, value}`), `event` (any host
  notification, e.g. `{"event": "title.inputs", "params": {...}}`),
  `expect` (`view/node/props`, `call/count/params`, `contrib/badge/label`,
  `title/card/text`, `card/rect`, `notify/title`), `wait` (ms, ≤ 5000).
- After the last step, the runner deactivates the plugin and fails if any
  process, view, title, contribution, bind or journaled file is left.

## Renderers

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
