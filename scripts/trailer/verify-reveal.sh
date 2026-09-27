#!/usr/bin/env bash
# Checks that the overlay reveal in a take plays once.
#
#   ./verify-reveal.sh ~/Videos/SuperDesktop/super-desktop-trailer-footage/desktop-raw-a.mkv
#   ./verify-reveal.sh take.mkv 2.0 3.4 0.4      # window start, end, reference
#
# The reveal can play twice: the overlay settles, a chunk of it drops out for a
# few frames, then animates in again. At 60 fps it is a ~100 ms stutter that is
# easy to miss by eye and obvious once measured.
#
# Every frame in the window is compared against a reference frame taken while
# the overlay is still hidden, so the distance rises as the overlay appears. A
# single reveal rises monotonically and then stays flat; a double dips after
# reaching the plateau.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

VIDEO=${1:?usage: verify-reveal.sh VIDEO [START END REF_T]}
START=${2:-1.9}
END=${3:-3.4}
REF_T=${4:-0.4}

WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
ffmpeg -y -v error -ss "$START" -t "$(python3 -c "print($END-$START)")" -i "$VIDEO" \
  -vf "fps=60,scale=192:108" "$WORK/f%04d.png"
ffmpeg -y -v error -ss "$REF_T" -i "$VIDEO" -frames:v 1 -vf "scale=192:108" "$WORK/ref.png"

WORK="$WORK" START="$START" python3 - <<'PY'
import glob, os, subprocess, sys

work, start = os.environ["WORK"], float(os.environ["START"])
vals = []
for i, f in enumerate(sorted(glob.glob(f"{work}/f*.png"))):
    out = subprocess.run(["magick", "compare", "-metric", "RMSE", f"{work}/ref.png", f, "null:"],
                         capture_output=True, text=True).stderr.split()[0]
    vals.append((start + i / 60.0, float(out) / 1000.0))

peak = max(v for _, v in vals)
if peak < 1.0:
    sys.exit("no reveal found in this window — check START/END and the reference time")

# Once within 15% of the peak the overlay is up; after that any real drop is a
# second animation rather than terminal output changing under it.
settled = next(i for i, (_, v) in enumerate(vals) if v >= peak * 0.85)
worst_t, worst_drop = None, 0.0
for (t, v) in vals[settled:]:
    drop = (peak - v) / peak
    if drop > worst_drop:
        worst_t, worst_drop = t, drop

print(f"  reveal completes at {vals[settled][0]:.2f}s, plateau {peak:.1f}")
if worst_drop > 0.08:
    print(f"  FAIL  drops {worst_drop*100:.0f}% at {worst_t:.2f}s — the reveal plays twice")
    print("        reshoot after two warm-up hide/show cycles (capture-desktop.sh does this)")
    print("  " + " ".join(f"{t:.2f}:{v:.1f}" for t, v in vals))
    sys.exit(1)
print(f"  PASS  single reveal, largest wobble after settling {worst_drop*100:.1f}%")
PY
