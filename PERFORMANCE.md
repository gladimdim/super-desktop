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

Remaining targets: batch recurring pane metadata/status work, profile VTE/widget
construction with larger saved desktops, and measure actual frame presentation.

## Verification

`cargo test --bins`

`cargo build --release --bins`

Tests include lazy Settings creation/reopening, close-versus-prepare ordering,
GTK cancellation before dispatch, restoration inventory scope, and refusing to
replay a control command after a connected daemon returns an empty response.
