# Harness integrations

SUPER DESKTOP prefers attributable native session metadata over terminal-output
heuristics. A named conversation is the card title when available; the submitted
prompt remains a separate field and supplies the title when no native name is
available. The Linux card, remote PC card, and bridge use the same resolver.

This is a capability inventory, not a claim that every release of every CLI is
100% compatible. A supported adapter still needs lifecycle testing against the
installed CLI version and its permission/retry configuration.

## Coverage

| Harness | Title and identity | Activity | Activation / validation |
| --- | --- | --- | --- |
| Codex | Own process's open CLI rollout and exact thread ID in its session index; generated/renamed name; own user messages | Explicit working and completed events; a failed turn (`task_complete` carrying an `error` object such as `usage_limit_exceeded`, or an `error`/`task_failed` event) is ERROR until the next turn; interrupts and ambiguous/partial metadata are UNKNOWN, not guessed idle | Existing rollout adapter plus exact-name tests, including another conversation and a child agent |
| Claude Code | Scoped hooks identify the native session; submitted prompts; native names and model switches | Session start/stop, prompt/tool activity, permission wait, API error, compaction; prompt-scoped final Stop supports completion alerts; child hooks cannot overwrite parent state | Installed CLI accepts hooks and emits SessionStart in an isolated no-prompt run; lifecycle reducer tests |
| OpenCode | Explicit TUI selection or submitted root prompt owns the card; background creation/update events and stale selection lookups cannot claim it; the auto-generated `New session - <timestamp>` / `Child session - …` placeholder is no title (card falls back to the submitted prompt) | IDLE at launch (the in-process server has no turn in flight); a selected session reports its own server's `session.status` (idle unless listed busy/retry, lookup failures stay UNKNOWN, newer events win); native session status (busy/retry/idle), permissions/questions, error; final successful own-prompt response produces FINISHED and completion alerts; deletion clears metadata | Installed-server smoke verifies selection, rename and switching; child-session/race contract tests; local-provider runtime completion/error/cancellation smoke |
| Pi | Per-launch extension reads session ID/name/model and submitted prompt; session switches clear old data | Agent start, permission UI, error/abort and final settlement; successful final text response produces FINISHED and completion alerts | Installed offline RPC smoke uses a local fixture provider to verify success, provider failure, recovery, rename and session switching |
| OpenClaw TUI | Dedicated `sd_term_*` key plus native session ID; exact session-store label/display name, refreshed while idle; model retained across hooks. Without the gateway plugin: tracked typed prompt | Working/idle/error and native exec-approval waits; stale pre-reset/run events ignored; stopped-gateway observations expire to UNKNOWN. Without the gateway plugin: screen heuristic | 2026.9.5 production plugin installation and real isolated gateway reset/SessionStart verified; approval transitions contract-tested. Settings detects the plugin from OpenClaw's config and offers **Connect** |
| Regular Bash terminals | Submitted command through the shell hook; foreground argv for older terminals | Foreground process group | See README's terminal command-title behavior |
| Grok, Reasonix, Hermes | Tracked submitted input | Visible-screen indicators only, never response text: a braille-spinner line whose label is followed by a running timer (Grok `⠋ Waiting for response… 0.8s`, `Thinking… 0.0s`, `Responding… 0.1s`; Reasonix `⣽  working · 0s`, `thinking… (0s · Esc cancels)`), and Hermes' `msg=interrupt · /queue …` composer placeholder or live `⏱` status-bar timer. They disappear when the reply ends, so status returns to IDLE | Real reply frames from the phone matrix (Grok 1.0.41, Reasonix 1.39.0, Hermes 0.19.0) are unit-test fixtures in `tests/fixtures/status-frames/`. Grok and Hermes offer hooks, but only through global/user config (`~/.grok/hooks`, Hermes `config.yaml`), which the launcher does not rewrite, so there is no scoped adapter yet |
| Other launchers | Tracked input and existing agent-specific fallbacks | Existing screen heuristic | Launchability does not imply native lifecycle support |

