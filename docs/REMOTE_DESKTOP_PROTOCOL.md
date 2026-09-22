# Desktop protocol and PC-to-PC increment status

This document accompanies [the implementation plan](REMOTE_DESKTOP_PLAN.md).
The machine selector now opens a **live remote workspace**: the host's consoles
are streamed over pinned WSS and rendered in real VTE widgets at the host's own
positions, sizes, stacking order and iconified state. Clicking a console and
typing (including paste and Ctrl+C) writes into that host session, and dragging
its header moves and raises the host's own card, and the top bar offers that
PC's own harness buttons, so a click launches the harness on that PC. The remote
workspace is drawn by the same widgets a local one uses — the same harness bar
and the same card, with its own buttons, drags and edges — and only their source
differs. Implemented: protocol negotiation, a daemon-owned local workspace model,
authenticated workspace snapshots/events, persisted terminal stacking order, the
host-side PTY attach transport with a host-owned grid, viewer keystrokes on that
stream, the viewer's live consoles, and the typed command route with `setLayout`,
`closeTerminal`, `setExpanded` and `createTerminal` behind epoch/revision checks
and per-device deduplication.
Outgoing certificate-pinned pairing, remote snapshot retrieval and a
`peer-attach` streaming CLI are available too (see below). The default-folder
command, the viewer's own close/iconify controls and live outgoing workspace
subscriptions remain pending; this is still an incremental development branch.

## Delivered user flow and current limit

The top-left **This PC** control is the machine selector. **Add a PC** has two
flows: create/copy a single-use connection link to let another PC connect here,
or paste a link from another PC to connect this PC there. Host approval and
six-digit-code comparison remain mandatory. Once approved, the peer is selected
and its workspace is drawn live: every visible console is a real terminal that
shows the host session's output, in colour, as it happens.

Click a console and type. VTE translates the keystroke (application cursor
keys, bracketed paste, IME) and the viewer sends those bytes after the host's
`attached` frame. The host writes them into the session PTY behind the same
input guard as phone and image-prompt input, and drops them if that guard is
busy or the PTY cannot accept them. Keys are not replayed after a disconnect,
a hide, or a switch to another PC. The viewer holds at most eight attachments
at once.

Dragging a card's header moves and raises it on the host: the gesture ends with
one typed `setLayout` command carrying the card revision the viewer drew, so a
concurrent host edit is refused as a conflict and the card snaps to the host's
real geometry. Layout commands are sent only to a host that advertises
`workspace-layout-v1`; a host that does not keeps its own layout and the drop
reverts, and a card the host shows expanded keeps its own rectangle. The same
route lets the host apply a resize, an iconify or a close — those are implemented
and tested host-side, and the viewer's own controls for them are the next
milestone.

A remote console is the same card a local one is, so its own header buttons work
on the host: minimize and maximize send `setLayout` and `setExpanded`, close
sends `closeTerminal`, and dragging the header or an edge sends one `setLayout`
carrying whatever the gesture changed. The host's snapshot is what this view
draws, so a control never invents a state the host did not report.

The top bar is the same bar too: the host's own harness list, with the same
buttons, labels, logos, tooltips and order its local toolbar shows, taken from
that host's snapshot (`harnessTypes` plus `visibleHarnesses`, which is the host's
stored selection minus what is not installed there). Clicking one sends
`createTerminal` and the host creates the card in the folder it published,
exactly like a local launch, without showing its own overlay. A host that does
not advertise `workspace-layout-v1` shows no buttons and says to update it, there
are no usage cards here (those numbers describe this PC, not that one), and
choosing another folder on that PC is not available yet. Cards the host shows in
front are the ones
that get live output; the rest keep their chrome with an explanation. While
the overlay is hidden the streams are released (the host keeps its sessions and
every card keeps its last frame), and showing it again reconnects at once.

