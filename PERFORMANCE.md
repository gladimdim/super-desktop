# Linux startup performance

## Startup changes (2026-09-20)

- The installed command is `target/release/super-desktop-client`, a small native
  IPC client linked only to libc/libgcc, not GTK/VTE. It handles an existing
  daemon's toggle/show/hide/status/kill commands directly. Other commands and
  cold starts delegate to the application with layer-shell preloaded. An IPC
  failure after connecting is never retried: replaying a toggle could undo it.
- The source checkout's Bash wrapper resolves its location with one subprocess
  and prefers the native client. `install.sh` and `rebuild.sh` install the native
  entry point. After `cargo clean`, build again before using that installed link.
- Settings and its Android page are constructed on first opening, then retained.
  Theme reloads do not instantiate unopened settings. This also removes the
  settings panel's duplicate harness scan from startup.
- Restored terminal widgets prepare their sessions on workers. A restoration
  batch shares one tmux session inventory instead of invoking `list-sessions`
  per terminal. Subsequent attachments use fresh inventory, not a persistent
  cache. GTK creation/attachment remains on the main thread.
- Closing a card marks its preparation cancelled immediately and serializes
  session destruction after any in-flight preparation. A late completion cannot
  attach a removed/replaced VTE. Close does not wait for tmux on the UI thread.

## Recurring-work changes (2026-09-24)

- **Layer-shell preload no longer leaks.** `main` removes only the
  `libgtk4-layer-shell.so*` entry from `LD_PRELOAD` before anything else runs
  (other user entries are kept; the variable is removed when empty). The
  library is already mapped. A cold `toggle`/`show` sets the preload
  explicitly on the spawned `daemon` only. At daemon start a worker removes the
  entry from tmux's global environment, so new panes (shells, agents, hooks)
  start clean; existing panes keep what they inherited.
- **One tmux inventory per refresh tick.** The 1 s card refresh now prepares
  every card on the main thread and runs one worker with one
  `tmux list-panes -a` (pane pid/command/dead/height/cwd plus the
  `@super_desktop_metadata`, `@super_desktop_last_prompt` and shell-title
  options; RS/US separators, ambiguous rows fall back to the per-session
  queries). A card captures its pane only for a visible preview, or for a
  screen-based status of an agent without native metadata. Opencode prompts
  and titles use the card's resolved session id (re-resolved at most every
  30 s, as before) instead of re-resolving every second.
- **Smaller captures.** Card previews, status and the bridge's launcher
  list read the visible screen plus 30 history lines instead of 300. The
  phone's per-terminal stream (`tmux_control`) still captures 300 lines for
  scroll-back; asset discovery keeps 300 lines.
- **Caches.** `sqlite3` answers are reused while `opencode.db` and its WAL keep
  the same size and mtime. Codex rollout lookup is cached per pane process
  (validated by process start time and the descriptor's link, re-walked at
  least every 5 s); rollout headers, `session_index.jsonl` titles and rollout
  prompts are reused while the file's (dev, inode, length, mtime) is unchanged.
- **Toolbar width is event-driven.** The per-frame tick callback, which kept
  the frame clock running at the display rate for the overlay's lifetime, is
  replaced by the frame clock's `layout`/`after-paint` signals, which only
  fire on frames GTK draws anyway and never while unmapped.
- **Hooks use the GTK-free client.** `super-desktop-client harness-event`
  records natively (shared `harness_record.rs`); new native-adapter launches
  point `SD_HARNESS_EXE` at the client when it is installed beside the app.
- Restoring saved cards reads `state.json` once per batch, not once per card.

### Bridge changes (2026-09-24)

