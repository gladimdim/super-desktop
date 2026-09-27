#!/usr/bin/env bash
# Assembles the 43.7 s trailer from the takes in $FOOTAGE and the generated
# assets in $ASSETS.
#
#   ./make-assets.sh && ./build.sh                 -> ~/Videos/super-desktop-trailer.mp4
#   ./build.sh out.mp4 --web                       -> also writes the page copy
#
# --web additionally encodes the copy that ships in docs/video/: CRF 22, which
# measured 49.5 dB PSNR against the master on a text-heavy frame — the terminal
# text stays crisp at roughly three-quarters the size. MP4 only, deliberately:
# H.264 plays in every current browser and a VP9 twin would double page weight.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

OUT=${1:-$HOME/Videos/super-desktop-trailer.mp4}
WEB=${2:-}

require_footage desktop-raw-a.mkv desktop-raw-b.mkv android-raw.mp4
[ -d "$ASSETS/logoframes" ] || { echo "no assets: run ./make-assets.sh first" >&2; exit 1; }

A="$FOOTAGE/desktop-raw-a.mkv"
B="$FOOTAGE/desktop-raw-b.mkv"
P="$FOOTAGE/android-raw.mp4"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
x264="-c:v libx264 -crf 17 -preset medium"

# ── 1. Reveal, typing, hide (17.2 s) ─────────────────────────────────────────
# The cut starts at 1.4 s so the website holds for a beat before the overlay.
echo "beat 1: the desktop"
ffmpeg -y -v error -i "$A" \
  -loop 1 -i "$ASSETS/keys.png" -loop 1 -i "$ASSETS/c_open.png" \
  -loop 1 -i "$ASSETS/c_type.png" -loop 1 -i "$ASSETS/c_hide.png" \
 -filter_complex "\
  [0:v]trim=1.4:18.6,setpts=PTS-STARTPTS,fps=$FPS,scale=$W:$H[v]; \
  [1:v]format=rgba,fade=in:st=0.25:d=0.3:alpha=1,fade=out:st=1.75:d=0.35:alpha=1[k]; \
  [2:v]format=rgba,fade=in:st=1.45:d=0.35:alpha=1,fade=out:st=4.3:d=0.4:alpha=1[c1]; \
  [3:v]format=rgba,fade=in:st=5.0:d=0.35:alpha=1,fade=out:st=9.4:d=0.4:alpha=1[c2]; \
  [4:v]format=rgba,fade=in:st=14.3:d=0.35:alpha=1,fade=out:st=16.5:d=0.4:alpha=1[c3]; \
  [v][k]overlay=0:0[a1];[a1][c1]overlay=0:0[a2];[a2][c2]overlay=0:0[a3]; \
  [a3][c3]overlay=0:0,format=yuv420p[o]" \
 -map "[o]" -t 17.2 -r $FPS $x264 "$WORK/1.mp4"

# ── 2. The phone, over the page it just hid (8.0 s) ──────────────────────────
# The backdrop is lifted from the end of take A so the crossfade lands on the
# same frame the previous beat finished on.
echo "beat 2: the phone"
ffmpeg -y -v error -ss 18.3 -i "$A" -frames:v 1 "$WORK/base.png"
magick "$WORK/base.png" -modulate 52 -blur 0x3 "$WORK/base_dim.png"
# One decode pass with trim, never input seeking: screenrecord keyframes are
# sparse and seeking lands on black frames at the cut points.
ffmpeg -y -v error -i "$P" -loop 1 -i "$ASSETS/pipmask.png" \
  -loop 1 -i "$WORK/base_dim.png" \
  -loop 1 -i "$ASSETS/c_phone.png" -loop 1 -i "$ASSETS/c_phone2.png" \
 -filter_complex "\
  [0:v]crop=1968:2184:0:168,scale=847:940:flags=lanczos,fps=$FPS,format=rgba, \
       trim=1.2:9.2,setpts=PTS-STARTPTS[ph]; \
  [1:v]format=gray[m];[ph][m]alphamerge[pm]; \
  [2:v]fps=$FPS,format=rgba[bg]; \
  [bg][pm]overlay=x='if(lt(t,0.7), 1920-937*(1-pow(1-t/0.7,3)), 983)':y=70:shortest=1[v1]; \
  [3:v]format=rgba,fade=in:st=0.8:d=0.35:alpha=1,fade=out:st=3.6:d=0.35:alpha=1[c1]; \
  [v1][c1]overlay=0:0[v2]; \
  [4:v]format=rgba,fade=in:st=4.6:d=0.35:alpha=1,fade=out:st=7.4:d=0.4:alpha=1[c2]; \
  [v2][c2]overlay=0:0,format=yuv420p[o]" \
 -map "[o]" -t 8.0 -r $FPS $x264 "$WORK/2.mp4"