Both PCs must run a current bridge. After pulling an update, run `./rebuild.sh`:
it now stops the independent `super-desktop harness-bridge` process as well as
the daemon, preventing an old bridge on port 8759 from masking the rebuilt code.
If a selected peer says to update/rebuild the host, update **that host** and run
the script there; the saved pairing does not need to be recreated.

## Available endpoints

`GET /api/v1/desktop/capabilities` requires the existing paired-device bearer
credential over HTTPS. It returns:

```json
{
  "machineId": "<existing persistent bridgeId>",
  "desktopApiVersion": 1,
  "capabilities": ["workspace-snapshot-v1", "workspace-layout-v1", "terminal-pty-v1"]
}
```

`workspace-snapshot-v1` advertises read-only workspace snapshots/events,
`terminal-pty-v1` advertises the live terminal attach stream, and
`workspace-layout-v1` advertises the command route. The host advertises it
because `POST /api/v1/desktop/commands` really applies layout and close
commands; it must never offer a capability whose endpoints are unimplemented,
which is why a command variant with no handler is answered with
`unsupported_command` instead of being half-applied. A desktop client enables
live consoles only for a host advertising `terminal-pty-v1`, and enables
interactive remote desktop mode (drags and routed controls) only for a host
advertising both `workspace-layout-v1` and `terminal-pty-v1` with a supported
`desktopApiVersion`. Unknown optional capabilities can be
ignored. Missing endpoint, missing capability or unsupported version means the
host needs an update, not permission to fall back to snapshot terminal
emulation.

All desktop endpoints use the same auth, browser-origin rejection, credential
expiry and revocation rules as existing bridge endpoints. Security protocol
remains v3; Android endpoints and credentials are unchanged.

| Endpoint | Behavior |
| --- | --- |
| `GET /api/v1/desktop/workspace` | Complete current workspace snapshot, retrieved from the local daemon via Unix IPC. |
| `GET /api/v1/desktop/events` (WSS) | Initial complete snapshot, then changed snapshots and five-second heartbeats. |
| `GET /api/v1/desktop/terminals/<card-id>/attach` (WSS) | Live bytes of one owned card's session. Host→viewer binary frames are terminal output; viewer→host binary frames are terminal input. |
| `POST /api/v1/desktop/commands` | One typed workspace command: `setLayout`, `closeTerminal`, and the not-yet-implemented `createTerminal`/`setWorkspace`. Requires the paired-device credential; a non-POST method on the path answers 405 after authorization. |

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

### Command route

One mutation per request, in a typed envelope. The old per-card
`POST /api/v1/desktop/cards/<card-id>/position` route is gone: a move that cannot
name the revision it was based on is exactly what this route exists to refuse.

```json
{
  "requestId": "m17f2c9a1b2",
  "machineId": "<the host's bridgeId>",
  "expectedEpoch": "<epoch from the viewer's last snapshot>",
  "command": {
    "type": "setLayout", "cardId": "sd_term_ab12",
    "expectedRevision": 7,
    "layout": {"x": 700, "y": 200, "width": 640, "height": 480,
               "restoredWidth": 640, "restoredHeight": 480,
               "iconified": false, "iconX": 32, "iconY": 64, "tag": 5}
  }
}
```

