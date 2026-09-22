# PC-to-PC SUPER DESKTOP implementation plan

Status: pairing, a live read-only remote workspace and its terminal transport are
delivered; the interactive remote-desktop release remains in progress.
Updated 2026-09-22 against `master`.

Implementation has started: see [increment status and protocol notes](REMOTE_DESKTOP_PROTOCOL.md).
The original architecture below remains the target. Capability negotiation,
the daemon-owned local state, persisted terminal order, authenticated workspace
snapshot/event routes and the host-side PTY attach transport with a host-owned
grid are implemented. Outgoing certificate-pinned PC pairing, private peer
storage, remote snapshot retrieval and a `peer-attach` streaming CLI are testable
through the CLI, and the top-left selector renders the host's consoles live at
their own positions and sizes. Lifecycle mutation extraction, remote commands,
live outgoing workspace subscriptions and viewer input remain pending.

## Current delivery status

The following is present on `master` and has been rebuilt/tested between two
PCs:

- The top-left **This PC** selector defaults to the local machine and retains a
  selected remote PC while the overlay is hidden. Restarting the daemon returns
  to the local machine.
- **Add a PC** supports both directions in one Omarchy-styled panel: create and
  copy a one-time connection link to share this PC, or paste a link created on
  the other PC to connect this PC. Approval and the verification code still
  happen on the host PC.
- Approved, certificate-pinned peers are stored privately and appear in the
  selector. The viewer polls the host's authenticated workspace snapshot and
  draws terminal cards in their host positions, sizes, iconified positions and
  stacking order, scaled to fit the viewer canvas and never enlarged.
- Each visible remote console is a real VTE terminal fed by the host's own tmux
  attach output over pinned WSS (`GET /api/v1/desktop/terminals/<card-id>/attach`),
  so output, colours, alternate-screen applications and redraws match the host.
  The host attaches at the grid it already owns, which is what keeps a viewer
  from resizing the host's panes, and pushes `grid` frames when that grid
  changes. Attachments are bounded (8 per credential, 16 per bridge), end on
  revocation and are released when the overlay is hidden.
- The Android bridge's connection-link popover is compact. Closing the
  selector popover no longer cancels an already submitted pairing request.
- `./rebuild.sh` now stops both the desktop daemon and its separate
  `harness-bridge` process before restarting. This is required because an old
  bridge can otherwise keep serving port 8759 and return a 404 for the desktop
  capability route after the source has been updated.

This is a **read-only live view**, not the finished interactive feature. It shows
the host's terminal pixels but accepts no input, creates or closes no cards and
writes no card geometry on the selected host; the transport has no input frame at
all. A host that says “Update and rebuild SUPER DESKTOP on the host” is serving an
older bridge without `terminal-pty-v1`, and its workspace is drawn as chrome
without live consoles. Hosts that hide their overlay release their viewers'
streams, and reopening the overlay reconnects them.

### Two-PC update and smoke check

Install the same current `master` on both PCs, then run this on each PC:

```sh
git pull origin master
./rebuild.sh
```

The second command must be run after pulling `655edbb` or newer: it replaces a
stale bridge as well as the daemon. Existing approved peers remain saved. Open
the selector on the viewing PC and select the peer again; the view refreshes
within two seconds and live consoles attach immediately. A host with the current
bridge returns **401** (before a credential is supplied) for
`GET /api/v1/desktop/capabilities`, and lists `workspace-snapshot-v1` together
with `terminal-pty-v1` once a credential is supplied; an old bridge returns
**404** (or omits `terminal-pty-v1`) and must be rebuilt.

To check the transport from a shell without the GUI:

```sh
super-desktop peer-workspace MACHINE_ID | python -m json.tool   # card ids
super-desktop peer-attach MACHINE_ID CARD_ID --seconds 10 > out.raw
```

