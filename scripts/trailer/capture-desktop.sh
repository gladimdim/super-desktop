#!/usr/bin/env bash
# The two desktop takes.
#
#   ./capture-desktop.sh a     the reveal, typing into a session, hiding again
#   ./capture-desktop.sh b     This PC -> another PC's workspace, typed remotely
#   ./capture-desktop.sh both
#
# ── What this needs, and why ─────────────────────────────────────────────────
# ydotool. Nothing else can drive this UI: /dev/uinput is root-only so no other
# tool can synthesise input, AT-SPI reports every widget as a 0x0 unnamed panel
# under layer-shell, and wtype's keys never arrive because the surface only
# takes keyboard focus after a pointer click. Start the daemon once:
#
#   sudo systemd-run --unit=ydotoold ydotoold -p /run/user/$UID/.ydotool_socket -o $UID:$UID
#
# ── Three things that will ruin a take ───────────────────────────────────────
# 1. A click landing outside the overlay dismisses it, and leaving the pointer
#    in the top-left corner trips the hot corner and toggles it. Park the
#    pointer in the middle after every click.
# 2. The first reveal after a card is created, iconified or restored can play
#    twice — the overlay settles, drops out for a few frames, then animates in
#    again. Warm-up cycles below make it single. Verify with verify-reveal.sh.
# 3. Whatever is in front when recording starts is the opening frame. Raise the
#    browser on the site first, which is what SITE_URL below does.
#
# Stage the desk before take A: the capture is 1080p and terminal text is
# legible. Iconify anything carrying pairing PINs, tokens or private paths.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WHICH=${1:-both}
MONITOR=${MONITOR:-eDP-1}
SITE_URL=${SITE_URL:-https://superdesktop.dmytrogladkyi.com/}
export YDOTOOL_SOCKET=${YDOTOOL_SOCKET:-/run/user/$UID/.ydotool_socket}
mkdir -p "$FOOTAGE"

# Coordinates, measured on a 1920x1080 display. Re-measure with `grim` if the
# top bar or the card you type into sits elsewhere.
PC_MENU_X=55;   PC_MENU_Y=30      # the This PC selector
PC_OTHER_X=90;  PC_OTHER_Y=133    # the second entry in its dropdown
LOCAL_CARD_X=900;  LOCAL_CARD_Y=600   # prompt line of the session to type into
REMOTE_CARD_X=900; REMOTE_CARD_Y=722  # same, once the remote workspace is open
PARK_X=960; PARK_Y=540            # anywhere central: off the hot corner

LOCAL_PROMPT=${LOCAL_PROMPT:-'Improve performance'}
REMOTE_PROMPT=${REMOTE_PROMPT:-'Run the full benchmark suite'}

sleep_py() { python3 -c "import time,sys; time.sleep(float(sys.argv[1]))" "$1"; }

warm_up() {  # a card change can make the next reveal play twice; this settles it
  for _ in 1 2; do
    super-desktop hide >/dev/null 2>&1 || true; sleep_py 1.5
    super-desktop show >/dev/null 2>&1 || true; sleep_py 2.0
  done
  super-desktop hide >/dev/null 2>&1 || true; sleep_py 1.5
}

start_rec() {  # start_rec OUTFILE
  rm -f "$1"
  wf-recorder -o "$MONITOR" -f "$1" -c libx264 -p crf=14 -p preset=veryfast \
              -r 60 -x yuv420p >/dev/null 2>&1 &
  REC=$!
}
stop_rec() { kill -INT "$REC" 2>/dev/null || true; wait "$REC" 2>/dev/null || true; }

take_a() {
  local out="$FOOTAGE/desktop-raw-a.mkv"
  nohup "${BROWSER:-google-chrome-stable}" "$SITE_URL" >/dev/null 2>&1 &
  sleep_py 4                                  # the site must be the opening frame
  warm_up
  super-desktop hide >/dev/null 2>&1 || true
  ydotool mousemove -a -x 1600 -y 1050 >/dev/null
  sleep_py 1.4                                # let the hide finish before frame 1
  start_rec "$out"
  LOCAL_CARD_X=$LOCAL_CARD_X LOCAL_CARD_Y=$LOCAL_CARD_Y PROMPT=$LOCAL_PROMPT python3 - <<'PY'
import os, subprocess, time
def yd(*a): subprocess.run(["ydotool", *a], capture_output=True)
x, y, prompt = os.environ["LOCAL_CARD_X"], os.environ["LOCAL_CARD_Y"], os.environ["PROMPT"]
time.sleep(2.4)                                   # the website, held
subprocess.run(["super-desktop", "show"], capture_output=True)
time.sleep(3.2)                                   # the reveal, settled
yd("mousemove", "-a", "-x", x, "-y", y); yd("click", "0xC0")
time.sleep(1.0)
yd("type", "-d", "55", prompt)                    # slow enough to read
time.sleep(1.0)
yd("key", "28:1", "28:0")                         # Enter: the agent really runs
time.sleep(7.5)
subprocess.run(["super-desktop", "hide"], capture_output=True)
time.sleep(2.5)                                   # back to a clean desktop
PY
  stop_rec
  echo "$out"
}

take_b() {
  local out="$FOOTAGE/desktop-raw-b.mkv"
  warm_up
  ydotool mousemove -a -x $PARK_X -y $PARK_Y >/dev/null
  start_rec "$out"
  MX=$PC_MENU_X MY=$PC_MENU_Y OX=$PC_OTHER_X OY=$PC_OTHER_Y \
  CX=$REMOTE_CARD_X CY=$REMOTE_CARD_Y PX=$PARK_X PY=$PARK_Y PROMPT=$REMOTE_PROMPT python3 - <<'PY'
import os, subprocess, time
def yd(*a): subprocess.run(["ydotool", *a], capture_output=True)
e = os.environ
time.sleep(1.2)
subprocess.run(["super-desktop", "show"], capture_output=True)
time.sleep(2.2)                                   # the local workspace
yd("mousemove", "-a", "-x", e["MX"], "-y", e["MY"]); yd("click", "0xC0")
time.sleep(1.4)                                   # menu open and legible
yd("mousemove", "-a", "-x", e["OX"], "-y", e["OY"]); yd("click", "0xC0")
yd("mousemove", "-a", "-x", e["PX"], "-y", e["PY"])   # off the hot corner
time.sleep(4.5)                                   # connect, consoles stream in
yd("mousemove", "-a", "-x", e["CX"], "-y", e["CY"]); yd("click", "0xC0")
time.sleep(0.9)
# Typed, deliberately not submitted: Enter would launch an agent on the other
# machine, in whatever repository that session is sitting in.
yd("type", "-d", "55", e["PROMPT"])
time.sleep(3.2)
PY
  stop_rec
  echo "$out"
}

case "$WHICH" in
  a) take_a ;;
  b) take_b ;;
  both) take_a; take_b ;;
  *) echo "usage: $0 [a|b|both]" >&2; exit 2 ;;
esac
