#!/usr/bin/env bash
# Generates every overlay, card and animation frame the trailer needs.
# Nothing here depends on the captures, so it is safe to re-run at any time.
#
#   ./make-assets.sh            -> $HOME/Videos/SuperDesktop/super-desktop-trailer-footage/assets
#   ASSETS=/tmp/a ./make-assets.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

# ─── Copy. Edit here when the story changes (iPhone shipping, new features). ──
CAP_OPEN='Open all your harnesses with a single button press'
CAP_TYPE='Work inside any session, right in the overlay'
CAP_HIDE='Hide it again and keep working'
CAP_PHONE='Reach every harness from anywhere — Android and iOS'
CAP_PHONE2='Browse and open any session from your phone'
CAP_REMOTE='Cross-connect your PCs and work on remote harnesses'
CAP_REMOTE2='Type here. It runs on that machine.'
WRAP_TITLE='Get it now'
WRAP_URL='superdesktop.dmytrogladkyi.com'
# When the iPhone app ships, this line is what changes (and a real iOS beat
# gets cut into build.sh).
WRAP_NOTE='Android app available · iPhone in development'
# ─────────────────────────────────────────────────────────────────────────────

mkdir -p "$ASSETS"
cd "$ASSETS"

echo "captions"
caption_png c_open.png    54 "$CAP_OPEN"
caption_png c_type.png    54 "$CAP_TYPE"
caption_png c_hide.png    54 "$CAP_HIDE"
caption_png c_phone.png   50 "$CAP_PHONE"
caption_png c_phone2.png  54 "$CAP_PHONE2"
caption_png c_remote.png  50 "$CAP_REMOTE"
caption_png c_remote2.png 54 "$CAP_REMOTE2"

echo "keycap badge"
keycap() {  # keycap LABEL WIDTH OUT
  magick -size ${2}x86 xc:"$KEYCAP" -bordercolor "$KEYEDGE" -border 2 \
    \( -background none +size -fill "$INK" -font "$FONT_MONO" -pointsize 34 label:"$1" \) \
    -gravity center -composite \
    \( +clone -alpha extract \
       -draw 'fill black polygon 0,0 0,12 12,0 fill white circle 12,12 12,0' \
       \( +clone -flip \) -compose multiply -composite \
       \( +clone -flop \) -compose multiply -composite \) \
    -alpha off -compose copy_opacity -composite "$3"
}
keycap 'SUPER' 190 k1.png; keycap 'SHIFT' 180 k2.png; keycap 'Q' 96 k3.png
text_png k_plus.png "$FONT_BOLD" 40 '#7b849b' '+'
magick -size 760x110 xc:none \
  k1.png -gravity west -geometry +0+0    -composite \
  k_plus.png -gravity west -geometry +200+0 -composite \
  k2.png -gravity west -geometry +240+0  -composite \
  k_plus.png -gravity west -geometry +436+0 -composite \
  k3.png -gravity west -geometry +476+0  -composite keys-row.png
magick -size ${W}x${H} xc:none keys-row.png -gravity center -geometry +0+300 -composite keys.png

echo "phone frame"
# Rounded mask and drop shadow for the phone footage, sized for the Fold's
# inner display cropped to its visible area (1968x2184 -> 847x940).
magick -size 847x940 xc:none -fill white -draw 'roundrectangle 0,0,846,939,26,26' pipmask.png

echo "brand mark"
magick -size 400x640 xc:none -fill white \
  -draw 'polygon 248,0 72,352 184,352 136,640 352,256 232,256 312,0' boltmask.png
magick -size 400x640 gradient:"${GOLD_HI}-${GOLD_LO}" boltgrad.png
magick boltgrad.png boltmask.png -alpha off -compose copy_opacity -composite bolt.png
magick bolt.png \( +clone -background "$GOLD_LO" -shadow 100x18+0+0 \) +swap \
  -background none -layers merge +repage boltglow.png
text_png w_super.png "$FONT_BOLD" 132 "$INK" 'SUPER'
magick -background none +size -font "$FONT_BOLD" -pointsize 132 label:'DESKTOP' \
  -alpha extract w_dmask.png
magick -size "$(identify -format '%wx%h' w_dmask.png)" gradient:"${CYAN}-${INDIGO}" w_dgrad.png
magick w_dgrad.png w_dmask.png -alpha off -compose copy_opacity -composite w_desktop.png

echo "end backdrop, flash and starburst"
magick -size ${W}x${H} radial-gradient:'#141a28'-'#05070c' bg_end.png
magick -size 1400x1400 xc:white \
  \( -size 1400x1400 radial-gradient:'#ffffff'-'#000000' -colorspace gray \) \
  -alpha off -compose copy_opacity -composite PNG32:flash.png
python3 - <<'PY'
import subprocess, math
cx = cy = 700; parts = []
for i in range(28):
    a = i * 2 * math.pi / 28
    parts += ["-strokewidth", "16" if i % 2 == 0 else "7", "-draw",
              "line %.0f,%.0f %.0f,%.0f" % (cx + math.cos(a)*120, cy + math.sin(a)*120,
                                            cx + math.cos(a)*690, cy + math.sin(a)*690)]
subprocess.run(["magick", "-size", "1400x1400", "xc:none", "-stroke", "#ffe9a8",
                "-fill", "none", *parts, "rays.png"], check=True)
PY

echo "wrap card"
text_png g1.png "$FONT_BOLD" 88 "$INK"   "$WRAP_TITLE"
text_png g2.png "$FONT_MONO" 44 "$CYAN"  "$WRAP_URL"
text_png g3.png "$FONT_REG"  34 "$MUTED" "$WRAP_NOTE"
magick bg_end.png \
  \( boltglow.png -resize x150 \) -gravity center -geometry +0-250 -composite \
  g1.png -gravity center -geometry +0-60 -composite \
  \( -size 900x94 xc:"$PANEL" \) -gravity center -geometry +0+60 -composite \
  g2.png -gravity center -geometry +0+60 -composite \
  g3.png -gravity center -geometry +0+180 -composite wrapcard.png

echo "logo animation (108 frames, ~2 min)"
python3 "$TRAILER_DIR/logo-anim.py" "$ASSETS"

echo
echo "assets written to $ASSETS"
