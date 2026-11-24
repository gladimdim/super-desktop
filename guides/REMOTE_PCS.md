[← Back to README](../README.md)

# Use Another PC's Harnesses

Pair two PCs running SUPER DESKTOP, and either one can open the other's
workspace from the top-left **This PC** selector. A remote workspace uses the
same top bar and console cards as a local one, and everything in it acts *on
that PC*:
- Its consoles stream live, in colour, at that PC's own positions, sizes and
  stacking order.
- Typing, paste and Ctrl+C reach the focused session there.
- Dragging, edge-resizing, minimizing, maximizing and closing a card do the
  same on that PC.
- Its harness buttons launch harnesses there, and the folder field lists the
  folders that PC offers.

Changes made on that PC appear as soon as it publishes them. Pairing is one-way:
approving a PC lets it open this one, not the reverse. Install the same current
build on both PCs (run the install command again, or `git pull && ./rebuild.sh`
in a clone; both also restart the separate bridge process).

## Pair two PCs

1. On the PC you want to open (the host), go to ⚙ Settings → **Connections** →
   **Add a device** → **Share this PC**. You can also use **This PC** → **Add a
   PC** → *I want this PC's harnesses to be available on another PC*. It checks
   the bridge, the firewall rule for port 8759/tcp and the network, then shows a
   single-use link that expires in five minutes. Click **Copy link**, then send
   it to the other PC over a channel you trust.
2. On the viewing PC, go to **This PC** → **Add a PC** → *I want to connect to
   another PC and view its harnesses*, or Settings → Connections → Add a device
   → **View another PC**. Paste the link and click **Connect to PC**.
3. The host's **connection request panel** opens. If the overlay is hidden,
   click the notification to open it. Click **Approve** only if its six-digit
   code matches the one on the viewing PC. **Reject** refuses the request and
   blocks that PC until you remove it from Settings → Connections → **Rejected
   devices**.

The viewing PC then selects the host and draws its workspace. To withdraw
access, go to the host's Settings → Connections → **PCs** → *Can open this PC*
and click **Revoke**. The viewer's streams close within a second.

## Viewing a remote workspace

- **Fit / 100%** (left of Hide) chooses how the workspace is drawn. **Fit**
  shows all of it, scaled down and never enlarged. **100%** shows one host pixel
  per logical pixel. To pan, drag empty canvas, use the scrollbars, or scroll.
  To zoom (25–300%), use Ctrl+scroll or pinch; click the button to return to
  100%. Terminals are redrawn at the new font size rather than scaled. The view
  is never sent to the host or saved.
- An outline in the theme's accent colour marks the edges of that PC's screen.
  **Fit** centres it with a margin on every side; at **100%** you can scroll a
  margin past each edge, so the outline stays visible at both ends. A card you
  drag stops at that edge, so it always stays whole on that PC's screen.
- Each action reports its result in a short line under the card's header, or
  under the top bar for launches and folder picks. The line clears itself.
  - *Changed on that PC · showing its layout*: someone changed that card there
    first, so the card moves back to that PC's layout.
  - *Cannot reach that PC · change not applied*: the request never arrived.
  - *Result unknown · check before retrying*: the request was sent but no
    answer came back. It is never resent automatically.
- While a PC is connecting or cannot be reached, its workspace shows why and
  what to do, above a checklist of each link between the two PCs: this PC's
  network (Wi-Fi name or Ethernet, and address), Tailscale (on or off, and
  whether that PC is online in your tailnet), the route to that PC, and
  whether its port 8759 answers. **Retry now** tries again at once and
  repeats the checks; **Pair again** appears when only a new pairing helps.
- Switching back to **This PC**, hiding the overlay, or restarting either PC
  leaves every harness running.

**Not available for a remote PC yet:** sticky notes (a remote workspace shows
only that PC's consoles, and Ctrl+N makes no note there), the **Files & links**
button, and removing a saved PC from the GUI. To remove a saved PC, run
`super-desktop peer-forget MACHINE_ID`; `super-desktop peer-list` shows the IDs.

## Troubleshooting

| You see | Do this |
| --- | --- |
| *Update SUPER DESKTOP on the host* (or *on that PC*) | That PC runs an older build: run the install command there (in a clone you work in, `git pull && ./rebuild.sh`). |
| *… needs pairing again* or *Pairing required · Add this PC again* | The host revoked this PC, or its 90-day credential expired. Click **Pair again** and use a new link. |
| *… is not answering*, *Cannot reach this PC* or *Reconnecting…* | Check that the host is on, SUPER DESKTOP is running there, and port 8759/tcp is reachable over LAN or Tailscale. The checklist under the message shows which link fails. Consoles reconnect on their own. |
| *Tailscale is off on this PC* | That PC was paired at its Tailscale address. Run `tailscale up` on this PC. |
| *… is on another network* | That PC's LAN address is outside this PC's network. Join its network, or pair the two PCs over Tailscale. |
| *That PC restarted · change not applied* | Its SUPER DESKTOP restarted before the change arrived, and the view has refreshed. Repeat the action if you still want it. |
| *The other PC rejected this PC earlier…* | On the host, remove this PC in Settings → Connections → Rejected devices, then use a new link. |
| *The other PC already has a request from this address* | Wait two minutes for that request to expire, then use a new link. |
| The host answers 404 for `/api/v1/desktop/capabilities` | An old bridge is still running there. Running the install command (or `./rebuild.sh`) on the host restarts both processes. |
