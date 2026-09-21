# Desktop protocol and PC-to-PC increment status

This document accompanies [the implementation plan](REMOTE_DESKTOP_PLAN.md).
The machine selector now offers a **read-only remote layout preview**.
Interactive remote consoles and network terminal transport are not available yet.
Implemented: protocol negotiation, a daemon-owned local workspace model,
authenticated workspace snapshots/events, persisted terminal stacking order, and
an isolated server-side PTY implementation with tmux/VTE integration tests.
Outgoing certificate-pinned pairing and remote snapshot retrieval are available
through the CLI (see below). Remote mutations and WSS terminal
attachment remain pending; this is still an incremental development branch.

## Available endpoints

`GET /api/v1/desktop/capabilities` requires the existing paired-device bearer
credential over HTTPS. It returns:

```json
{
  "machineId": "<existing persistent bridgeId>",
  "desktopApiVersion": 1,
  "capabilities": ["workspace-snapshot-v1"]
}
```

`workspace-snapshot-v1` advertises read-only workspace snapshots/events. It does
not advertise writable layout synchronization or interactive terminals. A future
desktop client must require
both `workspace-layout-v1` and `terminal-pty-v1`, together with a supported
`desktopApiVersion`, before enabling interactive remote desktop mode. Unknown
optional capabilities can be ignored. Missing endpoint, missing capability or
unsupported version means the host needs an update, not permission to fall back
to snapshot terminal emulation.

All desktop endpoints use the same auth, browser-origin rejection, credential
expiry and revocation rules as existing bridge endpoints. Security protocol
remains v3; Android endpoints and credentials are unchanged.

| Endpoint | Behavior |
| --- | --- |
| `GET /api/v1/desktop/workspace` | Complete current workspace snapshot, retrieved from the local daemon via Unix IPC. |
| `GET /api/v1/desktop/events` (WSS) | Initial complete snapshot, then changed snapshots and five-second heartbeats. |

Events use `{"type":"snapshot","workspace":{...}}` or
`{"type":"unavailable","error":"desktop_unavailable"}`. Events poll the owning
model every 500 ms and coalesce intermediate edits; every message is self-contained
and no delta application is necessary. Discard prior assumptions on epoch changes.
Streams expire after 30 minutes and ask the client to reconnect, like the existing
phone stream. Credential revocation closes these streams immediately.

An unavailable daemon returns HTTP 503 rather than an empty workspace. A daemon
whose window has not warmed up yet returns `desktop_not_ready` (503); a timeout
returns 504; a malformed/unsupported owner IPC response returns 502. A live stream
reports unavailability and resumes complete snapshots when the daemon returns.
Snapshot responses are bounded to 1 MiB over IPC and 256 saved terminal cards.

The bridge supplies its own persistent machine ID. The daemon never reads or
rewrites the bridge credential database. The DTO excludes notes, launch commands,
agent-session mappings, OS settings and any future outgoing peer credentials.

## Initial contracts

`src/desktop_protocol.rs` is the canonical serde schema for the next increments:

- `CardKey` combines machine and card identity. `MachineSelection::Local` works
  independently of bridge identity and is the startup default.
- `WorkspaceSnapshot` has a machine ID, daemon epoch, global revision, logical
  canvas metadata, host folders/harness types and explicit card DTOs. It never
  serializes the local application's complete state.
- Cards distinguish card ID from session name, saved geometry from transient
  expansion, missing sessions from removed cards, and card revision from workspace
  revision. A terminal size is columns/rows, not card pixels. `sessionAlive` and
  harness `available` are nullable: `null` means unknown while background discovery
  is pending or unavailable. `status` is `RUNNING`, `EXITED` or `UNKNOWN`; this API
  does not yet distinguish a busy agent from an idle running process.
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

Workspace snapshot/event routes are available. Command DTOs remain contracts
without network handlers. Future arrange/raise/expand operations and the WSS attach
envelope will be added alongside their implementations.

## Workspace ownership and revisions

`AppContext` owns `LocalWorkspace`; the existing window shares its local state
handle. CLI and Android operations keep referring to that local state. This is
an incremental extraction: lifecycle mutations still run through existing local
widget callbacks and are not yet a remotely writable model.