- **One shared harness-list collector.** `GET /api/v1/harnesses` and
  `WS /api/v1/harnesses/stream` share one collector thread that runs only while
  there is a consumer (a stream client, or a GET in the last 5 s) and produces
  at most one document per second: one `state.json` read, one
  `tmux list-panes -a` (the card inventory), one 30-line capture per session.
  The composer draft reuses that capture (it was a second `capture-pane`), and
  native metadata, prompts and shell titles come from the inventory row (no
  per-session `list-panes`/`show-options`/`display-message`). A GET is answered
  from the shared document when it is under 1 s old. Stream clients are sent a
  document on connect, when its content changes (`timestamp` and per-harness
  `updatedAt` excluded from the comparison), and at least every 20 s as a
  heartbeat; the old stream resent every second. The document schema is
  unchanged. Metadata written as a pane/window option is now found (the
  per-session `show-options -t =session` missed it), matching desktop cards.
- **Notification-driven terminal stream.** The per-terminal stream's control
  client no longer passes `no-output`; `%output`/`%extended-output`
  notifications only mark the pane dirty (payload discarded, lines read as bytes
  so split UTF-8 cannot end the reader). Dirty panes are re-captured over the
  control connection at most ~30 times a second, with a 1 s safety capture;
  idle panes send only the 5 s heartbeat. Status and grid use the same
  connection (`list-panes -t =session:` with the inventory format,
  `display-message` for the grid): status after output at most every 500 ms and
  otherwise every 2 s, the grid after layout notifications or every 10 s. Each
  frame is serialized once and compared before serialization. Phone input wakes
  only the streams of its own session (it used to speed up every stream). New
  optional `?ansiOnly=1` omits the plain `tail` whenever `tailAnsi` is present.
  The 300-line capture history is unchanged.
- **HTTP/1.1 keep-alive.** TLS connections serve further requests after a JSON
  response unless the client sent `Connection: close` or used HTTP/1.0 (30 s
  idle timeout, 200 requests per connection, 5 s header/body deadline per
  request). Each request is parsed and checked independently (browser
  Origin/Sec-Fetch-Site refusal, bearer check, device identity for revocation);
  bytes read past a body are kept for the next pipelined request. WebSocket
  upgrades, asset/PDF downloads and image uploads still end the connection
  (`Connection: close`). The owner-only Unix control socket stays one request
  per connection. Connection admission limits are unchanged (64 total, 12 per
  address), and idle kept-alive connections count against them.
- **Throttled authorization checks.** Stream reads/writes re-read the paired
  device list at most once per second per connection instead of on every
  socket operation (streams probed their peer up to 60 times a second, each
  taking the global pairing mutex). A failed check is final. Explicit
  revocation still shuts the device's sockets down at once and expiry is still
  enforced by the 250 ms watchdog. Unchanged socket read timeouts are no longer
  re-applied on every probe.
- **Completion long poll.** `POST /api/v1/completions` returns an `etag` and
  accepts `etag` + `waitMs` (0–25000) to hold the request until the result
  changes (re-collected about once a second). Held requests no longer take the
  four asset-transfer slots; at most eight are held at once, and any excess is
  answered immediately. A hang-up or revocation abandons the wait. See
  [docs/COMPLETION_NOTIFICATIONS.md](docs/COMPLETION_NOTIFICATIONS.md).
- `port_taken` ignores a TCP self-connection to a free port. The intermittent
  `test_port_taken_follows_the_listener` failure came from releasing the port
  while parallel tests bind ephemeral ports; the test now keeps the port bound
  and toggles only its listening state.

Wire changes for clients (all additive): harness-list stream pushes on change
with a heartbeat of at least every 20 s; `?ansiOnly=1` on the terminal stream;
completion `etag`/`waitMs`; HTTP/1.1 keep-alive with `Connection: keep-alive`
on JSON responses.

Measured with an isolated second bridge (own state dir, port, and for the idle
case an isolated `tmux -L` server) and a logging `tmux` shim; the running
bridge and daemon were not restarted:

| Case | Before | After |
| --- | ---: | ---: |
| tmux processes per harness-list document, 4 live sessions | 19 | 5 |
| Harness-list frames in 46 s, 2 idle sessions, 1 change | ~46 | 6 (connect, 2 settle, change, 2 heartbeats) |
| tmux processes, idle shell terminal stream, 12 s | ~48 (`list-panes` + `display-message` every 500 ms) | 1 (the control client) |
| Terminal output to phone frame (test, `cat` pane) | ≤ 100 ms poll | < 700 ms asserted; notification-driven |
| `?ansiOnly=1` first frame, small shell pane | 352 bytes | 285 bytes |

