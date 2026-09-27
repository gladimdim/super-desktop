# Shared configuration for the trailer scripts. Source it, don't run it.
#
#   FOOTAGE   where the takes and generated assets live (outside the repo:
#             the raw captures are large and are not worth versioning)
#   ASSETS    generated overlays, cards and animation frames
#
# Everything here is regenerated from scratch by make-assets.sh, so the only
# irreplaceable inputs are the three takes in $FOOTAGE and the iOS screenshot
# that ships in docs/screenshots/.
set -euo pipefail

TRAILER_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_DIR=$(cd "$TRAILER_DIR/../.." && pwd)
FOOTAGE=${FOOTAGE:-$HOME/Videos/super-desktop-trailer-footage}
ASSETS=${ASSETS:-$FOOTAGE/assets}

W=1920; H=1080; FPS=30

# The website's palette, so the cards and captions match the page they sit on.
INK='#f2f5fa'; MUTED='#9aa3b5'; DIM='#6b7385'
CYAN='#67e8f9'; INDIGO='#818cf8'; GOLD_HI='#ffe066'; GOLD_LO='#f59e0b'
INK_BG='#070a10'; PANEL='#131722'; KEYCAP='#151b28'; KEYEDGE='#46536e'

FONT_BOLD=$(fc-match -f '%{file}' 'Noto Sans:bold')
FONT_REG=$(fc-match -f '%{file}' 'Noto Sans')
FONT_MONO=$(fc-match -f '%{file}' 'JetBrainsMono Nerd Font')

# ImageMagick's -size leaks into the next label:, which silently rescales the
# text. Always reset it with +size — this bit us once already.
text_png() {  # text_png OUT FONT POINTSIZE FILL TEXT...
  local out=$1 font=$2 size=$3 fill=$4; shift 4
  magick -background none +size -fill "$fill" -font "$font" -pointsize "$size" label:"$*" "$out"
}

# A lower-third caption over a scrim, sized for the full frame so it can be
# overlaid at 0,0 and faded as a whole.
caption_png() {  # caption_png OUT POINTSIZE TEXT...
  local out=$1 size=$2; shift 2
  local tmp; tmp=$(mktemp -d)
  text_png "$tmp/t.png" "$FONT_BOLD" "$size" "$INK" "$@"
  magick -size ${W}x320 gradient:'#00000000'-'#000000d9' "$tmp/s.png"
  magick -size ${W}x${H} xc:none "$tmp/s.png" -gravity south -geometry +0+0 -composite \
         "$tmp/t.png" -gravity south -geometry +0+86 -composite "$out"
  rm -rf "$tmp"
}

require_footage() {
  local missing=0
  for f in "$@"; do
    [ -f "$FOOTAGE/$f" ] || { echo "missing footage: $FOOTAGE/$f" >&2; missing=1; }
  done
  [ "$missing" = 0 ] || { echo "run the capture scripts first" >&2; exit 1; }
}