Snapshots read in-memory state, including unsaved edits. They do not reload
`state.json`, show the overlay, create cards, or attach terminals. A background
worker is started lazily for runtime inventory and only probes while snapshots
are requested. It coalesces requests, polls at most once per second, bounds the
tmux subprocess to 750 ms/512 KiB output, and marks cached results older than five
seconds unknown. Titles reflect the owning card's cached title, including while
hidden; this API does not independently query harness conversation history.

A random epoch lasts for the daemon lifetime. Workspace revisions advance when
published content changes; individual card revisions advance only when that
card's exported content changes. Identical reads preserve revisions. Removals are
represented by absence from a complete snapshot. A future mutation handler must
publish current state before validating a client revision; this is not a log of
every pointer-motion event and no persisted revision continuity is promised.

`terminal_order` stores terminal IDs back-to-front in `state.json`. Loading old
state fills missing IDs in prior creation order, removes obsolete/duplicate
entries, and preserves valid ordering. Raising a terminal persists the new order;
notes retain their existing separate stacking behavior. Icon positions and saved
card dimensions remain separate from transient expansion and overlay animation.
Canvas metadata reports the logical placement canvas used by the current renderer,
including its existing minimum-size behavior. Actual-monitor fitting and topology
handling remain part of the future viewer/layout milestone.


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
cargo test --bin super-desktop workspace_model::tests
cargo build --bin super-desktop
python tests/bridge_security_smoke.py target/debug/super-desktop
./rebuild.sh --no-daemon
```

PTY tests use private tmux sockets and `/dev/null` tmux configuration, create only
their own test shell, and clean up that session. They do not attach to your
existing sessions. The VTE check runs in an isolated child process and requires
a working display; it intentionally reports failure if GTK cannot initialize.
The bridge smoke test uses temporary bridge state, credentials and an ephemeral
port and private daemon IPC stub, so snapshot tests never query your real desktop.
They exercise geometry updates, daemon-epoch changes, removals, malformed replies,
unavailability and revocation on the real TLS/WSS server. `--no-daemon` builds without stopping the daemon, installing symlinks
or refreshing assets. Use `./rebuild.sh` separately to install/restart deliberately.

To inspect real local layouts after deliberately installing this build:

```bash
./rebuild.sh
super-desktop desktop-workspace | python -m json.tool
```

Move, resize, iconify or raise a terminal and repeat the command. The card geometry,
order and revisions should change accordingly. Hiding the overlay should still
allow snapshots without showing it. Runtime fields may be `null` on the first read;
a subsequent read after a second should have inventory if tmux is reachable.
There is no machine selector to test yet.

The first increment's full-suite baseline has one known failure:
`tmux::tests::test_resolve_command_ai_agent_arguments` assumes Antigravity is
installed, but this PC falls back to its shell. The same failure was reproduced
on the unmodified base commit `013492a`. This increment's serial suite reports
201 passed, 4 ignored and that same single failure. No unrelated tests were changed.
The extended TLS/WSS security smoke test and build-only release validation passed.

## Outgoing PC pairing (CLI increment)

Both PCs must run this branch. On the host, enable the bridge in Settings →
Android, generate an invitation and copy its pairing link. On the viewing PC:

```sh
super-desktop peer-add --host 192.168.1.20 --name "Work laptop"
# Paste the invitation at the prompt; input is hidden.
super-desktop peer-list
super-desktop peer-workspace MACHINE_ID
super-desktop peer-forget MACHINE_ID
```

`--host` and `--port` override the invitation's address (useful for LAN/VPN
routing). The certificate pin still comes from the invitation. Compare the
six-digit code and approve the request on the host. Invitations are read from
stdin, never from command arguments; scripts may supply a compact JSON
invitation or the existing `superdesktop://pair?data=...` link. Treat invitations
as secrets. The viewer stores the peer only after host approval. New application starts select This PC. After pairing, open the top-left machine
selector to display the remote layout preview. The CLI commands themselves do
not switch the displayed workspace.

