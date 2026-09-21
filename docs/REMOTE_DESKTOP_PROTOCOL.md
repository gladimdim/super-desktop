# Desktop protocol: first implementation increment

This document accompanies [the implementation plan](REMOTE_DESKTOP_PLAN.md).
The machine selector and network terminal transport are **not available yet**.
This increment adds protocol types, authenticated feature negotiation, and an
isolated server-side PTY implementation with tmux/VTE integration tests.

## Available endpoint

`GET /api/v1/desktop/capabilities` requires the existing paired-device bearer
credential over HTTPS. It returns:

```json
{
  "machineId": "<existing persistent bridgeId>",
  "desktopApiVersion": 1,
  "capabilities": []
}
```

The empty list is intentional: neither complete workspace synchronization nor
network PTY attachment is implemented. A future desktop client must require
both `workspace-layout-v1` and `terminal-pty-v1`, together with a supported
`desktopApiVersion`, before enabling interactive remote desktop mode. Unknown
optional capabilities can be ignored. Missing endpoint, missing capability or
unsupported version means the host needs an update, not permission to fall back
to snapshot terminal emulation.

The endpoint uses the same auth, browser-origin rejection, credential expiry
and revocation rules as existing bridge endpoints. Security protocol remains
v3; Android endpoints and credentials are unchanged.

## Initial contracts

`src/desktop_protocol.rs` is the canonical serde schema for the next increments:

- `CardKey` combines machine and card identity. `MachineSelection::Local` works
  independently of bridge identity and is the startup default.
- `WorkspaceSnapshot` has a machine ID, daemon epoch, global revision, logical
  canvas metadata, host folders/harness types and explicit card DTOs. It never
  serializes the local application's complete state.
- Cards distinguish card ID from session name, saved geometry from transient
  expansion, missing sessions from removed cards, and card revision from workspace
  revision. A terminal size is columns/rows, not card pixels.
- `WorkspaceEvent` initially provides full snapshots and unavailability. Incremental
  deltas are intentionally not specified until gap recovery is implemented.
- Commands are typed envelopes with request/machine/epoch identity. Create takes
  a harness type and host workspace, not a shell command. Close and layout updates
  require a card revision; default-folder updates require a workspace revision.
  Mutation DTOs reject unknown fields. Handlers must additionally validate
  ownership, epochs, revisions, paths, layout limits and request deduplication.
- Replies echo request identity and authoritative epoch/revision, with a typed
  applied/rejected result. An acknowledgement is not yet an implemented durability
  guarantee: the host model and persistence integration come next.

These are compiled contracts, not available workspace/command routes. Future
arrange/raise/expand operations and the WSS attach envelope will be added alongside
their implementations. Do not advertise support from DTO availability alone.

## PTY feasibility result and required sizing policy

`src/terminal_transport.rs` implements an existing-session tmux attach on a Linux
PTY, preserving binary output and supporting bounded nonblocking input/output.
Dropping an attachment kills/reaps only that tmux client. It neither creates a
harness nor changes host session options. It is not currently callable over the
network or through a desktop widget.

Testing on tmux 3.7c found that `ignore-size` alone is insufficient: when all
attached clients have that flag, they can still influence the host grid. This
matches tmux's [client size arbitration](https://github.com/tmux/tmux/blob/master/resize.c).
Accordingly, the prototype rejects attachment unless the target window already
uses `window-size manual`. Tests simulate the future host model by explicitly
setting a 120×40 grid using `resize-window` before attaching.

The next host-model implementation must own that policy for exported sessions,
apply local card resizes explicitly, account for any tmux status rows, and keep
the grid consistent while hidden. It must not simply enable manual sizing on
all existing sessions without also implementing local resize behavior. The
current application does not change production session sizing in this increment.

The tests prove:

- Two differently sized viewers, viewport resize, detach and reconnect preserve
  the host's shell PID and 120×40 grid under manual sizing.
- Shell input, Unicode, alternate-screen application output and Ctrl+C work over
  the raw PTY connection; reconnect obtains a fresh tmux redraw.
- Real VTE renders the raw output even when bytes split escape/UTF-8 sequences.
- Unmanaged sizing and invalid session names are rejected before attachment.

Still to prove before network release: pinned WSS duplex transport, revocation
during raw input/output, prompt-transaction input arbitration, bounded slow-reader
behavior, full-screen application mouse/paste/IME through an actual viewer,
selection cancellation and host-driven resize updates. Do not bypass these gates.

## Test on another Linux desktop

After checking out this branch, use the existing development dependencies
(Rust, tmux, GTK4, VTE and a graphical session):

```bash
cargo test --bin super-desktop desktop_protocol::tests
cargo test --bin super-desktop terminal_transport::tests -- --nocapture
cargo build --bin super-desktop
python tests/bridge_security_smoke.py target/debug/super-desktop
./rebuild.sh --no-daemon
```

PTY tests use private tmux sockets and `/dev/null` tmux configuration, create only
their own test shell, and clean up that session. They do not attach to your
existing sessions. The VTE check runs in an isolated child process and requires
a working display; it intentionally reports failure if GTK cannot initialize.
The bridge smoke test uses temporary bridge state, credentials and an ephemeral
port. `--no-daemon` now builds without stopping the daemon, installing symlinks
or refreshing assets. Use `./rebuild.sh` separately to install/restart deliberately.

Validation on the development PC (2026-09-21): new protocol and tmux/VTE tests
passed, the extended bridge security smoke passed, and the release build passed.
The serial full suite reported 194 passed, 4 ignored and one existing failure:
`tmux::tests::test_resolve_command_ai_agent_arguments` assumes Antigravity is
installed, but this PC falls back to its shell. The same failure was reproduced
on the unmodified base commit `013492a`. The parallel run also had transient
failures in the existing released-port and shortcut-stub checks; both passed in
the serial run. Those unrelated tests were not changed by this increment.