New WAITING, ERROR and UNKNOWN badges are distinct from IDLE. An exited tmux
pane always wins over cached metadata. FINISHED/completion notifications support
Codex, Pi and newly launched Claude Code/OpenCode. OpenCode verifies the latest native assistant message after idle: matching submitted user-message parent, completed timestamp, `stop` finish, nonempty text, no error or summary. It checks at most 16 messages and suppresses stale lookups after new activity or selection changes. It never alerts for history merely opened in the TUI. Pi requires final `agent_settled`, a completed outcome and a
nonempty successful assistant text response, with a durable per-turn ID. A
Claude Stop without a tracked prompt and nonempty final text, or an
OpenCode/OpenClaw idle hook alone, is not a completion guarantee.

## Activating integrations

Rebuild the desktop, then create new Claude, OpenCode or Pi cards (or let the app
recreate missing sessions). Existing live processes are not restarted or injected
with commands. They retain their old fallback behavior until relaunched.

Adapters are scoped to the launch. No global Claude, Pi or OpenCode settings are
rewritten. Claude's normal settings continue to load, Pi's other extensions remain
loaded, and existing inline OpenCode plugin configuration is preserved. Custom
shell wrappers, explicit Claude `--settings`, disabled-plugin/bare modes, and
noninteractive invocations are left unchanged rather than silently rewriting them.
The OpenCode plugin reports IDLE as soon as it loads, because a fresh process has
no turn in flight.

An adapter that has not reported anything since its launch (no native session and
no reporter event: for example an OpenClaw gateway without the SUPER DESKTOP plugin,
or hooks that never ran) is treated as silent rather than authoritative. The card
title and the phone's `lastPrompt` then fall back to the prompt typed into that card
(`@super_desktop_last_prompt`, the same tracked input other launchers use, still
filtered by `harness_record::is_user_prompt`), and status comes from the screen
heuristic instead of a permanent UNKNOWN. As soon as the adapter reports, its prompt,
title and status win again, including an explicit UNKNOWN (for example an OpenClaw
gateway that stopped sending heartbeats). The fallback never uses response text.

Custom launchers that directly invoke `claude`, `opencode`, `pi`, or `openclaw`
receive the same scoped adapters and retain their custom launcher identity.
Shell wrappers and explicit incompatible CLI modes remain unchanged. Existing
running processes require a new launch; no live session is silently restarted.

OpenClaw needs its gateway installed/configured separately. Its cards report status,
prompts and native titles only through the SUPER DESKTOP gateway plugin. After
installing OpenClaw:

1. Open **Settings → Harness launchers** (press **Rescan** if OpenClaw is new). Under
   the OpenClaw row, a line says **Status & titles need the SUPER DESKTOP plugin**
   while the plugin is not connected. **Connect** runs the same registration as
   `super-desktop integrate-openclaw` on a worker thread (`openclaw plugins install
   --link`, `plugins enable`, and the conversation-hook grant), showing the last line
   of OpenClaw's output if a step fails. Each step is bounded to 90 seconds.
2. Then press **Restart gateway**, which runs `openclaw gateway restart` (the CLI's
   own service restart). If that fails, restart it yourself, e.g.
   `systemctl --user restart openclaw-gateway`. Recent OpenClaw versions may also
   hot-reload plugin changes; the restart makes it certain.
3. The line then shows **Connected**. Open cards update from their next turn; new
   cards report from launch.

Detection reads OpenClaw's config file, never an `openclaw` subprocess: the file is
`$OPENCLAW_CONFIG_PATH`, else `$OPENCLAW_STATE_DIR/openclaw.json`, else
`~/.openclaw/openclaw.json` (with `OPENCLAW_HOME` replacing `$HOME`), as seen by the
desktop process. The plugin counts as connected when `plugins.load.paths` lists
`~/.local/state/super-desktop/harness/openclaw`,
`plugins.entries["super-desktop-metadata"].enabled` is `true`, its
`hooks.allowConversationAccess` is `true`, and `plugins.enabled`/`allow`/`deny` do
not exclude it. The result is cached by the file's path, modification time and size,
so a card refresh costs one `stat`. Comments and trailing commas (JSON5) are
accepted; other JSON5 syntax reads as "could not be read", which still offers
Connect but shows no card hint. Named `--profile` configs and a gateway running with
a different environment than the desktop are not visible to this check.

When an OpenClaw card (built-in or a custom launcher that runs `openclaw` directly)
has a silent adapter while the plugin is not connected, the card shows its brief
notice **OpenClaw status needs setup · Settings → Harness launchers** once per
session per desktop run, the first time the card is on screen, never on every
refresh. Meanwhile the card uses the typed-prompt and screen fallbacks above.