`peer-attach` writes raw host bytes, so running it in a terminal shows the host's
own colours; `tests/desktop_terminal_smoke.py` drives the same path
automatically.

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
3. Next: authenticated `POST /api/v1/desktop/commands` handlers backed by the
   host's workspace model. Start with card layout/iconify and close operations;
   enforce machine/epoch/card revisions and request deduplication, then cover
   creation, harness inventory and default folders. Advertise
   `workspace-layout-v1` only when those handlers exist.
4. Next: viewer input. Send it only after a current-selection handshake, keep
   the prompt-transaction guard, add the input arbitration the plan requires,
   and never replay keys after a disconnect. Then route remote card gestures and
   toolbar actions through the command API, add the 100% + pan/scroll mode,
   implement fit/100% coordinate transforms, conflict feedback and live WSS
   workspace events. Add two-PC regression coverage for switching, concurrent
   edits, bridge/daemon restart and revocation.

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
the authenticated host APIs. Live terminal output is delivered, but full
interaction is still required for release: a read-only stream does not complete
this feature.

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
- Current terminal streams send `tail`/`tailAnsi` snapshots. Feeding these
  repeatedly into VTE would duplicate text and lose cursor/application state.
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
capabilities `workspace-snapshot-v1`, `terminal-pty-v1` and (once mutations
exist) `workspace-layout-v1`. A 404 or missing required capability yields “Update
SUPER DESKTOP on this PC”; never pretend phone snapshots provide full desktop
support. Do not bump security protocol v3 for additive APIs. A host advertises a
capability only for behavior it actually implements, so `workspace-layout-v1`
stays off until the command route exists.

Delivered endpoints (`✓`) and the ones still to build:

| Endpoint | Purpose |
| --- | --- |
| ✓ `GET /api/v1/desktop/workspace` | Authoritative local-workspace snapshot from the host daemon. |
| ✓ `GET /api/v1/desktop/events` (WSS) | Initial snapshot, then changed snapshots and host availability. |
| ✓ `GET /api/v1/desktop/terminals/<card-id>/attach` (WSS) | Live PTY transport for one owned session; currently host→viewer output only. |
| `POST /api/v1/desktop/commands` | Typed create, close, layout/tag/iconify and default-folder commands, with request IDs and revisions. |

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

Commands carry `requestId`, `machineId`, `expectedEpoch`, target card identity and
expected card revision where applicable. Validate allowed fields and bounds.
Apply through structured Unix IPC to the owning model, with a typed success or
error reply and resulting revision. No shell command interpolation, arbitrary
tmux targets or remote access to local administrative IPC.

Serialize mutations on the host model. Persist accepted layout/lifecycle changes
before reporting them durably committed. Local gestures use the same revision
mechanism. Submit drag/resize at gesture end (optional throttled previews later);
reject a stale edit with conflict and current geometry instead of overwriting a
concurrent edit. Closing a card wins over an in-flight drag.

Use a bounded per-device request-result cache for mutation deduplication within
one daemon epoch. Never automatically replay create/close after an epoch change
or an ambiguous timeout; refresh state and show an uncertain outcome. Persisting
deduplication across crashes is a later improvement, not an exactly-once claim.
Terminal keystrokes are never replayed after disconnect.

## 6. Full interactive terminal transport — delivered as a read-only stream

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

**Not yet implemented:** viewer input. The transport has no input frame, binary
frames from a viewer are ignored, and the plan's requirements for input still
gate the interactive release: send input only after a ready handshake for the
currently selected machine, keep the prompt-transaction guard, arbitrate
per-session input, and never replay keys after a disconnect or switch.

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
preserve the host grid. Test the transform against actual GTK allocation and
input coordinates. Never persist fitted coordinates, silently auto-arrange, or
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
3. **Desktop bridge APIs — snapshots/events and the PTY attach route delivered; mutations pending.** `src/desktop_bridge.rs`
   serves snapshot/events plus `terminals/<card-id>/attach`, reusing bridge
   authentication, limits, the shared reason-code list and owner IPC, and
   advertising `terminal-pty-v1`. `src/terminal_transport.rs` owns the grid rule.
   `POST /api/v1/desktop/commands` with epoch reconciliation and mutation
   deduplication is still pending, and stays unadvertised until it exists.
