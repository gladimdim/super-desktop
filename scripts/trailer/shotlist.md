# SUPER DESKTOP — trailer

43.7 s, 1920 × 1080, 30 fps, silent. Captions carry the message: social feeds
and the website both play it muted. The copy that ships on the site is in
`docs/video/`; see that folder's README for what is published and why.

## The cut

| # | t | Source | On screen | Caption |
|---|------|--------|-----------|---------|
| 1 | 0.0–4.0 | desktop, 60 fps | The website in Chrome, then `SUPER + SHIFT + Q` keycaps and the overlay sliding in over it | "Open all your harnesses with a single button press" |
| 1 | 4.0–15.0 | desktop | A session is focused and **"Improve performance" is typed live**; Enter, and the agent reads the workspace and answers | "Work inside any session, right in the overlay" |
| 1 | 15.0–17.2 | desktop | The overlay hides, the website is back, nothing left behind | "Hide it again and keep working" |
| 2 | 17.2–24.8 | Android, Z Fold 7 | The phone slides in over the right of the dimmed page: harness list, a live terminal, then the DUO split with two PCs' agents side by side | "Reach every harness from anywhere — Android and iOS" / "Browse and open any session from your phone" |
| 3 | 24.8–38.3 | desktop, 60 fps | The **This PC** menu opens, the other PC is chosen, "Connecting to…", its consoles stream in with `REMOTE` badges, and **a prompt is typed into one of them** | "Cross-connect your PCs and work on remote harnesses" / "Type here. It runs on that machine." |
| 4 | 38.3–40.6 | still | Wrap card: the bolt, "Get it now", the URL, the platform line | — |
| 5 | 40.6–43.7 | animation | **SUPER** flies in from the left, **DESKTOP** from the right; they land on the bolt and it blasts, then settles into the lockup | — |

Everything on screen is real. The overlay animation is a 60 fps `wf-recorder`
capture, the prompts are typed with `ydotool` into live sessions, and the
agent's replies are its own. The remote beat opens a genuinely paired second PC
over the LAN bridge.

The keycap badge shows the default `SUPER + SHIFT + Q`; the take itself is
triggered through `super-desktop show`, the same convention the older
`desktop-toggle.gif` capture used.

**The remote prompt is typed but not submitted.** Pressing Enter would launch an
agent on the other machine, in whatever repository that session is sitting in.
Set it going by hand first if you want a running agent in the shot.

## Building it

    ./make-assets.sh          # captions, keycaps, bolt, cards, 108 logo frames
    ./build.sh                # -> ~/Videos/SuperDesktop/super-desktop-trailer.mp4 + poster
    ./build.sh out.mp4 --web  # also refreshes docs/video/ for the website

`make-assets.sh` regenerates every overlay from scratch — the copy lives in one
block at the top of that file, so changing a caption never means editing
ImageMagick calls. Takes and generated assets live in `$FOOTAGE`
(`~/Videos/SuperDesktop/super-desktop-trailer-footage` by default), outside
the repository: the raw captures are large and not worth versioning.

`common.sh` holds the palette, fonts and helpers, and is sourced by the rest.

## Shooting it

    ./capture-desktop.sh both   # beats 1 and 3
    ./capture-android.sh        # beat 2

`capture-desktop.sh` needs `ydotool`, because nothing else can drive this UI:
`/dev/uinput` is root-only, AT-SPI reports every widget as a 0 × 0 unnamed panel
under layer-shell, and `wtype`'s keys never arrive since the surface only takes
keyboard focus after a pointer click. Start the daemon once:

    sudo systemd-run --unit=ydotoold ydotoold -p /run/user/$UID/.ydotool_socket -o $UID:$UID

Three things that will ruin a take, each learned the hard way:

1. **A click outside the overlay dismisses it**, and parking the pointer in the
   top-left corner trips the hot corner and toggles it. Move the pointer to the
   middle after every click.
2. **The first reveal after a card is created, iconified or restored can play
   twice** — the overlay settles, drops out for a few frames, then animates in
   again. It is subtle but visible. The warm-up cycles in `capture-desktop.sh`
   make it single; `verify-reveal.sh` proves it before you build.
3. **Whatever is in front when recording starts is the opening frame.** The
   script raises the browser on the site first, and waits for the hide animation
   to finish, so frame one is always the website.

**Stage the desk before beat 1.** The capture is 1080p and terminal text is
legible: iconify any card carrying pairing PINs, tokens or private paths. A
fresh card in a scratch workspace makes a good subject —

    super-desktop add-term-in '{"agentType":"claude","workspace":"/tmp/demo"}'

— and a harness sitting at its welcome prompt costs nothing.

## When the iPhone app ships

There is no iPhone beat: the iOS app is simulator-only and cannot be recorded
from Linux, so beat 2's caption and the wrap card carry it in words. To add one,
record the simulator on the Mac, cut a beat between 3 and 4 in `build.sh`, and
update `WRAP_NOTE` in `make-assets.sh`.
