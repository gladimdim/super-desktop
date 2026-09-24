# Mobile terminal output compatibility

The Android/iOS snapshot contract captures up to 300 tmux history lines plus the
visible pane, including ANSI styling and its existing column width. Apps that
keep their history entirely in an alternate screen cannot expose that history
through capture alone. Do not stitch repaint frames into a fabricated transcript
or resize the shared desktop pane to force more text into a phone snapshot.

## Stream frames (`WS /api/v1/harnesses/<id>/stream`)

Each text frame carries `tail` (plain text) and `tailAnsi` (the same capture
with SGR styling), plus `tailFormat`, `columns`, `rows`, status and title.
Clients that derive plain text themselves may connect with `?ansiOnly=1`
(`true` is also accepted): the bridge then omits the `tail` key whenever
`tailAnsi` is present, roughly halving each frame. Without the parameter frames
are unchanged. When the session is gone the final `EXITED` frame keeps
`tail: null` and `tailAnsi: null` either way, then the socket closes with 1000
`session ended`.

Frames are pushed when the pane changes: the stream's private tmux control
client listens for `%output` notifications (the payload is discarded) and
re-captures at most about 30 times a second, with a 1 s safety capture in case a
notification is missed. An unchanged frame is resent every 5 s as before.
Phone input wakes only the streams of the session it was sent to.

## Launch defaults

| Agent | Default | Verification on 2026-09-24 |
| --- | --- | --- |
| Codex | `--no-alt-screen` | Existing live sessions have history; isolated startup uses main screen. |
| Grok | `--minimal` | Live `/minimal` transition preserved conversation and exposed 102 history lines plus 20 visible rows. |
| OpenCode | `--mini` | Installed 1.18.31 default uses alternate screen; mini startup uses main screen. |
| Pi | `--tui-mode regular` | Installed 0.87.1 regular startup exposes history; fullscreen does not. |
| Hermes | `--cli` | Installed 0.19.0 classic startup uses main screen; provider-authenticated long response not exercised. |
| Claude Code | `--settings` with `"tui": "default"` (merged into the launcher's hook settings) | 2.1.281 with a user `"tui": "fullscreen"` setting: default launch used the alternate screen (no history); the override kept the main screen and a local `!seq 1 200` produced 196 history lines. |
| Gemini, Cursor | Existing defaults | Startup/main screen only; login/onboarding prevents claiming full-response verification. |
| OpenClaw | Existing `tui` | Installed 2026.9.5 constructs `TuiMainScreen`, whose renderer uses main screen and scrollback. Live gateway response not exercised here. |
| Crush | Existing fullscreen UI + remote paging | Installed 0.96.1 long saved-response fixture verifies Tab, Page Up, and Page Down. |
| Reasonix | Existing `code` (alternate screen) + remote paging | 1.39.0 has no main-screen/inline mode: `reasonix --help`/`code --help` list no display flag, its `[ui]` settings are only `cursor_shape`, `shortcut_layout` and `show_turn_usage`, and no `REASONIX_*` variable selects one (its Bubble Tea view always requests the alternate screen). The phone matrix shows `alternate_on=1` and zero history before and after a reply. Reasonix documents PgUp/PgDn as transcript scrolling, so the phone's PgUp/PgDn keys are the way to read older output. No launch flag is added. |
| Other/custom agents | Existing defaults + remote paging keys | No blanket compatibility claim; unavailable CLIs not exercised. |

Requires CLI versions supporting these flags. Existing custom permission flags
no longer suppress display defaults. Explicit custom mode overrides (Grok
`--fullscreen`, Pi `--tui-mode`, Hermes `--tui`) are respected; explicit shells
are unchanged. Defaults apply to future launches, not a silent restart of a live
conversation. Grok alone has a verified in-place `/minimal` switch used here.

## Fullscreen navigation from the phone

Android’s keyboard panel includes PgUp, PgDn, Tab, and Shift-Tab in addition to
arrows, Esc, Enter and Ctrl-C. In Crush, press Tab to focus the conversation,
then PgUp/PgDn to read older/newer pages; Tab returns to the remote composer.
The remote program defines each key’s meaning. Phone swipes still scroll the
local snapshot; paging keys deliberately change the desktop viewport too.

The existing ordered input socket carries `ESC [ 5 ~`, `ESC [ 6 ~`, HT and
`ESC [ Z`, with `enter:false`. No new bridge protocol or permissions are needed.
No local draft or attachment is sent/cleared. Offline/exited keys remain disabled;
uncertain acknowledgements are never replayed automatically.

## Verification

- Android debug APK, test APK, and unit tests pass.
- Rust default-mode/custom-command tests pass, including flag deduplication,
  explicit overrides, and shell preservation.
- `python3 tests/crush-output-smoke.py` uses an isolated HOME/config/data/runtime,
  a loopback-only dummy provider, and a seeded 80-line saved response. It resumes
  that fixture and verifies Tab/PageUp reveal older lines and PageDown returns
  to the newest. No prompt or model request is issued; test processes are removed.
  Set `CRUSH_TEST_BINARY` to a real installed binary if mise is unavailable.
- Full Rust suite: 307 passed, five failed, six ignored. Failures match the
  observed port-release check, missing Antigravity command, and three mapped-GTK
  geometry checks; this change does not disable or weaken them. The release build
  succeeds. These failures mean the full suite is not green.
- Desktop reload preserved the three pre-existing tmux pane identities.

For the iOS implementation, copy the key payloads and pane-specific routing in
`OmarchyAILauncher/PROTOCOL.md`, and the keyboard-panel and limits descriptions in
`designs/ANDROID_FEATURES.md` and `designs/ANDROID_UI_SPEC.md`. Main-screen startup
verification must not be described as a live model-response test.