`peer-workspace` returns the host's typed layout snapshot, including card
positions, sizes and stacking order. The host desktop daemon must be running.
It checks the pinned certificate, persistent machine identity and desktop
capabilities before fetching the layout. Errors contain no credentials or
response bodies. Redirects, environment proxies and automatic retries are
disabled. Pairing waits up to two minutes; an ambiguous submission failure
requires checking the host and generating a fresh invitation rather than
silently retrying it.

Outgoing credentials live in `~/.local/state/super-desktop/peers.json`
(directory 0700, file 0600), separate from the bridge's incoming devices. They
are **not encrypted at rest**; processes running as the same user can read them.
`SUPER_DESKTOP_PEERS_STATE_DIR` overrides this directory for testing. The store
rejects unsafe permissions and symlinks, uses a lock to prevent lost updates,
and replaces records atomically. `peer-list` prints only public metadata.
`peer-forget` removes the local record; revoke the device on the host to
invalidate its token. Re-pairing the same machine replaces its saved endpoint,
pin and credential after approval. Older hosts may omit expiry metadata; host
HTTP authorization remains authoritative.

Run the isolated integration regression with:

```sh
python3 tests/peer_pairing_smoke.py target/release/super-desktop
```

It starts disposable bridges and a fake owner IPC endpoint. It tests real TLS,
CLI pairing, approval/denial, pin rejection, self-pair rejection, private
storage, snapshots, identity changes, expiry, local removal and host revocation.
No production daemon or bridge credentials are used.


## Machine selector and remote layout preview

The top-left selector defaults to **This PC**. Open it to read the current peer
registry, including PCs added with `peer-add` while the application was running.
Selecting a peer displays its host workspace path, harness names and console
layout. The preview preserves card positions, sizes, stacking, iconified
positions and expanded geometry, uniformly scaled to fit the viewer. Physical
monitor scale is not applied again to the host's logical coordinates.

This is explicitly a **read-only layout preview**, not a terminal screenshot or
interactive console. No remote content is passed to the local terminal/session
creation code. The local canvas and widgets remain alive while hidden, so
switching to This PC restores their existing state. Local launch and arrange
controls are absent from remote mode, and Ctrl+N cannot create a local note
while a remote PC is selected. Incoming bridge snapshots still describe this
PC's local model even while it is viewing another PC.

The preview fetches snapshots every two seconds while mapped, with one request
in flight at a time. Network and registry operations run outside GTK. Each
selection has a generation number: late responses from an earlier PC or an
earlier visit to the same PC are discarded. Disconnect, revocation and invalid
layout responses clear the preview and show a status message. Reopening the
window refreshes the remote snapshot. Application restart returns to This PC;
hiding and showing the same window preserves its current selection.

This increment uses bounded HTTPS polling. Live WSS subscriptions, graphical
peer removal, terminal streaming and remote layout mutations remain pending.


## Add a PC from the selector

Open the top-left selector and choose **Add a PC**. On the host, open
Settings → Android, enable the bridge, generate an invitation and expand
**Copy pairing link**. Paste that link into the viewer's form. A friendly name
and connection address/port overrides are optional. Click **Connect**, compare
the six-digit code on both PCs, and approve on the host. The approved peer is
saved and its read-only layout preview opens automatically.

The invitation field is hidden and cleared after submission or closing the
form. Network requests and private-store writes run on a worker. Only one
pairing worker can run at a time. Cancel/Back, closing the menu or hiding the
application cancels an unfinished attempt. A request already submitted to the
host can remain pending there: deny it on the host, and use a fresh invitation
for another attempt. Cancellation before saving claims completion prevents a
late approval from writing a peer; a save already begun is allowed to finish.
Host revocation remains authoritative if access must be withdrawn.

The existing isolated pairing smoke test can also drive the real GTK form
without opening a production window. Build tests with `cargo test --no-run`,
then supply the main test executable as a second argument:

```sh
python3 tests/peer_pairing_smoke.py target/release/super-desktop target/debug/deps/super_desktop-TEST_HASH
```

This exercises GUI approval, denial and cancellation before a late host
approval, then checks persisted peers, layout retrieval and host revocation.