`closeTerminal` takes `cardId` and `expectedRevision`. `setExpanded` takes those
and a boolean: it is the host's presentation (its maximize/minimize state), which
the snapshot already exports as `expanded`, so the viewer mirrors whatever the
host decides. `createTerminal` takes
`agentType` and `workspace`, and is accepted only when the host itself offers
that harness (it is in the snapshot's `visibleHarnesses`) and that workspace is
the folder the host itself published: a viewer never names a command, a flag, a
tmux target, an environment variable or a path the host did not publish.
`setWorkspace` is declared so its shape is fixed early and is refused with
`unsupported_command` until the host implements it.

Refusals, all with a stable code from the shared command list:

| Condition | Status | Code |
| --- | --- | --- |
| Applied | 200 | typed `applied` result with the card's new revision and geometry |
| Card changed since the viewer's snapshot | 409 | typed `conflict` result carrying the host's current revision and geometry |
| Host restarted (different epoch) | 409 | `epoch_changed` |
| The card is expanded | 409 | `terminal_expanded` |
| Unknown card | 404 | `unknown_card` |
| Malformed envelope, bounds or identity | 400 | `invalid_command`, `invalid_layout`, `unsupported_command` |
| A create names a harness the host does not offer | 400 | `unsupported_harness` |
| A layout command on a card the host shows expanded | 409 | `terminal_expanded` (a close or an expand is still accepted) |
| A create names a folder the host did not publish | 400 | `invalid_workspace` |
| Command addressed to another machine | 409 | `wrong_machine` |
| Owner unavailable / warming up / timed out | 503 / 503 / 504 | `desktop_unavailable`, `desktop_not_ready`, `desktop_timeout` |
| Same request id, owner never answered | 409 | `unknown_outcome` |

The request id is deduplicated per paired device inside one daemon epoch: 16
entries per device, 256 overall, entries from another epoch dropped. A repeated
request id replays the recorded answer and never applies the mutation twice. A
request whose owner did not answer is remembered as *uncertain*: it is not
retried automatically, and a retry is refused with `unknown_outcome` so the
viewer refreshes its state instead of guessing. Nothing about a command is
persisted across a crash, so this is deduplication, not an exactly-once claim.

The bridge validates the envelope's shape and bounds before any owner IPC, and
the owner re-checks epoch, revision and bounds before applying. Both answer with
the same code, and an owner error that is not one of our codes is reported as
`invalid_desktop_response`, never as free-form text.

### Terminal attach stream

An attach request is fully resolved **before** the WebSocket upgrade, so every
refusal is an HTTP status with a stable `error` code instead of a socket that
opens only to close: `unknown_card` (404, including a card whose session is not
an owned `sd_term_*` name), `terminal_exited` (409), `attachment_limit` (429),
`terminal_grid_unknown` (503) and the usual `desktop_unavailable` (503). The
viewer accepts only codes from the protocol's own list, never free-form text
from a peer.

After the upgrade the host streams, in order:

- one text frame `{"type":"attached","cardId":"…","columns":N,"rows":M}` with the
  host-owned grid the viewer's emulator must match;
- binary frames of raw terminal bytes, at most 16 KiB each, in stream order —
  UTF-8 characters and escape sequences may be split across frames;
- further text frames `{"type":"grid","columns":N,"rows":M}` when the host grid
  changes (for example when the host resizes its own card), plus WebSocket pings
  every 15 seconds.

The only text frame a viewer can send is
`{"type":"grid","columns":N,"rows":M}`: the host compares it with its own live
grid and applies it only when they are already equal, otherwise it answers with
the authoritative grid. Unknown fields, unknown frame types, invalid sizes and
out-of-range values are refused. **Keystrokes are binary frames**, up to 16 KiB
of raw terminal bytes, written into the host PTY. The viewer sends them only
after it has accepted `attached` for the stream it still has selected. The host
takes the per-session input guard and drops the frame when the guard is busy,
the chunk is empty or oversized, or the PTY cannot accept the write. Nothing
is buffered to replay after that. Attachments are bounded to 8 per credential
and 16 per bridge as a whole, are closed when the credential is revoked or
expires, and end after 30 minutes with a `reconnect` close frame. Dropping an
attachment reaps exactly its own tmux client on the host — never the session,
its panes or its processes.

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
- `AttachEvent` and `AttachCommand` are the attach stream's text frames, with
  `ATTACH_MAX_CHUNK`, `ATTACH_MAX_BACKLOG`, `ATTACH_MAX_SECS`, `MAX_REMOTE_VIEWERS`
  and the shared `ATTACH_REASONS` code list the protocol, the bridge and the
  viewer all agree on.

Workspace snapshot/event routes and the terminal attach route are available.
Command DTOs remain contracts without network handlers. Future arrange/raise/expand
operations and viewer input will be added alongside their implementations.

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


## PTY transport and grid ownership

`src/terminal_transport.rs` attaches an existing session to a Linux PTY,
preserving binary output and supporting bounded nonblocking input/output.
Dropping an attachment kills/reaps only that tmux client: it never creates a
harness, changes host session options, or touches another client. The bridge
exposes it as the attach route above; the viewer never gets a tmux client of its
own.

### Grid ownership without touching host sizing

A tmux *client's* terminal is the window plus the status line(s) tmux draws on
it, so `window_width`, `window_height` and the effective `status` value fully
describe the grid a viewer's emulator has to match. The host reads exactly that
and attaches at it.

That single rule is what makes a viewer unable to resize the host: under `latest`
the sizing client is the newest non-ignored one, under `largest`/`smallest` the
extremes are unchanged by a client that equals the current size, and `manual`
ignores clients entirely. So the transport admits an attachment only when the
requested grid is the host's own current client grid — a mismatch is refused as
`host_grid_mismatch` instead of attached — and it does **not** require or set
`window-size manual`, which would have broken local card resizing for every
session. `ignore-size` is additional defence for viewport changes.

Testing on tmux 3.7c found that `ignore-size` alone is insufficient: when all
attached clients have that flag, the remaining client still influences the host
grid. That is why the size rule, not the flag, is the gate. The residual case is
documented rather than hidden: if the host hides its overlay *and* has no other
client, a viewer's proxy becomes the sizing source — and because viewers only
ever apply the host's own grid, the effect is limited to the host's last known
grid.

Grid changes are pushed to viewers: the pump notices a changed host grid after
output (a host resize always redraws) and sends a new `grid` frame, so no viewer
has to poll for sizing. The proxy PTY is resized to that same grid at the same
time.

The tests prove:

- Two viewers attach at the host's 120×40 grid; a viewport change from either
  one, detach and reconnect preserve the host's shell PID and grid.
- A viewer asking for anything other than the host's own grid is refused
  without attaching and without resizing the pane.
- The client grid adds the status lines tmux draws (0 lines with `status off`,
  1 with `status on`, N with `status N`).
- Shell input, Unicode, alternate-screen application output and Ctrl+C work over
  the raw PTY connection; reconnect obtains a fresh tmux redraw.
- Real VTE renders the raw output even when bytes split escape/UTF-8 sequences.
- Unmanaged sizing and invalid session names are rejected before attachment.

`tests/desktop_terminal_smoke.py` covers the network path end to end against a
disposable bridge, a private tmux server and the real viewer CLI: pinned WSS
attach, live coloured bytes, card ownership (`unknown_card` for a foreign or
unknown card), revocation tearing the stream down, and the host session
surviving every detach. `tests/bridge_security_smoke.py` also drives the command
route against a stub owner: authorization, refused envelopes that never reach the
owner, deduplicated replay, the uncertain-outcome path, a conflict carrying the
owner's own geometry, and an owner error downgraded instead of passed on.
`machine_selector::tests::stale_poll_inner` covers the viewer's revision merge
and epoch reset.

Viewer keystrokes, the prompt-transaction input guard and the attach handshake
gate are in place: bytes VTE commits (keys, paste, IME, and mouse reports the
host application has enabled) are written after `attached` and dropped on
disconnect. Host-driven commands (`setLayout`, `setExpanded`, `closeTerminal`,
`createTerminal`) are delivered on the command route, and the buttons that send
them are the local workspace's own card and bar widgets, with the host's state
behind them. The remote folder list is still pending. Do not bypass the input
guard or replay queued keys.

## Test on another Linux desktop

After checking out this branch, use the existing development dependencies
(Rust, tmux, GTK4, VTE and a graphical session):

```bash
cargo test --bin super-desktop desktop_protocol::tests
cargo test --bin super-desktop terminal_transport::tests -- --nocapture
cargo test --bin super-desktop workspace_model::tests
cargo test --bin super-desktop peer_terminal::tests remote_terminal::tests
cargo build --bin super-desktop
python tests/bridge_security_smoke.py target/debug/super-desktop
python3 tests/desktop_terminal_smoke.py target/debug/super-desktop
./rebuild.sh --no-daemon
```

PTY tests use private tmux sockets and `/dev/null` tmux configuration, create only
their own test shell, and clean up that session. They do not attach to your
existing sessions. The VTE check runs in an isolated child process and requires
a working display; it intentionally reports failure if GTK cannot initialize.
The bridge smoke test uses temporary bridge state, credentials and an ephemeral
port and private daemon IPC stub, so snapshot tests never query your real desktop.
They exercise geometry updates, daemon-epoch changes, removals, malformed replies,
unavailability and revocation on the real TLS/WSS server. The terminal smoke
test starts its own bridge, its own tmux server under `TMUX_TMPDIR` and pairs a
real viewer through the CLI, so it never touches your sessions, credentials or
port 8759; `tests/desktop_terminal_smoke.py` takes the binary as its argument. `--no-daemon` builds without stopping the daemon, installing symlinks
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

To watch one host console from a second machine without the GUI:

```bash
super-desktop peer-attach MACHINE_ID CARD_ID --seconds 10 > console.raw
```

It writes the host's raw bytes to stdout, so a real terminal renders exactly the
host's colours; progress and the attached grid go to stderr. Without `--seconds`
it streams until the host ends the attachment.

The full suite has one known, pre-existing failure:
`tmux::tests::test_resolve_command_ai_agent_arguments` assumes Antigravity is
installed, but this PC falls back to its shell. The same failure was reproduced
on the unmodified base commit `013492a`, and `bridge::tests::test_port_taken_follows_the_listener`
is a pre-existing flake: it re-uses a released ephemeral port and loses it to a
parallel test's connection roughly once in eight full-suite runs, on the base
commit included. With terminal streaming, the command route and the remote launch
bar the suite reports 261 passed, 5 ignored and those same failures. Coverage includes
`desktop_protocol` (envelope, bounds, epoch/revision checks, typed outcome and
reply), `terminal_transport`, `peer_terminal`, `remote_terminal`, `window` and
`machine_selector`, plus the isolated terminal smoke test and the bridge smoke
test's command-route checks.

## Outgoing PC pairing (CLI increment)

Both PCs must run this branch. On the host, enable the bridge in Settings →
Android, generate an invitation and copy its pairing link. On the viewing PC:

```sh
super-desktop peer-add --host 192.168.1.20 --name "Work laptop"
# Paste the invitation at the prompt; input is hidden.
super-desktop peer-list
super-desktop peer-workspace MACHINE_ID
super-desktop peer-attach MACHINE_ID CARD_ID --seconds 15
super-desktop peer-forget MACHINE_ID
```

`--host` and `--port` override the invitation's address (useful for LAN/VPN
routing). The certificate pin still comes from the invitation. Compare the
six-digit code and approve the request on the host. Invitations are read from
stdin, never from command arguments; scripts may supply a compact JSON
invitation or the existing `superdesktop://pair?data=...` link. Treat invitations
as secrets. The viewer stores the peer only after host approval. New application
starts select This PC. After pairing, open the top-left machine selector to
display that PC's live consoles. The CLI commands themselves do not switch the
displayed workspace.