The launcher runs `openclaw tui --session <card-session-name>`. The plugin only
observes sessions with an explicit matching desktop mapping. It does not change
permissions, supply prompts, submit messages, approve tools, or observe unrelated
gateway sessions. The gateway must run as the same local user for this integration;
remote gateways need a future authenticated event transport. The setup command
installs/enables this bundled plugin and grants its conversation-hook access;
it does not restart the gateway. Gateway labels/display names refresh every three
seconds for observed mapped sessions (maximum 256); missing heartbeats for fifteen
seconds yield UNKNOWN. Exec-approval events report WAITING; unsupported approval
types do not invent a wait signal. Native names are read, never generated here.

## Implementation contract

- `harness_metadata.rs` prepares scoped launches, records observations, validates
  process identity, sanitizes metadata and provides the shared reader.
- Each launch gets a unique private metadata file. The tmux session points to that
  file, and PID **and process start time** must match. Old files cannot describe a
  new process that reused a PID. A native session switch clears title/prompt/model.
- Hook writers use a file lock and atomic replacement. Only bounded metadata is
  read; no token stream or full terminal output is copied. A short cache shares
  one observation across a refresh's title, prompt and status queries.
- Claude hooks and the JS reporters call `super-desktop harness-event`. They never
  emit blocking decisions. Reporter failures must not interrupt a harness turn.
- Native OpenCode IDs take precedence over the legacy DB ownership matcher.
- Codex's native title logic is also shared by local cards and bridge fields.
- Runtime assets and private observations live in
  `~/.local/state/super-desktop/harness/`. Hook installation does not read API keys.

## Validation and remaining work

OpenClaw setup and silent-adapter fallback (2026-09-25): the diagnosed cause of
OpenClaw cards that never updated was an unregistered gateway plugin; after
registration and a gateway restart a real card updated its prompt, title and status
within a second. Unit tests cover plugin detection (registered, disabled, missing,
not loaded, no conversation access, `plugins.enabled`/`allow`/`deny`, JSON5 comments
and trailing commas, malformed configs) and OpenClaw's config-path overrides. GTK
child tests drive the Settings line with fake OpenClaw steps (failure, Connect,
Restart, retries) and the real panel against an isolated config. `card_title_` tests
cover the typed-prompt fallback for silent Claude/OpenCode/Pi/OpenClaw adapters, the
`is_user_prompt` filter on it, and the reporting adapter winning; each fails when its
fallback or filter is removed. No test runs `openclaw` or touches the user's gateway
or config.

Run `cargo test harness_metadata`, `cargo test completion::tests`,
`node --test tests/harness-metadata.test.mjs`, and `cargo test toolbar_`.
After `cargo build`, `python3 tests/native-harness-smoke.py` exercises installed
Claude/Pi/OpenCode adapters in isolated homes without external model requests.
Pi uses `tests/fixtures/pi-probe-provider.mjs` for real runtime success/error turns.
`python3 tests/openclaw-harness-smoke.py` tests the production setup command and
a real isolated gateway without changing the user's gateway or credentials.
Run `python3 tests/opencode-completion-smoke.py` for installed OpenCode success,
provider failure and cancellation against an isolated local fixture model.
Run `python3 tests/claude-completion-smoke.py` for installed Claude Code Stop,
StopFailure and interruption checks against an isolated local Anthropic fixture.
Run the full Rust suite before rebuilding the installed desktop.

Validation on 2026-09-23: 302 Rust tests passed, five were marked ignored, and
`test_resolve_command_ai_agent_arguments` failed because Antigravity is not
installed (the existing resolver falls back to Bash). All four toolbar checks,
two control-client tests, three JS adapter contract tests and Android unit tests
passed. Installed Pi/OpenCode smoke tests both reported their own named idle
session without model requests. OpenClaw is not installed; its tests use mocked
gateway callbacks.

Validation on 2026-09-24: Claude 2.1.278, Pi 0.87.1, OpenCode 1.18.31 and
OpenClaw 2026.9.5 passed the isolated runtime checks. Pi's local fixture provider
exercised successful settlement, failure/recovery and distinct completion IDs;
Pi/OpenCode rename and session switching passed. OpenClaw's production install,
native reset/SessionStart and idle-rename heartbeat passed against a real gateway.
Seven JS contract tests and 77 Android unit tests passed; Android lint and debug
APK build passed. Both control-client tests passed separately.