The "before" stream counts are derived from the old code (2 processes per
status tick for a shell pane, more for native agents, at 2 ticks/s), not
measured. With busy agents, previews change
every second, so the list stream still sends about one document per second;
the saving there is that N clients share one collection.

### Measurements (development machine, 2026-09-24)

| Command, median of 20–30 runs | Clean env | Layer-shell inherited |
| --- | ---: | ---: |
| `tmux list-panes -a` (wall / CPU) | 1.82 / 1.42 ms | 35.92 / 34.07 ms |
| `super-desktop harness-event claude` | 23.37 ms | 23.80 ms |
| `super-desktop-client harness-event claude` | 0.66 ms | 24.09 ms |

Subprocesses for one refresh of three test cards (two shells without preview,
one screen-based agent with preview), counted with a logging `tmux` shim: 11
tmux processes on the old per-card path, 3 on the batched path (1 inventory +
2 captures). A native-metadata or shell card without a preview needs none of
its own. An idle mapped test window with the event-driven toolbar painted
fewer than 10 frames in 500 ms; the same window with the old tick callback
painted 27. No end-to-end daemon CPU or FPS figure was measured; the running
daemon was not restarted.

## Measurements and limits

Read-only warm `status` benchmarks against the same running daemon, 15 samples
per case, on the development machine:

| Entry point | No inherited preload | Inherited layer-shell preload |
| --- | ---: | ---: |
| Previous Bash wrapper | 25.12 ms median | 96.84 ms median |
| Native control client | 0.52 ms median | 24.23 ms median |

The shell running the measurements had `LD_PRELOAD=libgtk4-layer-shell.so`.
That forces even a GTK-free client (and the wrapper's subprocesses) to load GTK.
The no-preload samples removed it only from the benchmark child environment;
no user or desktop environment configuration was changed. Earlier samples under
different load measured approximately 88 ms versus 17 ms with that preload.

These figures measure command round trips, **not cold startup, first visible
frame, animation completion, or terminal readiness**. Compiler release settings
already use optimization level 3 and fat LTO. No FPS or cold-start speedup is
claimed from the command benchmark.

One instrumented restart with the existing five-terminal/one-note desktop
(three non-iconified terminals) recorded IPC listening at 59 ms, overlay
construction from 105.4 to 112.1 ms, the first frame-clock tick at 165 ms, and
terminal attach process spawning at 242–260 ms. All terminal pane IDs and process
IDs survived unchanged. This was a single warm-filesystem restart, not a
statistical cold-boot comparison; it demonstrates that terminal preparation
finishes after the overlay can start drawing.

## Profiling startup

Set `SUPER_DESKTOP_PROFILE_STARTUP=1` when starting a daemon to log elapsed stage
times to stderr: process entry, GTK initialization, theme/styles, IPC listening,
overlay construction, first frame-clock tick, terminal preparation, and attach
process spawning. Normal starts do not log timings. No terminal content, note
text, or credentials are logged.

Only profile a fresh daemon during a planned restart; a second daemon correctly
refuses to take over the existing one. Run the main process in the foreground
(`SUPER_DESKTOP_PROFILE_STARTUP=1 bin/super-desktop start`) or capture its stderr
in the service manager. Auto-start via `toggle` discards the spawned daemon's
stderr. A frame-clock tick is not proof of compositor presentation, and spawning
an attach process is not proof that the first terminal output has rendered.

Remaining targets: profile VTE/widget construction with larger saved desktops
and measure actual frame presentation. The bridge's per-session polling now
uses the batched inventory (see the bridge changes above).

## Verification

`cargo test --bins`

`cargo build --release --bins`

Tests include lazy Settings creation/reopening, close-versus-prepare ordering,
GTK cancellation before dispatch, restoration inventory scope, and refusing to
replay a control command after a connected daemon returns an empty response.