`peer-workspace` returns the host's typed layout snapshot, including card
positions, sizes and stacking order, and the card ids `peer-attach` takes.
`peer-attach` upgrades the same pinned, authenticated connection to the attach
route and prints the console's raw bytes. Piped stdin is written to the host
session as input frames; a terminal on stdin stays output-only. The host desktop
daemon must be running.
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


## Machine selector and live remote consoles

The top-left selector defaults to **This PC**. Open it to read the current peer
registry, including PCs added with `peer-add` while the application was running.
Selecting a peer displays its host workspace path, harness names and console
layout, and each visible console is a real terminal showing that host session's
live output.

Cards are the local workspace's own card widget, so they show the host's
positions, sizes, stacking order, iconified state and expanded geometry,
uniformly scaled to fit the viewer and **never enlarged**
(`s = min(1, Vw/Hw, Vh/Hh)`, as the plan specifies). The scale is applied to
positions, sizes and the terminal font, so the host's cell grid fits the card
just as it does on the host. Physical monitor scale is not applied again to the
host's logical coordinates.

Remote cards never enter local session creation: their source is a host stream,
not a session on this machine, which is the only thing about them that differs
from a local card. Their emulators forward VTE's committed bytes to the host. A
drag, a resize or a header button ends in one typed command carrying the revision
the viewer drew, so the host either applies it or refuses it with its own
geometry; a layout command for a card the host shows expanded, and any command to
a host that does not advertise `workspace-layout-v1`, are left alone. The local canvas and widgets stay alive while hidden, so switching
back to This PC restores their existing state. Local launch and arrange
controls are absent from remote mode, and Ctrl+N cannot create a local note
while a remote PC is selected.
Incoming bridge snapshots still describe this PC's local model even while it is
viewing another PC.