The full Rust run reported 307 passed, four failed and six ignored. All four
failures also reproduce on a clean checkout of `8a7ff7e`: the resolver test
expects an installed Antigravity CLI; the GTK outline-resize, slide-position and
mapped-toolbar-resize tests fail allocation assertions on this environment.
`cargo test toolbar_` reported three passed and the same mapped-toolbar failure,
also reproduced with an isolated Broadway display. No installed desktop rebuild
or app deployment was performed.

A later parallel full run also hit the existing ephemeral-port release test
(`test_port_taken_follows_the_listener`): 306 passed, five failed, six ignored.
That test passed on an immediate isolated rerun; no bridge-port implementation
was changed. The final serial full run reported 308 passed, the four repeatable
baseline failures and six ignored tests. Those four failures remain unresolved.

The six existing ignored tests were not enabled: `sandbox_renders_a_single_page_fixture`,
`android_page_visual_preview`, `layer_popup_stays_open`, `native_codex_attachment_probe`,
`live_logind_lock_acquires_and_releases`, and `clipboard_round_trip`. They need
explicit sandbox, interactive display, disposable Codex, logind or clipboard setup.
No additional tests were excluded. Physical-phone notification delivery was not
tested in this pass.

Before marking any adapter fully validated, exercise real model turns, tool calls,
permission accept/deny, cancellation, API failures, retry/continuation, rename,
new/resumed/forked sessions, process crashes and simultaneous same-directory cards.
The automated tests cover state transitions and isolation; they do not substitute
for that live matrix. Remaining work includes real-provider permission/cancellation
matrices, additional launcher adapters, remote OpenClaw gateway transport and
completion adapters beyond Codex/Claude/Pi/OpenCode. The September 23 OpenClaw installation
blocker above is superseded by the September 24 isolated gateway smoke. Existing
live/custom wrapper processes are deliberately not injected or restarted.

OpenCode completion follow-up (2026-09-24): eight JS contract tests, ten metadata
Rust tests and six completion Rust tests pass. Installed OpenCode 1.18.31 passes
successful turns, distinct completion IDs, provider errors and cancellation using
a local fixture model (no external model requests). The full desktop run reports
309 passed, the same four documented baseline failures, and six existing ignores.
Android unit tests, lint and debug build pass. Paid-provider permission matrices
and physical-phone OpenCode notification delivery remain unverified. The installed
desktop was not replaced or restarted; new adapter activation requires rebuilding
it and launching a new OpenCode card.

The native message shape and paginated chronological ordering were checked against
[OpenCode v1.18.31 message records](https://github.com/anomalyco/opencode/blob/v1.18.31/packages/opencode/src/session/message-v2.ts)
and [plugin hooks](https://github.com/anomalyco/opencode/blob/v1.18.31/packages/plugin/src/index.ts).

Claude completion follow-up (2026-09-24): installed Claude Code 2.1.278 passed
successful responses, distinct IDs for repeated prompts, API StopFailure and
stream-JSON interruption using an isolated local fixture. Eleven metadata tests,
six completion tests and eight JS adapter tests passed. Android unit tests, lint
and debug build passed. Full Rust results: 313 passed, one known failure
(`test_resolve_command_ai_agent_arguments`, Antigravity is absent), six existing
ignored tests listed above. Previously intermittent GTK allocation tests passed
in this run. Physical-phone Claude notification delivery remains unverified.
No installed desktop binary or running harness was replaced/restarted.

Claude completion uses the documented main-agent Stop final-text field, not
transcript flushing. Capability requires updated scoped launch hooks; missing
fields fail closed. Continued Stop hooks are conservatively suppressed. It is
response completion, not a guarantee that other Stop hooks will not request more
work, or that background tasks/goals are finished. See the completion-alert doc.

## Upstream contracts

- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [OpenCode plugins and events](https://opencode.ai/docs/plugins/)
- [Pi extension event types](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/extensions/types.ts)
- [Pi lifecycle guidance](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/extensions.md)
- [OpenClaw plugin hook reference](https://docs.openclaw.ai/plugins/hooks/reference)

Internal transcript/index formats can change. Unknown records fail closed;
terminal silence and response text never establish native completion.
