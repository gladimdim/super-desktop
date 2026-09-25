# PC-to-PC SUPER DESKTOP implementation plan

Status: pairing, a live remote workspace, its terminal transport, viewer typing
and the host's own harness bar, folder picker and card chrome are delivered, and
both workspaces now run the *same* UI code with only the source swapped, and
every remote command reports its result in the chrome, the view can switch
between Fit and 100% + pan/zoom, and the host pushes live workspace events in
place of the viewer's two-second poll. The two-PC regression matrix is
delivered as a simulated matrix on one machine (`python3 tests/two_pc_matrix.py`,
section 10); the rows that need two physical machines remain manual checks.
The README has a user guide (pairing, viewing, known limits and recovery) and
SECURITY covers the rejected-device block and the viewer's outgoing credentials.
What remains for the first release is the real two-PC walkthrough and its
measurements (section 9, phase 7). Updated 2026-09-24 against `master`.

Implementation has started: see [increment status and protocol notes](REMOTE_DESKTOP_PROTOCOL.md).
The original architecture below remains the target. Capability negotiation,
the daemon-owned local state, persisted terminal order, authenticated workspace
snapshot/event routes and the host-side PTY attach transport with a host-owned
grid are implemented. Outgoing certificate-pinned PC pairing, private peer
storage, remote snapshot retrieval and a `peer-attach` streaming CLI are testable
through the CLI, and the top-left selector renders the host's consoles live at
their own positions and sizes. Typing into a focused remote console reaches
that host session after the attach handshake, and
`POST /api/v1/desktop/commands` applies typed layout, close and create commands
with epoch/revision checks and per-device deduplication. While a remote PC is
selected its top bar offers that PC's own harness buttons, and a click launches
that harness there. Default folder selection, the viewer's own close/resize and
iconify controls, the 100% + pan/zoom mode and live workspace events
(`workspace-events-v1`) are delivered, and so is the simulated two-PC
regression matrix (section 10).

## Current delivery status

The following is present on `master` and has been rebuilt/tested between two
PCs:

- The top-left **This PC** selector defaults to the local machine and retains a
  selected remote PC while the overlay is hidden. Restarting the daemon returns
  to the local machine.
- **Add a PC** closes the selector popover and opens a centered Omarchy-styled
  wizard in the SUPER DESKTOP overlay. Its first page asks whether to view
  another PC's harnesses or make this PC's harnesses available elsewhere. The
  viewer path explains where to get a link, accepts it and shows the verification
  code. The host path (also Settings → Connections → Add a device) checks the
  bridge, firewall and network, then creates and copies a one-time link. The
  request it produces is decided in the host's connection request panel, which
  shows the device, its address and the code with Reject and Approve. Rejecting
  blocks that device until it is removed from Settings → Connections → Rejected
  devices.
- Approved, certificate-pinned peers are stored privately and appear in the
  selector. The viewer subscribes to the host's authenticated workspace events
  (or polls its snapshot, for a host without them) and draws terminal cards in their host positions, sizes, iconified positions and
  stacking order, scaled to fit the viewer canvas and never enlarged.
- Dragging a remote card's header moves and raises it on the host: the gesture
  ends with one typed `POST /api/v1/desktop/commands` (`setLayout`) that carries
  the card revision the viewer drew, so a concurrent host edit is answered with
  `conflict` and the host's own geometry instead of being overwritten. The same
  command route applies resize, iconify and `closeTerminal` on the host, driven
  by the card's own edge-resize and close/iconify controls. A
  card the host shows expanded, or a host that does not advertise
  `workspace-layout-v1`, keeps its own layout.
- **One UI, two sources.** A remote workspace is drawn by the same two widgets a
  local one is: `src/harness_bar.rs` owns the harness bar (buttons, labels,
  logos, tooltips, order and the launch line) and `src/mini_terminal.rs` owns the
  card (chrome, header buttons, drag, resize, expand and close). The only
  difference is the source they are given (`src/card_source.rs`): this machine's
  tmux and `state.json`, or a host's streamed session and its snapshot. A remote
  workspace's top bar is therefore the local one, showing the host's own harness
  list in the host's order, and its consoles carry the same buttons.
- Every remote control acts **on the host**, and the viewer mirrors the host's
  snapshot: minimize, maximize, close and resize are one typed command each
  (`setLayout`, `setExpanded`, `closeTerminal`), carrying the card revision this
  view drew, so a concurrent host edit is refused with the host's own geometry
  instead of being overwritten. A card's click-raise is local until the host's
  own stacking order changes.
- **Conflict feedback in the chrome.** Every card command's answer is turned
  into a brief, non-blocking notice by one pure decision
  (`src/command_feedback.rs`) and shown as a pill under the card's header
  (`MiniTerminalCard::show_notice`: an overlay that takes no input, never sizes
  the card, and crossfades away after 4 s, 6 s for failures, 8 s for an unknown
  outcome). A `conflict` adopts the host's revision and geometry and glides the
  card there (220 ms ease-out, skipped when GTK animations are off or the card
  is unmapped; a new gesture cancels it) with “Changed on that PC · showing its
  layout” (“· not closed” for a close). A typed refusal reverts the viewer's
  optimistic drop, explains itself (restarted, already closed, expanded on that
  PC, desktop not running, update needed, identity changed) and refreshes. A
  request that never reached the host (connect, TLS or pin failure) says
  “Cannot reach that PC · change not applied” and leaves recovery to the poll.
  A request that may have been applied — a timeout after it left, a broken
  reply, `desktop_timeout`, `unknown_outcome`, a gateway 504 — is reported as
  **“Result unknown · check before retrying”**, reverts to the last snapshot
  and refreshes; it is never replayed. `peer_client::command` tells these apart
  (`command_outcome_unknown` for anything after the connect phase) and keeps the
  bridge's stable code on 502/503/504 answers. `createTerminal` and
  `setWorkspace` report the same way in the line under the remote top bar, which
  also dismisses itself; a folder conflict says “Folder changed on that PC ·
  showing its folder”. Answers that arrive after the user switched PCs are
  dropped. The local workspace builds the same (hidden) notice and never shows
  it.
