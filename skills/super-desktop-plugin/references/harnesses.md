# Adding a harness

A harness is an AI coding CLI that runs in a terminal card (Claude Code,
Codex, OpenCode, …). A plugin can add one SUPER DESKTOP does not ship. It gets
a launcher button in the top bar, its logo on cards, status (working, waiting,
idle…), the user's prompt as the card title, resume, and completion alerts if
the harness can report them.

Needs the `harness.provide` permission.

## Declaration

```json
"harnesses": [{
  "id": "acme",
  "name": "Acme Agent",
  "glyph": "🦊",
  "logo": "icons/acme.svg",
  "binaries": ["acme", "~/.local/bin/acme"],
  "launch": { "args": ["--tui"] },
  "resume": ["--resume", "${session}"],
  "adapter": { "kind": "generic-report", "setup": ["--hooks", "${plugin}/hooks/acme-hooks.json"] },
  "status": { "work": ["^\\s*[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] ", "Thinking…"], "shellFallback": true },
  "prompts": { "injectedPrefixes": ["<acme-system>"], "placeholderTitles": ["New session"] },
  "completion": true
}]
```

- The harness key is `p.<plugin id>.<id>`, for example `p.acme-tools.acme`.
  It is stored in the user's saved cards, so do not rename it.
- `binaries`: the first found is launched; if none is found the launcher is
  shown as unavailable with the names it looked for.
- `glyph` is used where the logo is not drawn. `logo` is an SVG or PNG in the
  plugin.
- `status.work`: regular expressions (RE2, ≤ 200 characters, no
  backreferences) matched against the last screen lines; a match means
  *working*. With `shellFallback`, an idle shell prompt in the pane means the
  harness exited.

## Adapters: how the harness reports

`"kind": "none"`: no reporting. Status comes from `status.work` and the
shell; the title comes from what the user typed into the card.

`"kind": "generic-report"`: the harness (through a hook, extension or plugin
of its own, which you ship) reports events to SUPER DESKTOP. The host launches
it wrapped so these environment variables are set:

| Variable | Meaning |
| --- | --- |
| `SD_HARNESS_EXE` | The SUPER DESKTOP event reporter to run. |
| `SD_HARNESS_FILE` | This card's metadata file (do not write it yourself). |
| `SD_HARNESS_AGENT` | The harness key, `p.<plugin>.<id>`. |
| `SD_HARNESS_PID` | The harness process id. |

`adapter.setup` arguments are added to the launch so the harness loads your
hook; `${plugin}` is the plugin directory. If the harness can only be
configured through a file, write it with `harness.writeConfig` so it is undone
on deactivation.

Each event is one run of the reporter with a JSON object on stdin (≤ 64 KiB):

```sh
printf '%s' '{"emitter": 4242, "session": "abc123", "prompt": "fix the login form", "status": "working"}' \
  | "$SD_HARNESS_EXE" harness-event "$SD_HARNESS_AGENT"
```

| Field | Meaning |
| --- | --- |
| `emitter` | Required, non-zero: the PID of the process sending the event. |
| `session` | The harness's own session id (used for resume). A new session clears title, prompt and model. |
| `prompt` | The text the user just submitted. Send it from the harness's "user submitted a prompt" hook only. |
| `title` | The harness's own session name, if it has one. |
| `model` | Model name for display. |
| `status` | `working`, `completed`, `idle`, `waiting` (needs the user: a permission prompt or question), `error`, `unknown`. |
| `completionTurn` | Increment once per finished turn to raise a completion alert (with `completion: true`). |
| `completionSupported` | `true` once the hook is known to work. |

Keep reporters fast (SUPER DESKTOP's own reporters give up after 2 s),
truncate strings to 1000 characters, and ignore failures: the
harness must never wait on SUPER DESKTOP.

## The prompt rule

A card's title and stored prompt must only ever be a prompt **the user
submitted** to that harness. Harnesses inject turns of their own (system
reminders, tool results, auto-continuations, slash-command echoes). Rules:

- Report `prompt` only from the hook that fires when the user submits text.
- If the harness marks injected turns, do not report them as `prompt`.
- List every prefix the harness uses for injected turns in
  `prompts.injectedPrefixes`; the host drops prompts that start with one.
- List the harness's placeholder session names (for example "New session")
  in `prompts.placeholderTitles`.
- Never derive a prompt from screen text or assistant output.

The host filters every reported prompt again; a plugin cannot bypass it. If
you want a different *displayed* title, that is the separate `titles`
contribution, which never touches the stored prompt.

## What the phone and other PCs see

Nothing new. A card running a plugin harness appears to the phone app and to
other PCs exactly as a plain terminal card does, and they cannot launch plugin
harnesses. Do not try to reach the bridge; there is no API for it.

## Checklist

- [ ] Launcher shows when a binary exists and is disabled with a reason when not.
- [ ] Status moves working → waiting/idle → completed in a real session.
- [ ] The card title is the prompt the user typed, never an injected turn
      (try the harness's own slash commands and background tasks).
- [ ] Resume reopens the same session after a SUPER DESKTOP restart.
- [ ] Turning the plugin off leaves running cards open as plain terminals and
      restores any file written with `harness.writeConfig`.