# ── 3. The other PC (13.9 s) ─────────────────────────────────────────────────
echo "beat 3: the other PC"
ffmpeg -y -v error -i "$B" \
  -loop 1 -i "$ASSETS/c_remote.png" -loop 1 -i "$ASSETS/c_remote2.png" \
 -filter_complex "\
  [0:v]trim=1.5:15.4,setpts=PTS-STARTPTS,fps=$FPS,scale=$W:$H[v]; \
  [1:v]format=rgba,fade=in:st=0.6:d=0.35:alpha=1,fade=out:st=4.2:d=0.4:alpha=1[c1]; \
  [2:v]format=rgba,fade=in:st=8.4:d=0.35:alpha=1,fade=out:st=12.8:d=0.4:alpha=1[c2]; \
  [v][c1]overlay=0:0[b1];[b1][c2]overlay=0:0,format=yuv420p[o]" \
 -map "[o]" -t 13.9 -r $FPS $x264 "$WORK/3.mp4"

# ── 4 and 5. Wrap card and the logo blast ────────────────────────────────────
# There is no iPhone beat: the iOS app is simulator-only and cannot be recorded
# from Linux, so the phone caption and the wrap card carry it in words. When it
# ships, cut a real take and add it between beats 3 and 4.
echo "beats 4-5: wrap and logo"
ffmpeg -y -v error -loop 1 -i "$ASSETS/wrapcard.png" -t 2.8 -r $FPS \
 -vf "format=yuv420p,scale=$W:$H" $x264 "$WORK/4.mp4"
ffmpeg -y -v error -framerate $FPS -i "$ASSETS/logoframes/f%03d.png" -t 3.6 -r $FPS \
 -vf "format=yuv420p,scale=$W:$H" -c:v libx264 -crf 16 -preset medium "$WORK/5.mp4"

# ── Assemble ─────────────────────────────────────────────────────────────────
echo "assembling"
mkdir -p "$(dirname "$OUT")"
ffmpeg -y -v error -i "$WORK/1.mp4" -i "$WORK/2.mp4" -i "$WORK/3.mp4" \
  -i "$WORK/4.mp4" -i "$WORK/5.mp4" \
 -filter_complex "[0:v][1:v]xfade=transition=fade:duration=0.4:offset=16.8[x1]; \
                  [x1][2:v]xfade=transition=fade:duration=0.4:offset=24.4[x2]; \
                  [x2][3:v]xfade=transition=fade:duration=0.5:offset=37.8[x3]; \
                  [x3][4:v]xfade=transition=fade:duration=0.5:offset=40.1,format=yuv420p[v]" \
 -map "[v]" -r $FPS -c:v libx264 -crf 18 -preset slow -pix_fmt yuv420p \
 -movflags +faststart "$OUT"

# The closing lockup makes the poster: the mark, before anyone presses play.
ffmpeg -y -v error -ss 41.5 -i "$OUT" -frames:v 1 -q:v 80 "${OUT%.mp4}-poster.webp"

if [ "$WEB" = "--web" ]; then
  echo "web copy"
  ffmpeg -y -v error -i "$OUT" -c:v libx264 -crf 22 -preset slow -pix_fmt yuv420p \
    -movflags +faststart "$REPO_DIR/docs/video/super-desktop-trailer.mp4"
  cp "${OUT%.mp4}-poster.webp" "$REPO_DIR/docs/video/super-desktop-trailer-poster.webp"
fi

ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$OUT" \
  | xargs printf '%s  %s\n' "$OUT"
