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
TLS/request deadline enforced by a socket reaper; 16 KiB headers and bodies;
16 KiB inbound WebSocket frames; 125-byte control frames. Pairing is invitation-
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

The smoke test uses temporary state and an ephemeral port, verifies TLS rejection,
authentication, invitation consumption, desktop-only approval, live revocation,
oversized requests, and malformed headers. It never approves production phones.
Android has unit tests and an optional `BridgeSecurityInstrumentation` real-device
test (public `host`, `port`, `pin` arguments); it writes no pairing credentials.

This is implementation hardening and regression coverage, not an independent
security certification or penetration test.