- **Fit / 100% + pan/zoom.** A two-part toggle left of Hide in the remote top
  bar switches the view. **Fit** is the default and the rule of section 7
  (`s = min(1, Vw/Hw, Vh/Hh)`, centered, never enlarged). **100%** maps one host
  logical pixel to one viewer logical pixel times a zoom (25–300%, shown on the
  button; clicking it again resets to exactly 100%). The workspace pans with
  the scrollbars, touchpad/wheel scroll, or a drag that starts on empty canvas
  (a drag on a card stays that card's gesture); Ctrl+scroll and pinch zoom
  around the pointer/fingers, and Ctrl+scroll in Fit enters 100% mode at the
  current scale. One pure transform (`remote_workspace::ViewTransform`) maps
  host ↔ canvas ↔ viewport pixels with GTK's own rounding; cards live in canvas
  pixels (`host × scale`), so pan never reaches a card gesture and every
  command (drag, edge resize, iconify, expand, close) is converted back with
  the current scale and rounded only at commit, carrying the same card
  revision and conflict feedback as before. Each card's resize bounds and
  minimum follow the current scale. Terminal fonts are refitted to the host's
  grid at each scale (VTE redraws text, nothing is bitmap-scaled), and no mode
  or zoom change sends anything to the host. The viewport requests no size, so
  neither a large host at 100% nor an off-screen card enlarges the overlay
  window, and focus never auto-scrolls it. The mode is kept per PC for the
  session only (in memory): nothing about the view is persisted, as this
  section's “never persist fitted coordinates” rule asks.
- **Live workspace events.** A host advertising `workspace-events-v1` pushes
  its workspace over pinned WSS (`GET /api/v1/desktop/events`): a complete
  snapshot on connect, another after each published change, sequenced
  heartbeats, `resync` for a subscriber that fell behind (its four-event queue
  is dropped rather than grown), a 30-minute lifetime, 4 subscriptions per
  credential and 16 per bridge (`subscription_limit`, 429), and teardown within
  a second of revocation. The host is change-driven: every state save, a local
  card's expand/collapse and title, and a changed runtime inventory bump a
  counter in the daemon; one owner-only `desktop-watch` feed wakes one hub per
  bridge, which reads one snapshot per change for all subscribers (plus one
  reconciliation read per five idle seconds). The viewer
  (`src/peer_events.rs`) subscribes while that PC is selected and on screen,
  releases on hide and on a machine switch, stands the two-second poll down
  while connected, reconnects with jittered backoff, and turns a sequence gap,
  an epoch change, a heartbeat naming a newer revision or `resync` into one
  snapshot fetch — never a replayed command. Hosts without the capability keep
  the poll. Conflict feedback is unchanged: an older snapshot cannot take back
  a revision a command's answer already adopted.
- Clicking a harness button sends a typed `createTerminal` command and the host
  builds the card exactly like a local launch — its own inventory, sandbox flags
  and folder — without being forced to show its overlay. The bar is offered only
  by a host that advertises `workspace-layout-v1`, is inert while a launch is in
  flight, and names only harnesses and folders that host published.
- The folder new harnesses start in is the local control, fed by the host: the
  snapshot publishes the folders that host offers, the viewer lists them, and
  picking one sends `setWorkspace` (judged against the workspace revision). The
  same list is the permission, so a create may name any folder in it — which is
  what lets a folder be picked and used without waiting for the next snapshot.
- Each visible remote console is a real VTE terminal fed by the host's own tmux
  attach output over pinned WSS (`GET /api/v1/desktop/terminals/<card-id>/attach`),
  so output, colours, alternate-screen applications and redraws match the host.
  The host attaches at the grid it already owns, which is what keeps a viewer
  from resizing the host's panes, and pushes `grid` frames when that grid
  changes. Attachments are bounded (8 per credential, 16 per bridge), end on
  revocation and are released when the overlay is hidden.
- The Android bridge's connection-link popover is compact. The selector only
  lists PCs; closing it does not cancel a pairing request already submitted in
  the wizard. Explicit Cancel, Close and overlay hide end the viewer's attempt.
- `./rebuild.sh` now stops both the desktop daemon and its separate
  `harness-bridge` process before restarting. This is required because an old
  bridge can otherwise keep serving port 8759 and return a 404 for the desktop
  capability route after the source has been updated.

This is a **live view you can type into, rearrange and launch from**: the same
widgets as a local workspace, with a host on the other end. It shows the host's
terminal pixels, sends keystrokes, paste and Ctrl+C to the focused session, and
its cards' own buttons, drags and edges move, resize, iconify, expand and close
the host's cards, and the folder beside its top bar lists the folders *that PC*
offers, so a new harness there can start in any of them. Each of those actions
reports its result on the card or under the bar, and the **Fit / 100%** toggle
shows the whole workspace or that PC's own pixels with pan and zoom, and changes
made on that PC appear as soon as it publishes them. A host that says “Update and rebuild SUPER DESKTOP on the host” is
serving an older bridge without `terminal-pty-v1`, and its workspace is drawn as
chrome without live consoles. Hosts that hide their overlay release their
viewers' streams, and reopening the overlay reconnects them.

### Two-PC update and smoke check

Install the same current `master` on both PCs, then run this on each PC:

```sh
git pull origin master
./rebuild.sh
```

The second command must be run after pulling `655edbb` or newer: it replaces a
stale bridge as well as the daemon. Existing approved peers remain saved. Open
the selector on the viewing PC and select the peer again; the view refreshes
at once (or within two seconds for a host without `workspace-events-v1`) and
live consoles attach immediately. A host with the current
bridge returns **401** (before a credential is supplied) for
`GET /api/v1/desktop/capabilities`, and lists `workspace-snapshot-v1` together
with `terminal-pty-v1` once a credential is supplied; an old bridge returns
**404** (or omits `terminal-pty-v1`) and must be rebuilt.

To check the transport from a shell without the GUI:

```sh
super-desktop peer-workspace MACHINE_ID | python -m json.tool   # card ids
super-desktop peer-attach MACHINE_ID CARD_ID --seconds 10 > out.raw
super-desktop peer-events MACHINE_ID --seconds 30        # one JSON event per line
echo '{"command":{"type":"setExpanded","cardId":"CARD_ID","expectedRevision":3,"expanded":true}}' \
  | super-desktop peer-command MACHINE_ID               # reply + the notice it would show
```

`peer-attach` writes raw host bytes, so running it in a terminal shows the host's
own colours; `tests/desktop_terminal_smoke.py` drives the same path
automatically, and `python3 tests/two_pc_matrix.py` runs the whole simulated
two-PC matrix (section 10) on one machine.

### Next implementation commits

1. **Delivered:** the pinned WSS terminal-attach protocol around the existing
   tmux PTY transport: capability negotiation, server-ready `attached` frame,
   binary byte framing, host-owned grid with `grid` control frames and pushes,
   per-credential attachment budgets, heartbeats, 30-minute lifetime,
   revocation teardown and bounded backpressure on both sides.
2. **Delivered:** remote VTE widgets replacing the layout preview. Visible cards
   take live streams (frontmost first, at most eight), a detached or over-budget
   card explains itself, and both hide and machine switches detach without
   touching the host session.
3. **Delivered:** `POST /api/v1/desktop/commands` backed by the host's own
   workspace, with the typed envelope (`requestId`, `machineId`,
   `expectedEpoch`, card id, `expectedRevision`), machine/epoch/revision checks,
   rejected-with-geometry conflicts, a bounded per-device request cache for
   deduplication inside one epoch, and no automatic replay after an ambiguous
   timeout. `setLayout` (move, resize, iconify), `setExpanded` (the two
   presentation modes) and `closeTerminal` are applied through the daemon's own
   card actions, `createTerminal` builds a card exactly like a local launch, and
   `workspace-layout-v1` is advertised because those handlers exist. Every caller
   is now the same UI a local workspace runs: the card's own buttons and
   gestures, the harness bar and the folder picker. `setWorkspace` (a folder from
   the host's own list, judged against the workspace revision) is applied too, so
   every command variant has a handler.
4. **Delivered:** conflict feedback in the card chrome (and under the remote top
   bar for launches and folder picks), with the host-geometry glide and the
   unknown-outcome notice described above.
5. **Delivered:** the 100% + pan/scroll mode with zoom and the fit/100%
   coordinate transforms (`remote_workspace::ViewTransform`), with the toggle
   in the remote top bar.
6. **Delivered:** live WSS workspace events in place of the two-second poll
   (`workspace-events-v1`): change-driven host hub, sequenced snapshots and
   heartbeats, `resync` backpressure, per-credential/per-bridge budgets,
   revocation teardown, and a viewer that subscribes while the PC is shown,
   resyncs by fetching, and reconnects with backoff.
7. **Delivered:** two-PC regression coverage for switching, concurrent edits,
   bridge/daemon restart, revocation, network drop and the old-host fallback,
   as `tests/two_pc_matrix.py` (section 10): isolated host and viewer instances
   on one machine, the real CLI and the real GTK selector. It found and fixed
   four bugs (see section 10). Viewer input stays behind the current-selection
   handshake and the prompt-transaction guard, and keys are never replayed
   after a disconnect.
8. **Delivered:** user and security documentation — the README guide (pairing,
   viewing, known limits, recovery) and SECURITY's rejected-device and
   outgoing-credential sections.
9. **Next:** the real two-PC walkthrough and measurements (section 10's manual
   rows and performance targets), recorded in the release PR.

## 1. Intended experience and scope

Add another PC (the requested “slave”) as a **Remote PC**. Each installation can
both host its own workspace and connect to other installations. There is no
permanent master/slave mode and no automatic reciprocal authorization.

- Put a machine selector at the far left of the top dock, before the brand and
  workspace folder. Default to **This PC · <hostname>** on every daemon startup.
- Offer paired PCs with connecting/online/offline/approval-required states, plus
  **Add PC…** and **Manage PCs…**. Remember the selection across overlay hide/show,
  but do not restore a remote selection after restarting the application.
- Selecting a PC shows that PC's terminal/harness cards, positions, sizes, tags,
  iconified state, and live interactive terminals. The folder picker and harness
  launch buttons use that PC's folders and installed harnesses.
- Typing, pasting, interrupting commands, and using terminal applications act on
  the selected PC. Creating or closing a terminal also acts on that PC.
- Dragging/resizing/iconifying a remote card updates its layout on the host PC.
  Local changes on the host appear on the viewer. Merely viewing or fitting a
  workspace to another screen must never rearrange the host's cards.
- Switching PCs leaves every harness running and restores the previous local
  workspace intact. An offline selected PC stays selected with disabled input
  and a visible disconnected state; never silently fall back to local input.

First release targets the existing Linux GTK/VTE/Hyprland application on both
machines, over LAN or Tailscale. “Desktop” means the SUPER DESKTOP workspace,
not arbitrary OS windows, a video stream, remote login, or file synchronization.
The target PC must run its daemon and bridge. Existing Android clients must keep
working without re-pairing solely because this feature was installed.

Notes are deferred in the first release: show only the selected host's terminals,
hide local notes in remote mode, and disable remote New Note with an explanation.
Do not silently create local notes while viewing a remote PC. Remote file previews
and image prompts are a follow-up; hide/disable those buttons until routed through
the authenticated host APIs. Live terminal output, viewer typing, remote create,
close and layout changes, the host's own folder list and the rest of the
viewer's command UI are delivered.

## 2. What exists and where to change it

| Existing code | Relevant behavior / implication |
| --- | --- |
| `src/window.rs` | Owns the GTK Fixed canvas, top dock, card lifecycle, geometry callbacks, and workspace controls. Currently assumes all cards are local. |
| `src/mini_terminal.rs` | VTE widgets spawn local tmux attaches; session preparation can create/resume local harnesses. Remote cards must never enter this path. Has separate detach and close-session operations. |
| `src/state.rs` | `TerminalData` persists card geometry, restored size, icon position, tag, session name and working directory. No host identity, layout revision, canvas metadata, or persisted stacking order. |
| `src/main.rs` | Daemon and Unix IPC command dispatch. Several create operations call `show_window`; remote operations need host-state access without selecting/showing the host overlay. |
| `src/bridge.rs` | Separate bridge process; TLS/WSS, persistent bridge ID, harness list, create/delete, workspaces and installed harness endpoints. `collect_harnesses` combines persisted metadata with live tmux sessions, without card geometry. |
| `src/bridge_pairing.rs`, `src/bridge_security.rs` | Invitation, host approval, token hashes, certificate identity, Unix-only administration, revocation and connection bounds. Reuse these trust rules. |
| `src/tmux_control.rs` | Phone control client uses `ignore-size,no-output`, captures styled pane snapshots and sends input. It is not a full terminal byte transport. |
| `src/ws.rs` | Small server-oriented WebSocket implementation. Its decoder requires masked client frames; it cannot be reused unchanged to read unmasked server frames as a client. |
| `src/launcher_settings.rs`, `src/workspace_bar.rs` | Pairing/settings and folder UI integration points. |
| `SECURITY.md`, `tests/bridge_security_smoke.py` | Existing security contract and isolated bridge regression coverage. |

Important details to preserve:

- The current bridge security protocol is v3 even though routes use `/api/v1`.
  Some bridge source comments still describe loopback exceptions; actual security
  uses owner-only Unix control and no network loopback authentication bypass.
- Current terminal streams send `tail`/`tailAnsi` snapshots, plus the pane's own
  `columns`/`rows` so a phone can lay that text out at the width it was rendered
  at. Feeding the tail repeatedly into VTE would duplicate text and lose
  cursor/application state.
- Current geometry uses the first monitor and minimum dimensions of 1920×1080
  in `window.rs`. Define actual logical canvas bounds before promising matching
  layouts on smaller monitors. Multi-monitor placement is not currently modeled.
- Card identity (`TerminalData.id`) and tmux session name are separate concepts;
  the existing harness API exposes the session name as its `id`.

## 3. Architecture and ownership

Use the bridge as the network boundary and the owning desktop daemon as the
authority for its local workspace. A viewer renders remote state and sends typed
commands; it never writes remote cards into its own `AppState.terminals`.

```mermaid
flowchart LR
    UI[PC A selector and card views] --> L[Local workspace backend]
    UI --> R[Remote workspace backend]
    R <-->|Pinned HTTPS / WSS| B[PC B bridge]
    B <-->|Owner-only Unix IPC| D[PC B daemon / local workspace model]
    B <-->|PTY attached tmux clients| T[PC B tmux sessions]
    D --> S[PC B state.json]
    D --> V[PC B local card views]
```

Introduce a small backend boundary for workspace snapshot/subscription, create,
close, layout updates, folder queries and terminal attach. Keep presentation
(card chrome, drag/resize, status labels) reusable, with distinct local and remote
terminal attachment implementations. Do not perform a wholesale UI rewrite.

Separate the daemon's **local workspace model** from its **selected view**. This
is essential: if B is viewing C, a request from A to B must still operate on B's
local workspace. Existing CLI commands and Android endpoints also remain bound
to their own daemon's local machine. Never proxy requests transitively to the
selected peer, and never expose outgoing peer credentials or peer catalogs.

Key every client-side object by `(machine_id, card_id)`, with an explicit mapping
to `session_name`. Use the existing persistent bridge ID for network machine
identity, bound to its certificate pin; display names and addresses are not IDs.
Use a separate Local enum variant so local operation never requires the bridge.

All network, filesystem and blocking tmux work runs off the GTK thread. Deliver
bounded model updates to GTK. Every asynchronous result carries machine identity
and a selection generation; discard stale results after a switch. Cancel pending
drags, focus, queued input and attach operations when selection changes.

## 4. Pairing, discovery, and persistence

1. On B, use the existing secure invitation UI and copy its pairing link.
2. On A, choose Add PC and paste the link. Parse and validate it without opening
   arbitrary URLs or treating a discovered address as trusted.
3. Connect with the invitation's certificate pin; allow a manual LAN/Tailscale
   address override while keeping that pin. No HTTP fallback or redirects.
4. Reuse the invitation-gated pairing request/poll flow. Show A's device name
   and verification code on both PCs; B's user explicitly approves on B.
5. Store B's bridge ID, pin, credential expiry, label and routes; add B to the
   selector. Never approve pairing through a remote terminal-management API.

Manual invitation entry is sufficient for the first release. Existing mDNS
`_omarchy-harness._tcp` discovery can later suggest routes; it must not establish
trust. Deduplicate routes by the verified bridge ID. Reject self-pairing.

Store outgoing peers separately from local workspace state and incoming paired
devices, e.g. `~/.local/state/super-desktop/peers.json`, with a 0700 directory,
0600 file, atomic replacement and no secrets in logs, process arguments, URLs,
clipboard exports, or `state.json`. Prefer a desktop secret store if available;
the first-release fallback may be a private credential file under the same-user
trust boundary, explicitly documented as unencrypted at rest. Store only metadata
in any exportable preferences. Tokens are necessarily recoverable on the client;
the server continues storing only hashes.

Distinguish **Forget on this PC** from **Revoke on the host**. Reuse the host's
registered-device revocation UI, generalized from phones to devices. Revocation
must close live layout and terminal streams and reap their tmux attach clients.
Expiry requires pairing again. A changed certificate or unexpected bridge ID
blocks connection and requires a fresh trusted invitation, never silent trust.

## 5. Proposed desktop protocol

Keep existing Android response shapes and endpoint semantics. Negotiate an
additive authenticated capability document, for example
`GET /api/v1/desktop/capabilities`, containing bridge ID, desktop API version and
capabilities `workspace-snapshot-v1`, `terminal-pty-v1` and
`workspace-layout-v1`. A 404 or missing required capability yields “Update
SUPER DESKTOP on this PC”; never pretend phone snapshots provide full desktop
support. Do not bump security protocol v3 for additive APIs. A host advertises a
capability only for behavior it actually implements: `workspace-layout-v1` is
advertised because the command route applies layout, close and create commands,
and a command whose handler does not exist is refused with `unsupported_command`
instead of being half-implemented.

Delivered endpoints (`✓`) and the ones still to build:

| Endpoint | Purpose |
| --- | --- |
| ✓ `GET /api/v1/desktop/workspace` | Authoritative local-workspace snapshot from the host daemon. |
| ✓ `GET /api/v1/desktop/events` (WSS) | `workspace-events-v1`: sequenced snapshot on connect, then one per published change (driven by the daemon's change feed, not polling), heartbeats naming epoch/revision, `resync` on backpressure, host availability. |
| ✓ `GET /api/v1/desktop/terminals/<card-id>/attach` (WSS) | Live PTY transport for one owned session: host output as binary frames, viewer keystrokes back. |
| ✓ `POST /api/v1/desktop/commands` | Typed commands with request IDs, epoch and card revisions: all five command variants are applied: `setLayout`, `setExpanded`, `closeTerminal`, `createTerminal` and `setWorkspace`. |

Reuse existing authenticated workspace/harness-type endpoints where their
semantics fit. New desktop mutations should use the command envelope so create
and close can reconcile uncertain results; preserve Android's existing endpoints.

Snapshot schema must explicitly include:

- `machineId`, fresh daemon `epoch`, monotonically increasing `revision`.
- Logical canvas origin, width, height, scale information and usable dock inset.
  First release describes the current single overlay canvas, not invented
  multi-monitor coordinates.
- Current default folder, remote display/home metadata, configured visible
  harnesses and available harness types.
- Cards with `cardId`, `sessionName`, `agentType`, title/status, workspace directory,
  tag, x/y/width/height, restored width/height, iconified/iconX/iconY, per-card
  revision and stacking order. Include tmux cell dimensions for attachments.
- Saved cards whose sessions have exited, rather than silently dropping them;
  distinguish a live session, a missing session and a deleted card. Unmanaged
  tmux sessions in the phone list have no desktop placement and are excluded.

Export purpose-built DTOs, not raw `AppState` or arbitrary launch command strings.
Initialize subscriptions with an atomic snapshot plus revision; subscribe before
collecting or retain updates so no mutation is lost between snapshot and stream.
On a revision gap, reconnect or changed epoch, discard incremental assumptions
and obtain a fresh snapshot. Full snapshots are acceptable initially if bounded
and sent only on changes; don't poll and parse `state.json` for interactive layout.
**Delivered:** the bridge registers a subscriber before reading its initial
snapshot, sends only snapshots whose epoch changed or revision grew, and numbers
every message; the viewer fetches a snapshot on a gap, an epoch change or
`resync` (see the protocol notes).

Commands carry `requestId`, `machineId`, `expectedEpoch`, target card identity and
expected card revision where applicable. Validate allowed fields and bounds.
Apply through structured Unix IPC to the owning model, with a typed success or
error reply and resulting revision. No shell command interpolation, arbitrary
tmux targets or remote access to local administrative IPC. **Delivered:**
`src/desktop_protocol.rs` owns the envelope, its bounds and the epoch/revision
checks; the bridge validates shape and bounds before any IPC and keys its
bounded deduplication cache on the paired device, the epoch and the request id;
the daemon publishes current state, re-checks, applies through the card's own
actions and publishes once more, so the revision it reports is the one it really
stored.

Serialize mutations on the host model. Persist accepted layout/lifecycle changes
before reporting them durably committed. Local gestures use the same revision
mechanism. Submit drag/resize at gesture end (optional throttled previews later);
reject a stale edit with conflict and current geometry instead of overwriting a
concurrent edit. Closing a card wins over an in-flight drag.

Use a bounded per-device request-result cache for mutation deduplication within
one daemon epoch (**delivered:** 16 per device, 256 overall, entries pruned when
the epoch changes). Never automatically replay create/close after an epoch change
or an ambiguous timeout; refresh state and show an uncertain outcome
(**delivered:** a request whose owner never answered is remembered as uncertain
and its retry is refused with `unknown_outcome` rather than applied again).
Persisting deduplication across crashes is a later improvement, not an
exactly-once claim. Terminal keystrokes are never replayed after disconnect.

## 6. Full interactive terminal transport — output and viewer typing delivered

Delivered: the host creates a **dedicated tmux attach client on a server-side
PTY** per remote view and bridges its bytes over authenticated WSS into the
viewer's own VTE widget. The viewer has no proxy helper process and no tmux
client; it is a pure emulator fed by the network. The `peer-attach` CLI uses the
same path, which is what makes the transport testable without a GUI.

A versioned attach handshake, server-ready `attached` frame, binary data frames,
`grid` control frames, close reasons, 15-second heartbeats, a 30-minute stream
lifetime and a shared stable reason-code list are implemented. Attaching tmux
produces a full initial redraw; a reconnect is a new attach and a fresh redraw,
never replayed old output. TERM/COLORTERM match the local cards, and the viewer
paints the raw bytes, so Unicode, escape sequences, alternate screen and colour
come from tmux exactly as they do locally.

**Sizing policy (implemented):** the host owns the session's terminal cell grid.
The bridge reads the host's own client grid — window size plus the status lines
tmux draws — and admits an attachment only at that exact size, which no
`window-size` policy can change and which leaves local card resizing untouched
(`ignore-size` is kept as defence for the viewer's own viewport). The viewer's
VTE is matched to the host grid, and the host pushes a `grid` frame when that grid
changes, including when no local client is attached. Viewer fitting changes
presentation only and never writes layout back. The residual case — a host with
no client of its own whose only remaining client is a viewer — is documented
rather than hidden, and can only pin the host to a grid the host itself reported.

**Viewer input is delivered.** Binary frames from the viewer are raw terminal
bytes. They are sent only after the `attached` handshake for the stream that
is still selected, written behind the prompt-transaction input guard, and
dropped (never replayed) when that guard is busy, the PTY cannot accept them,
or the viewer disconnects, hides or switches machines.

Terminals are shared sessions. Concurrent local/remote/phone input may interleave;
show remote connection presence and document shared control. An exclusive control
lease is deferred. Preserve the image-prompt input guard: raw terminal input must
not bypass an in-progress guarded prompt transaction. Centralize per-session
remote input arbitration; never buffer user keys to replay later after a busy
transaction or disconnected transport.

Disconnect/switch/hide destroys only that view's attach client and proxy, never
the session or harness process. Explicit Close is the separate host operation.
The existing “one tmux client per card” invariant becomes one owning local client
plus bounded remote view clients; verify `detach-on-destroy` behavior and ensure
no attach uses a flag that detaches other users.

Use a client-capable HTTP/WSS implementation with rustls pin verification, server
frame decoding and client masking; do not invert the existing server decoder.
Prefer a maintained library over expanding custom framing. Retain TLS signature
verification in any pin verifier and preserve server authorization on every
stream. Implemented as `tungstenite` (framing and the upgrade handshake) over the
existing pinned `rustls` client config, so the invitation's pin replaces CA and
DNS-name trust while handshake signatures are still verified; the server keeps
its own decoder.

Keep the existing 64 global / 12 per-source connection limits initially. Budget
one workspace stream, at most eight attached remote terminals per selected peer,
and room for commands/pairing/phone traffic. Prioritize focused/visible cards;
show unattached previews with click-to-attach when over budget. Apply bounded
byte queues (initially 1 MiB per attachment), frame chunks within existing 16 KiB
limits, timeouts, backpressure and explicit overflow disconnect. Never drop
arbitrary terminal bytes and continue with a corrupted display. Reuse or extend
revocation checks to cover PTY forwarding and process teardown.

## 7. Layout fidelity and screen differences

Host coordinates are authoritative **logical** pixels, not physical display
pixels. On equal usable canvas sizes, render the same rectangles and order.
Persist stacking order with migration defaults. Publish current expanded state
if available, but keep it distinct from saved/restored geometry: expansion is
currently transient and must not accidentally overwrite the saved card rectangle.

Default remote view to **Fit workspace**. For host usable rectangle `(hx,hy,Hw,Hh)`
and viewer usable rectangle `(vx,vy,Vw,Vh)`, use
`s = min(1, Vw/Hw, Vh/Hh)` and centered offsets. Display a host point as
`(vx + (Vw-s*Hw)/2 + s*(x-hx), vy + (Vh-s*Hh)/2 + s*(y-hy))`.
Apply the same transform to sizes and hit-testing; invert it for user drags before
sending integer host coordinates. Round only at commit and enforce bounds on the
host. Account for dock space once, not separately in each widget.

GTK/VTE font/layout scaling may prevent exact pixel equivalence. Provide a
**100% + pan/scroll** mode when fitted terminals become unreadable or cannot
preserve the host grid (delivered, with zoom: see “Current delivery status”).
Test the transform against actual GTK allocation and input coordinates (the
`remote_terminal` pan/zoom regression does, through a mapped window). Never persist fitted coordinates, silently auto-arrange, or
resize the host because the viewer monitor changes. Use the viewer's theme for
the first release; matching font metrics and colors exactly is not guaranteed.
Multi-monitor topology mirroring is deferred; do not add meaningless monitor IDs
without implementing their coordinate semantics.

## 8. UI actions, lifecycle, and failure behavior

Audit every toolbar button, keyboard shortcut, card callback and background job:

| Action | While a remote PC is selected |
| --- | --- |
| New terminal / Ctrl+T / harness buttons | Create on selected host using its installed harnesses and validated folder. |
| Folder picker/search/history | Query/validate/remember on selected host. Never probe its path on the viewer filesystem or expand `~` locally. |
| Drag, resize, tag, iconify, expand, arrange | Route to host model with appropriate revision; arrange only supported terminal cards in remote mode. |
| Close card | Explicitly close on named remote host; never run a local `kill_session`. |
| Esc, toggle, hot corner | Control this viewer's overlay. |
| Settings / shortcut / sleep lock / theme | Remain clearly labeled local settings. Peer management is local; do not expose remote OS settings. |
| Notes / Files / image attachment | Disabled/hidden until their remote implementation exists. |

Selecting a host freezes outgoing input before detaching old views, increments
the selection generation, cancels old requests and populates the new snapshot.
Restore local views without recreating sessions. Outbound selection never changes
what this machine serves to its own incoming clients.

On disconnect, disable input immediately, mark snapshots stale, cancel pending
gestures, and retain the selected host label. Reconnect with bounded exponential
backoff and jitter, verify identity, refresh capabilities/snapshot and attach anew.
Do not reconnect automatically after revocation, expiry, pin mismatch or explicit
Forget. A sleeping PC is offline; wake-on-LAN and relay/NAT traversal are deferred.

When the viewer overlay is hidden, release its terminal streams and retain at
most lightweight workspace/status monitoring. Inactive peers use bounded,
low-frequency checks rather than terminal streams; cap monitoring concurrency.
The local workspace model continues serving incoming clients when hidden or
while viewing another machine. If the host daemon is down but its bridge lives,
return a distinct `desktop_unavailable`, not an empty workspace.

## 9. Implementation sequence and handoff boundaries

Each phase should be a separately reviewable change. All phases through 7 are
required for the first release; follow-ups in section 1 are separate work.

1. **Contracts and terminal feasibility — delivered.** Define DTOs, identity, IPC contracts,
   capability schema and errors in a proposed `src/desktop_protocol.rs`. Build
   an isolated PTY/WSS/VTE proof with one shell and one full-screen terminal app.
   Prove grid ownership, mouse/paste, redraw/reconnect, revocation and clean
   detach with two viewers. Resolve technical failures before building the UI.
2. **Local model boundary — partially delivered.** Extract authoritative local workspace operations
   from `window.rs` into a proposed `src/workspace_model.rs`; add revisions,
   canvas metadata, stacking migration and structured IPC in `main.rs`. Keep
   local UI behavior and CLI/Android targeting intact. Ensure create/resize works
   while hidden without forcibly showing the host overlay.
3. **Desktop bridge APIs — snapshots/events, the PTY attach route and the command route delivered.** `src/desktop_bridge.rs`
   serves snapshot/events, `terminals/<card-id>/attach` and
   `POST /api/v1/desktop/commands`, reusing bridge authentication, limits, the
   shared reason-code list and owner IPC, and advertising `terminal-pty-v1` and
   `workspace-layout-v1`. `src/terminal_transport.rs` owns the grid rule; the
   command route owns envelope validation and per-device deduplication, and the
   daemon re-checks epoch, revision and bounds before applying anything. The
   per-card `cards/<id>/position` route it replaced is gone.
4. **Peer client and registry — delivered for pairing, snapshots and the attach stream.** `src/peer_client.rs` and
   `src/peer_store.rs` handle invitation pairing, pinned HTTPS/WSS (shared pin
   verifier, no proxies, no redirects), private credentials, version checks,
   cancellation and reconnect, and `src/peer_terminal.rs` carries live terminal
   bytes to the viewer with a bounded queue. Tested without GTK, both in unit
   tests and through the `peer-attach` CLI.
5. **Selector and remote card view — delivered as live consoles.** `src/machine_selector.rs` owns the
   selector, the pairing form, disconnect states and guarded asynchronous
   callbacks; `src/remote_terminal.rs` renders each host card as chrome plus a
   VTE fed by the network stream, forwards that VTE's committed bytes to the
   host, keeps host geometry and stacking, budgets the attachments and never
   touches local session preparation.
6. **Layout and simultaneous use — delivered; one manual check remains.** Host-owned grids,
   stream budgeting, the inverse drag transform, revision conflicts with the
   host's own geometry, the shared bar and card widgets, every routed
   move/resize/iconify/expand/close control, conflict feedback in the chrome
   and the 100% + pan/scroll mode are delivered. Still to run on real machines:
   B views C while A operates B, and A/B view each other without recursion or
   exported peer state (section 10).
7. **Regression, documentation and release — matrix and documentation delivered; next.** The
   automated two-PC matrix is `tests/two_pc_matrix.py`; the manual rows it
   cannot reach are listed in section 10. README has the user guide (pairing,
   viewing, known limits and a recovery table) and SECURITY covers the new
   routes, the rejected-device block, outgoing credentials and resource limits.
   Remaining: an actual two-PC walkthrough and the measured performance results
   of section 10 (p95 echo latency, idle CPU/network for 1/4/8 consoles,
   bounded memory/descriptors/attach clients after repeated reconnects).

Suggested file names are boundaries, not a requirement to create empty modules.
Before each phase, inspect the preceding implementation and tests; do not treat
this document's proposed APIs as already existing.

## 10. Verification and release acceptance

Automated coverage must test behavior across boundaries, especially:

- Legacy `state.json` loads unchanged; peer state never pollutes local cards;
  identities remain unique when two PCs have equal card IDs/session names.
- Pinned TLS, expired invitations, denial, duplicate routes, self-pairing,
  token expiry, changed certificates, immediate revocation, auth on every new
  endpoint, malformed frames and bounded queues. Isolate test state/ports.
- Snapshot/subscription race, gaps and epoch changes; concurrent move/close;
  deduplicated create; uncertain mutations never blindly retried. A slow poll
  cannot take back a revision the viewer already adopted. Delivered coverage:
  `desktop_protocol` (envelope, bounds, epoch/revision/conflict checks, typed
  outcome and reply), `bridge_security_smoke` (route auth, refused envelopes
  before owner IPC, deduplicated replay, uncertain outcome, conflicts with the
  owner's geometry, free-form owner errors downgraded) and
  `machine_selector::tests::stale_poll_inner` (revision merge and epoch reset).
  Event coverage: `bridge::desktop_events::tests` (in-process handler: a
  layout change reaches a subscribed viewer without polling; budgets,
  backpressure, heartbeats, lifetime, revoked socket), `peer_events::tests`
  (sequence gaps, epoch changes, stale reads, validation, backoff),
  `machine_selector::tests::live_events_inner`, and the two smoke tests.
- Terminal UTF-8 across frame boundaries, alternate screen, color, reconnect
  redraw, size changes, slow reader and abrupt disconnect. Session PID survives
  detach/switch/hide. Delivered coverage: `terminal_transport` (grid rule, two
  viewers, raw Unicode/control/alternate-screen, VTE rendering),
  `peer_terminal` (typed frames, misrouted cards, bounded delivery),
  `remote_terminal` (geometry, stream budgets, suspend/resume),
  `desktop_protocol` (capability and frame schema) and
  `tests/desktop_terminal_smoke.py` (real bridge, private tmux server, real
  viewer CLI: pinned attach, live coloured bytes, card ownership, revocation
  teardown, host session survival, and piped stdin reaching the host shell).
  VTE commit forwarding covers paste, IME and the control bytes VTE emits,
  including an interior NUL. Mouse tracking follows whatever the host
  application has enabled, because those reports are commit bytes too.
- Stale results after A→B→A cannot attach to, close, resize or send input to the
  wrong host. Incoming bridge and local CLI stay local regardless of selection.
- Geometry round trips on equal/different aspect ratios, fractional display
  scales, small screens, icon positions, restored dimensions and docking insets.

Manual acceptance matrix:

| Scenario | Required result |
| --- | --- |
| Fresh start and upgrade | This PC selected; original local cards and behavior intact. |
| Pair A→B and A→C | Both PCs selectable with verified identities; reverse access needs separate pairing. |
| Equal-sized displays | Host positions, card sizes, icon state and order match. |
| Different sizes/scales | Fit or 100% view is usable; all cards reachable; viewing never writes layout back. |
| Shell + installed harness + full-screen app | Live console output, colour and full-screen redraws match the host; typing, paste and Ctrl+C reach the focused session. |
| Launch a harness from the viewer's top bar | The buttons are the host's own list, in the host's order, and the click creates the card on the host in the host's published folder. A harness the host does not offer is not listed, and an older host offers no buttons at all. |
| Drag/resize from either machine | Other view updates; a stale gesture is refused as a conflict and the card glides to the host's real geometry with “Changed on that PC · showing its layout”; no resizing oscillation. |
| Unplug B's network mid-command | A's card says “Cannot reach that PC” (not sent) or “Result unknown · check before retrying” (sent, no answer), reverts to the last snapshot, and nothing is resent. |
| Close on B from A | B owns the lifecycle: its card, widget and session go, and A's local state is unchanged. |
| Create on B from A | B owns the lifecycle: the card appears in B's workspace and the chosen folder, A's local state is unchanged, and B's overlay is not forced to show. |
| Pick a folder from A, then launch | The list is B's own folders; the pick changes B's working folder (visible on B's own bar) and the next launch starts there. |
| Rapid switching and hide/show | No keys reach the wrong PC and no harness is killed. |
| B asleep, bridge restart, daemon restart | Clear stale/offline state; safe refresh; no duplicate commands, and every console says why it has no live stream. |
| Android plus desktop viewers | Existing phone pairing/list/input/files remain compatible. |
| B views C while A views B | A sees and controls B's local workspace, never C's. |
| Revoke A on B | Streams close promptly; A cannot keep typing; B's sessions survive. |

### Two-PC regression matrix (delivered, simulated on one machine)

```sh
python3 tests/two_pc_matrix.py            # builds, then runs everything (~85 s)
python3 tests/two_pc_matrix.py --no-gui   # CLI rows only, no display (~65 s)
python3 tests/two_pc_matrix.py target/debug/super-desktop target/debug/deps/super_desktop-HASH
```

Only one physical PC is needed. The script starts three isolated **hosts**
(A, B and C) and three **viewers**, all on this machine:

- Each host has its own bridge state (machine id, TLS certificate, paired
  devices), HOME/XDG directories, owner IPC socket, ephemeral bridge port and a
  private tmux server (`TMUX_TMPDIR`, `$TMUX` dropped; the bridge uses tmux's
  default socket name, so a per-host `TMUX_TMPDIR` is the `tmux -L`
  equivalent). Hosts have no D-Bus or display, so pairing notifications never
  reach the desktop.
- Each viewer has its own peer registry and pairs through the real invitation,
  code and approval flow, from its own loopback source address (a bridge
  admits one pending pairing per source, as it would per PC).
- The host *daemon* is `HostOwner`, a stateful model of the owner IPC contract
  (epoch per lifetime, growing revisions, `desktop_protocol::check_command`'s
  refusals, conflicts carrying the owner's geometry, the `desktop-watch` feed
  with `idle` lines, and real tmux sessions for created cards). Everything else
  is the shipped code: bridge routes, TLS/pin, credentials and revocation,
  deduplication, the event hub, the PTY attach transport, `peer_client`, the
  event worker, `command_feedback`, and — in the GTK phase — `MachineView` and
  `RemoteCanvas` in a real window.
- A TCP hop (`Proxy`) sits between viewer 1 and host A to refuse, reset,
  freeze or cut the link, including "applied on the host, reply lost". An
  old host is a TLS-terminating front (`LegacyHost`) on host C that presents
  C's own certificate, strips `workspace-events-v1` and answers 404 on
  `/events`, exactly like a bridge that predates it.
- `super-desktop peer-command MACHINE_ID < command.json` (new) sends one typed
  command through `peer_client::command` and prints the host's reply together
  with the notice, geometry and refresh decision the card chrome would show.
  It never retries. The input is `{"command": {...}}`, optionally with
  `expectedEpoch` and `requestId`.

Every process is stopped by its recorded PID and every directory is a private
temp dir under `/tmp` (Unix socket paths must stay short). The user's daemon,
bridge on port 8759, default tmux server, peers and `~/.config/super-desktop`
are never touched. The GTK phase opens one ordinary test window for about
twenty seconds; it needs a display, like the other GTK child-process tests.

| Row | What the simulated matrix checks |
| --- | --- |
| Pair A→B and A→C | Both hosts listed with verified, distinct identities; a credential for one host is 401 on the other; `peer-list` never prints a credential. |
| Switching / no cross-host leakage | A→B→A attaches carry only that host's bytes and release their tmux clients; B's card id is `unknown_card` on A (attach and command, “Already closed on that PC”); typed bytes reach only the attached host. GTK: A's cards leave in the same frame as the switch, A's tmux clients and sockets are released within ~0.2 s, a burst A→B→A→B→A settles on A with B never drawn, keys typed into B's console reach B's pane and none of A's. |
| Concurrent edits | Host move pushed as an event; a viewer drop at the old revision is a conflict with the host's geometry (“Changed on that PC · showing its layout”, snap to host). Two viewers racing on one revision: exactly one applied, the loser gets the winner's geometry (3 rounds). Close vs move and two folder picks settle with one winner and the right notice. A repeated create request id makes one card. GTK: the real drag commit after a host move shows the conflict notice and glides to the host's rectangle. |
| Bridge restart | Events notice at once, attach ends, commands say “Cannot reach that PC” (nothing sent), the bridge returns with the same identity, pin and credential, the subscription resubscribes within about a second, nothing is replayed. A request id replayed after a restart (the bridge's dedup cache is gone) is still refused by the owner's revision check. GTK: subscription and every console back within ~6 s. |
| Daemon restart | The stream reports `unavailable`, commands say “desktop is not running”, the same subscription then pushes the new epoch, old-epoch commands are `epoch_changed` (“That PC restarted”), the new daemon receives no command, sessions survive. GTK: new epoch drawn and consoles streaming within ~1 s. |
| Network drop | Refused or reset link → “Cannot reach that PC · change not applied”, no refresh, host untouched. Reply cut after the host applied → “Result unknown · check before retrying”, applied exactly once and never resent; a manual retry while the first is in flight is `unknown_outcome`, afterwards it replays the recorded answer. Frozen link → the same after the 8 s client timeout. Events back off 1 s → 2 s → 4 s (jittered), recover with the change made meanwhile, and a silent drop is detected by three missed heartbeats (~15 s). |
| Revocation | Attach and events end within a second, the subscription exits with `peer_revoked_or_expired` instead of retrying, capabilities/workspace/commands/attach/events all answer 401, the host reaps the tmux clients, other viewers and host sessions are untouched. GTK: “Pairing required” and a cleared view within ~2 s. |
| Old host | Missing capability → `peer-events` ends with `update_remote_super_desktop` without touching `/events`; snapshots and consoles still work. GTK: no subscription, a host move is drawn by the two-second poll. |
| Hide/show | GTK: hiding releases every tmux client and socket within ~0.2 s; showing re-attaches. |

Bugs this matrix found, each fixed with a focused change and its own unit
test:

1. **Dropped consoles never reconnected while events were live.** A console
   whose stream ended (a network blip, the host reaping its tmux client) was
   retried only by the next snapshot drawn, and a quiet host with a live
   subscription sends none, so the card said “Reconnecting…” until something
   changed on the host. The selector's two-second tick now asks the canvas to
   re-attach (`RemoteCanvas::retry_streams`, local only, after the session's
   own 5 s backoff) while the subscription is live.
2. **A typed `unknown_card` answer read as “Cannot reach that PC”.** The bridge
   answers a command for a card that is already gone with 404 and a typed
   `rejected` result; the viewer treated every 404 as a missing endpoint, so a
   close that lost a race said “Cannot reach” and skipped the refresh. A 404
   carrying a typed result is now the host's answer (“Already closed on that
   PC”); a 404 without one is still an endpoint the host does not have.
3. **Long folder paths were cut off at the daemon.** The daemon read each IPC
   command with one 1 KiB read, so a `createTerminal` or `setWorkspace` whose
   folder path pushed the envelope past 1 KiB (paths may be 4 KiB) was
   truncated and refused as `invalid_command` (“Update SUPER DESKTOP”). It now
   reads to the newline, bounded at 16 KiB, with a 2 s stall limit. (The
   matrix's owner is a model, so this is covered by `ipc_tests`, not by the
   matrix itself.)
4. **Concurrent registry readers failed instead of waiting.** The peer
   registry lock was non-blocking, so the selector's refresh, the settings
   page and any `peer-*` command that overlapped for a few milliseconds got
   `peer_store_busy`, which the selector showed as a failed connection and a
   cleared remote view. The lock now waits up to one second.

Still manual (they need two physical machines, or a real daemon):

- A real host daemon and its GTK workspace under the commands (the owner is a
  model here): host-side overlay behaviour, card widgets moving on the host,
  `state.json` persistence, and the long-path fix end to end.
- A real network: Wi-Fi/Ethernet unplug, sleep/wake, NAT/VPN routing, MTU and
  latency; p95 echo latency and idle CPU/network for 1/4/8 consoles.
- Equal and different display sizes/scales between two real monitors,
  fractional scaling and docking insets.
- Installed harnesses and full-screen applications on the host, and Android
  plus desktop viewers on the same host.
- B views C while A views B (the matrix never runs a viewer and a host in one
  instance), and the fresh-start/upgrade rows.

Run `cargo test --bin super-desktop`, isolated bridge smoke tests (extend
`tests/bridge_security_smoke.py` or add a desktop-specific companion), and the
repository-prescribed `./rebuild.sh --no-daemon` for build-only validation. Use
`./rebuild.sh` only when deliberately installing/restarting for manual testing;
do not disturb live sessions to execute automated tests.

Record input-to-display latency and idle CPU/network usage for one, four and
eight visible terminals, plus repeated switch/reconnect cycles.
`tests/two_pc_measure.py` automates the transport part between two real PCs:
`sample` on the host, `viewer MACHINE_ID` on the viewer (it creates, types
into and closes its own Shell cards on the host), then `report` for a markdown
summary for the release PR. It times keystroke echo through the real pinned
attach path, counts only the TCP traffic between the two PCs, and checks the
host bridge's memory, descriptors, threads and tmux clients across repeated
attach/detach cycles. It uses the CLI viewer, so the viewer's GTK rendering
cost is measured separately (`sample` on the viewer while the overlay shows
the host). `simulate` validates the tool itself on one machine. Initial LAN
target: p95 echo latency below 100 ms on an unloaded wired network; report hardware
and conditions rather than treating this as an Internet guarantee. Require bounded
memory, file descriptors and attach-client counts after repeated disconnects, and
no regression in local overlay responsiveness with every peer offline.

The feature is complete when a user can pair two PCs, select either workspace,
recognize its layout, work interactively in its existing harnesses, and return
to local work without losing sessions or sending actions to the wrong machine.