The viewer polls snapshots every two seconds while mapped, with one request in
flight at a time, and holds at most eight attachments: the host's frontmost
consoles get live output and the rest keep their chrome with an explanation.
Network and registry operations run outside GTK; terminal bytes reach the
emulator through a bounded queue, and a viewer that falls behind is disconnected
rather than shown a corrupted screen. Each selection has a generation number:
late responses from an earlier PC or an earlier visit to the same PC are
discarded. Disconnect, revocation and invalid layout responses clear the view and
show a status message. Reopening the window refreshes the remote snapshot, and
application restart returns to This PC.

Hiding the overlay releases the streams: the host reaps exactly those tmux
clients, every card keeps its last frame so the hide still animates, and showing
the overlay again reconnects without a failure backoff. The overlay's slide-out
moves remote consoles to their nearest border exactly like local cards, so a
remote workspace hides the way a local one does.

This increment still uses bounded HTTPS polling for layout at two-second
intervals. Live WSS workspace subscriptions, graphical peer removal, the remote
folder list and conflict feedback in the card chrome remain pending. Viewer
typing is delivered on the attach stream, and layout, expand, close and create
commands are delivered on the command route.


## Add a PC from the selector

Open the top-left selector and choose **Add a PC**. On the host, open
Settings → Android, enable the bridge, generate an invitation and expand
**Copy pairing link**. Paste that link into the viewer's form. A friendly name
and connection address/port overrides are optional. Click **Connect**, compare
the six-digit code on both PCs, and approve on the host. The approved peer is
saved and its read-only layout preview opens automatically.

The invitation field is hidden and cleared after submission or closing the
form. Network requests and private-store writes run on a worker. Only one
pairing worker can run at a time. Use Cancel/Back to stop an unfinished
attempt. The selector may transiently close while the Wayland layer surface
changes focus; the pending request continues so a host approval can save and
select the peer. A request already submitted to the host can remain pending
there: deny it on the host, and use a fresh invitation for another attempt.
Cancellation before saving claims completion prevents a late approval from
writing a peer; a save already begun is allowed to finish. Host revocation
remains authoritative if access must be withdrawn.

The existing isolated pairing smoke test can also drive the real GTK form
without opening a production window. Build tests with `cargo test --no-run`,
then supply the main test executable as a second argument:

```sh
python3 tests/peer_pairing_smoke.py target/release/super-desktop target/debug/deps/super_desktop-TEST_HASH
```

This exercises GUI approval, denial and cancellation before a late host
approval, then checks persisted peers, layout retrieval and host revocation.
