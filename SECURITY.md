# Bridge security (protocol v3)

The bridge is a remote terminal capability. An approved phone can operate the
desktop user's SUPER DESKTOP terminals; it is not a read-only dashboard or a
sandbox. Do not approve unknown devices or publish pairing invitations.

## Transport and identity

Port 8759 accepts TLS 1.2/1.3 HTTPS and WSS only, on LAN, Ethernet, or Tailscale.
There is no HTTP fallback. Tailscale is optional. The bridge creates a persistent
self-signed certificate; Android trusts its SHA-256 certificate fingerprint only
after scanning the desktop QR (or pasting the invitation through a trusted
channel). Discovery and IP addresses never establish trust. A changed certificate
requires a fresh, physically verified invitation. HTTP redirects are disabled.

Wi-Fi discovery intentionally discloses the machine name and service address.
The unauthenticated HTTPS ping returns network interface addresses. Neither
endpoint grants terminal access. Android requests detailed metadata only once
it has the desktop's trusted certificate pin.

## Pairing and access

1. Desktop settings → Android → Show secure pairing QR. The compact QR contains
   `superdesktop://pair?data=<base64url invitation JSON>`; its copyable link is expandable.
2. Scan with a camera that supports custom-scheme links and tap Open, or use
   Android → Bridges → Scan pairing QR, or paste the invitation. External links
   require explicit confirmation and never automatically approve a device. Manual address
   selection can override the invitation's LAN route without changing its pin.
3. Android requests approval using the QR's random 192-bit, single-use secret.
   Invitations expire after 300 seconds. Approval requests expire after 120 seconds.
4. Compare the displayed code and approve on the desktop. Names are untrusted labels.
5. The phone receives a separate random 192-bit bearer credential, valid for 90 days.

Desktop state stores SHA-256 credential hashes, not bearer credentials. Android
stores credentials and pins in Keystore-backed encrypted preferences; backups and
preference device transfer are disabled. Old HTTP-era credentials are invalidated
on upgrade and phones must pair again. Certificates persist across restarts.

Desktop approval, invitations, device listing, and revocation are available only
over an owner-only Unix socket inside a 0700 state directory. Network localhost
has no authentication bypass. Requests with browser Origin/Sec-Fetch-Site headers
are rejected. Revoke access disconnects the device's active sockets and invalidates
its credential; removing a connection on Android alone does not revoke it.
The desktop's active/registered counter counts distinct devices, not sockets.
Active means an open authenticated connection or authenticated activity within
the last 60 seconds. Expired/revoked devices are never counted active; registered
devices remain visible while the bridge is stopped.

The additive `/api/v1/desktop/capabilities`, `/api/v1/desktop/workspace`,
`/api/v1/desktop/events`, `/api/v1/desktop/commands` and
`/api/v1/desktop/terminals/<card-id>/attach` endpoints require paired-device
authentication and follow the same origin rejection, expiry and revocation
rules. Authorization runs before the method check, so an unpaired caller learns
nothing about a route's shape. The read-only snapshot capability exposes
owned terminal geometry, ordering, folders and cached titles; it excludes notes,
launch commands, local OS settings and credentials. Snapshots come from the
daemon's local in-memory model over Unix IPC, not from a peer it may later be
viewing. Export is capped at 256 cards and 1 MiB of IPC data. Events reuse bridge
connection bounds, write deadlines, stream lifetime and live revocation, are
limited to 4 subscriptions per credential and 16 per bridge (refused with
`subscription_limit` before the upgrade), and never buffer more than four events
per subscriber: a subscriber that falls behind is told to `resync` instead. The
daemon's change feed (`desktop-watch`) is served only on the owner-only Unix
socket, carries no workspace data, and is limited to four readers.

The attach stream carries the bytes of one owned `sd_term_*` session in both
directions. Binary frames from an authenticated viewer are raw terminal input:
the bridge writes them into that session's PTY behind the same per-session
input guard that serializes phone keystrokes and image-prompt submission. A
busy guard or a PTY that cannot accept the bytes drops them; they are never
queued for a later write. Text frames remain control-only (`grid`). A card id
is resolved against the daemon's own workspace snapshot, so an unowned or
unrelated tmux session is not addressable. Attachments are limited to 8 per
credential and 16 per bridge, are refused with a stable code before the
WebSocket upgrade, are torn down when the credential is revoked or expires,
and end after 30 minutes. The host attaches its own tmux client at the grid it
already owns, which is what prevents a viewer from resizing host panes, and
dropping the stream reaps exactly that client.