4. **Peer client and registry — delivered for pairing, snapshots and the attach stream.** `src/peer_client.rs` and
   `src/peer_store.rs` handle invitation pairing, pinned HTTPS/WSS (shared pin
   verifier, no proxies, no redirects), private credentials, version checks,
   cancellation and reconnect, and `src/peer_terminal.rs` carries live terminal
   bytes to the viewer with a bounded queue. Tested without GTK, both in unit
   tests and through the `peer-attach` CLI.
5. **Selector and remote card view — delivered as live read-only consoles.** `src/machine_selector.rs` owns the
   selector, the pairing form, disconnect states and guarded asynchronous
   callbacks; `src/remote_terminal.rs` renders each host card as chrome plus a
   read-only VTE fed by the network stream, keeps host geometry and stacking,
   budgets the attachments and never touches local session preparation.
6. **Layout and simultaneous use — partially delivered; next UI milestone.** Host-owned grids and
   stream budgeting are delivered. Fit/100% modes, the inverse drag transform,
   revision conflicts and routed remote controls remain, together with a two-PC
   check that B can view C while A operates B, and that A/B can view each other
   without recursion or exported peer state.
7. **Regression, documentation and release — ongoing.** Complete the matrix below, update
   README and SECURITY (device terminology, outgoing credentials, new routes,
   resource limits), and document known limits and recovery. Include an actual
   two-PC walkthrough and measured performance results in the implementation PR.

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
  deduplicated create; uncertain mutations never blindly retried.
- Terminal UTF-8 across frame boundaries, alternate screen, color, reconnect
  redraw, size changes, slow reader and abrupt disconnect. Session PID survives
  detach/switch/hide. Delivered coverage: `terminal_transport` (grid rule, two
  viewers, raw Unicode/control/alternate-screen, VTE rendering),
  `peer_terminal` (typed frames, misrouted cards, bounded delivery),
  `remote_terminal` (geometry, stream budgets, suspend/resume),
  `desktop_protocol` (capability and frame schema) and
  `tests/desktop_terminal_smoke.py` (real bridge, private tmux server, real
  viewer CLI: pinned attach, live coloured bytes, card ownership, revocation
  teardown, host session survival). Control keys, IME, bracketed paste, mouse
  and viewer input remain to cover with the input increment.
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
| Shell + installed harness + full-screen app | Live console output, colour and full-screen redraws match the host; typing is not yet routed. |
| Drag/resize from either machine | Other view updates; conflicts visible; no resizing oscillation. |
| Create/close on B from A | B owns the lifecycle and folder; A's local state is unchanged. |
| Rapid switching and hide/show | No keys reach the wrong PC and no harness is killed. |
| B asleep, bridge restart, daemon restart | Clear stale/offline state; safe refresh; no duplicate commands, and every console says why it has no live stream. |
| Android plus desktop viewers | Existing phone pairing/list/input/files remain compatible. |
| B views C while A views B | A sees and controls B's local workspace, never C's. |
| Revoke A on B | Streams close promptly; A cannot keep typing; B's sessions survive. |

Run `cargo test --bin super-desktop`, isolated bridge smoke tests (extend
`tests/bridge_security_smoke.py` or add a desktop-specific companion), and the
repository-prescribed `./rebuild.sh --no-daemon` for build-only validation. Use
`./rebuild.sh` only when deliberately installing/restarting for manual testing;
do not disturb live sessions to execute automated tests.

Record input-to-display latency and idle CPU/network usage for one, four and
eight visible terminals, plus repeated switch/reconnect cycles. Initial LAN
target: p95 echo latency below 100 ms on an unloaded wired network; report hardware
and conditions rather than treating this as an Internet guarantee. Require bounded
memory, file descriptors and attach-client counts after repeated disconnects, and
no regression in local overlay responsiveness with every peer offline.

The feature is complete when a user can pair two PCs, select either workspace,
recognize its layout, work interactively in its existing harnesses, and return
to local work without losing sessions or sending actions to the wrong machine.
