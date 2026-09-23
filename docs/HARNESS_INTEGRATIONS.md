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
| Codex | Own process's open CLI rollout and exact thread ID in its session index; generated/renamed name; own user messages | Explicit working and completed events; ambiguous/partial metadata is UNKNOWN, not guessed idle | Existing rollout adapter plus exact-name tests, including another conversation and a child agent |
| Claude Code | Scoped hooks identify the native session; submitted prompts; session name when supplied and bounded `custom-title` transcript records | Session start/stop, prompt/tool activity, permission wait, API error, compaction | Added automatically to direct CLI launches; 2.1.278 accepted the configuration and emitted SessionStart in an isolated no-prompt run; lifecycle reducer tests |
| OpenCode | Per-launch plugin tracks the selected root session and its native title; child sessions cannot claim the card; owned DB title fallback for older sessions | Native session status (busy/retry/idle), permissions/questions, error | Added automatically to direct CLI launches; 1.18.31 loaded the plugin and reported a real locally created session/title; plugin lifecycle contract tests |
| Pi | Per-launch extension reads session ID/name/model and submitted prompt; session switches clear old data | Agent start and final settlement, error/abort outcome, blocking extension UI prompts | Added automatically using `--extension`; 0.87.1 loaded it in isolated offline RPC mode and reported a named idle session; retry/settlement contract tests |
| OpenClaw TUI | Separate `sd_term_*` gateway session per card; prompt-based title; no guessed generated conversation name | Gateway session start, prompt/model start, run success/error and end | Discoverable after installing `openclaw`; requires the bundled gateway plugin below. Gateway mapping/hook contract tests pass; live gateway validation is pending because OpenClaw is not installed on this development PC |
| Regular Bash terminals | Submitted command through the shell hook; foreground argv for older terminals | Foreground process group | See README's terminal command-title behavior |
| Other launchers | Tracked input and existing agent-specific fallbacks | Existing screen heuristic | Launchability does not imply native lifecycle support |

New WAITING, ERROR and UNKNOWN badges are distinct from IDLE. An exited tmux
pane always wins over cached metadata. FINISHED/completion notifications remain
Codex-only: a Stop/idle hook is not proof that a response is eligible for a
completion notification, especially when other hooks can continue a turn.

## Activating integrations

Rebuild the desktop, then create new Claude, OpenCode or Pi cards (or let the app
recreate missing sessions). Existing live processes are not restarted or injected
with commands. They retain their old fallback behavior until relaunched.

Adapters are scoped to the launch. No global Claude, Pi or OpenCode settings are
rewritten. Claude's normal settings continue to load, Pi's other extensions remain
loaded, and existing inline OpenCode plugin configuration is preserved. Custom
shell wrappers, explicit Claude `--settings`, disabled-plugin/bare modes, and
noninteractive invocations are left unchanged rather than silently rewriting them.
An adapter that has not emitted attributable metadata reports UNKNOWN.

OpenClaw needs its gateway installed/configured separately. After installing it:

1. Run `super-desktop integrate-openclaw` to register the bundled local plugin.
2. Restart the OpenClaw gateway using your usual gateway service workflow.
3. Rescan harness launchers in SUPER DESKTOP settings and launch OpenClaw.

The launcher runs `openclaw tui --session <card-session-name>`. The plugin only
observes sessions with an explicit matching desktop mapping. It does not change
permissions, supply prompts, submit messages, approve tools, or observe unrelated
gateway sessions. The gateway must run as the same local user for this integration;
remote gateways need a future authenticated event transport. OpenClaw permission
waits and generated/renamed gateway titles are not yet verified native capabilities.

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

Run `cargo test harness_metadata`, `cargo test completion::tests`,
`node --test tests/harness-metadata.test.mjs`, and `cargo test toolbar_`.
After `cargo build`, `python3 tests/native-harness-smoke.py` exercises installed
Pi/OpenCode adapter loading in isolated homes without making model requests.
Run the full Rust suite before rebuilding the installed desktop.

Validation on 2026-09-23: 302 Rust tests passed, five were marked ignored, and
`test_resolve_command_ai_agent_arguments` failed because Antigravity is not
installed (the existing resolver falls back to Bash). All four toolbar checks,
two control-client tests, three JS adapter contract tests and Android unit tests
passed. Installed Pi/OpenCode smoke tests both reported their own named idle
session without model requests. OpenClaw is not installed; its tests use mocked
gateway callbacks.

Before marking any adapter fully validated, exercise real model turns, tool calls,
permission accept/deny, cancellation, API failures, retry/continuation, rename,
new/resumed/forked sessions, process crashes and simultaneous same-directory cards.
The automated tests cover state transitions and isolation; they do not substitute
for that live matrix. Remaining priorities are OpenClaw live gateway validation,
its permission/title events, legacy/custom-launch migration, and native adapters
for additional installed harnesses. Record exact versions and observed failures
here as those checks are completed.

## Upstream contracts

- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [OpenCode plugins and events](https://opencode.ai/docs/plugins/)
- [Pi extension event types](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/extensions/types.ts)
- [Pi lifecycle guidance](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/extensions.md)
- [OpenClaw plugin hook reference](https://docs.openclaw.ai/plugins/hooks/reference)

Internal transcript/index formats can change. Unknown records fail closed;
terminal silence and response text never establish native completion.