The command route is the only remote mutation endpoint, and it accepts one typed
envelope per request: a card id that exists in the daemon's own workspace, a
request id, the daemon epoch the viewer saw, the card revision it based the
change on, and geometry within fixed bounds. Nothing accepts a shell command, a
tmux target, an environment variable, a file path or free-form text; unknown
fields and unknown command variants are refused. The bridge validates shape and
bounds before any owner IPC, and the daemon re-checks epoch, revision and bounds
before it applies anything through the card's own local actions, so a stale
viewer is refused with the daemon's current geometry instead of overwriting a
concurrent edit. Request ids are deduplicated per credential inside one epoch
(16 per device, 256 overall, dropped on an epoch change); a request whose owner
never answered is remembered as uncertain and its retry is refused rather than
applied twice. `workspace-layout-v1` is advertised because `setLayout`, `setExpanded`,
`closeTerminal` and `createTerminal` are implemented; `setWorkspace` is refused
with `unsupported_command`.

`setExpanded` is presentation only: it is the same expand/collapse the host's own
double-click performs, it writes no saved geometry, and it leaves the host's
layer-shell keyboard mode alone, so a viewer can look into a card without taking
that machine's keyboard. The buttons that send these commands are the host's own
card and bar widgets rendered by the viewer, so a remote console offers exactly
the controls a local one does and nothing more.

A create is the one command that makes something new, so it is bounded by the
host's own snapshot: `agentType` must be one of the harnesses that host lists as
visible, and `workspace` must be one of the folders that host published (`folders`
in the snapshot: the effective one first, then the ones it has used before, capped
at 16). `setWorkspace` — the remote folder picker — is bounded by that same list and
by the workspace revision, so a viewer can move that machine to a folder it already
offered and nothing else, and two viewers cannot silently overwrite each other's
choice. The host then
builds the card through the same path a local launch uses, so the executable, the
sandbox flags, the tmux session name and the card geometry are all decided on the
host, and a card is reported only when its session is alive. A refused create
creates nothing. The host is not forced to show its overlay, and no peer can
choose a command, a flag or a path. Security protocol v3 and existing Android
credentials are unchanged.

## Resource bounds

### Referenced files

File listing/registration, content downloads and PDF page previews require the
same paired-device authentication as terminal access. Asset IDs are not bearer
capabilities. The service serves only registered, supported regular files within
the recorded terminal workspace: no arbitrary-path GET, hidden components,
symlinks, hard links, special files, HTML/SVG, URL fetching or directory scans.
Descriptor-relative opens prevent symlink-swap escapes, and metadata/version
checks reject replaced or changed files. Limits are 16 MiB per file, 512 KiB for
text, 64 references per terminal, 64 terminal catalogs and four jobs per process.
Revocation is checked during chunked content writes. Exported copies cannot be
revoked. PDF rasterization requires a no-network bubblewrap sandbox with CPU,
memory, output and wall-clock bounds; it never falls back to unsandboxed parsing.
See [file previews](docs/FILE_ASSETS.md) for decoder limits and known limitations.

64 simultaneous network connections; 12 per source IP; 5-second total initial
TLS/request deadline enforced by a socket reaper; 16 KiB headers and normal bodies;
16 KiB inbound WebSocket frames; 125-byte control frames. A command document is
capped at 8 KiB and its deduplication cache at 16 answers per credential and 256
overall. The authenticated image-prompt route alone allows a 3 MiB JSON body, a
30-second upload deadline,
and four concurrent jobs shared with file previews. Authorization and origin
checks happen before accepting the larger body. Images are validated, capped,
re-encoded and privately staged; no client-selected file paths or overwrites.
See [image prompt security and retention](docs/IMAGE_PROMPTS.md).
Pairing is invitation-
gated and limited to eight pending/recent requests and one per source per 120 seconds.
At most 64 paired devices. These bounds mitigate abuse, not volumetric network DoS.

## Trust boundary and recovery

Other programs running as the same desktop user (and root) remain trusted. A
compromised desktop, unlocked/compromised phone, maliciously replaced QR, or stolen
credential is outside transport protection. Avoid Internet port forwarding;
use your firewall to constrain reachability to networks/devices you intend.

State lives in `~/.local/state/omarchy/harness-bridge/`. Keep `tls-identity.json`
private (0600). Loss/replacement requires pairing again. The optional
`SUPER_DESKTOP_BRIDGE_STATE_DIR` isolates state for tests or separate instances.

## Verification

`cargo test --bin super-desktop`

`cargo build --bin super-desktop && python tests/bridge_security_smoke.py target/debug/super-desktop`

`python3 tests/desktop_terminal_smoke.py target/debug/super-desktop`

The smoke tests use temporary state, an ephemeral port and a private tmux server,
verify TLS rejection, authentication, invitation consumption, desktop-only
approval, live revocation (including mid-stream), oversized requests, malformed
headers, card ownership and that a refused attachment never reaches a session.
They never approve production phones or touch production sessions.
Android has unit tests and an optional `BridgeSecurityInstrumentation` real-device
test (public `host`, `port`, `pin` arguments); it writes no pairing credentials.

This is implementation hardening and regression coverage, not an independent
security certification or penetration test.
