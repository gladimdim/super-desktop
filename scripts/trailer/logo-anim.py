#!/usr/bin/env python3
"""Closing animation: SUPER flies in from the left, DESKTOP from the right,
they meet on the bolt and it blasts.

108 frames at 30 fps (3.6 s), written to <assets>/logoframes/. Run through
make-assets.sh, which generates the bolt, wordmarks, rays and flash it needs.

The starburst is composited *before* the bolt and wordmark: drawn on top it
reads as spokes crossing the letters rather than light coming from behind.
"""
import math
import os
import shutil
import subprocess
import sys

ASSETS = sys.argv[1] if len(sys.argv) > 1 else "."
OUT = os.path.join(ASSETS, "logoframes")
shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)

N, W, H = 108, 1920, 1080
BOLT_W, BOLT_H = 472, 712          # boltglow.png, glow included
FINAL_BOLT_H = 430
CX, CY = 960, 505                  # bolt centre
SUPER_W, DESK_W, TXT_H = 417, 592, 182
GAP = 56
FLY_A, FLY_B = 10, 45              # wordmarks fly in over these frames
BLAST = 45                         # and land on the blast

sx_end = CX - FINAL_BOLT_H * BOLT_W / BOLT_H / 2 - GAP - SUPER_W
dx_end = CX + FINAL_BOLT_H * BOLT_W / BOLT_H / 2 + GAP
ty = CY - TXT_H // 2
sx_start, dx_start = -SUPER_W - 80, W + 80


def ease_out(p):
    return 1 - (1 - p) ** 3


def clamp(v, a=0.0, b=1.0):
    return max(a, min(b, v))


def asset(name):
    return os.path.join(ASSETS, name)


for f in range(N):
    args = ["magick", asset("bg_end.png")]

    # starburst, behind everything
    if BLAST <= f < BLAST + 13:
        k = (f - BLAST) / 13
        rs = 900 + 1500 * ease_out(k)
        args += ["(", asset("rays.png"), "-resize", f"{rs:.0f}x{rs:.0f}!",
                 "-alpha", "set", "-channel", "A", "-evaluate", "multiply",
                 f"{(1 - k) ** 2.0 * 0.75:.3f}", "+channel", ")",
                 "-geometry", f"+{CX - rs / 2:.0f}+{CY - rs / 2:.0f}", "-composite"]

    # the bolt: fades in, takes the hit, overshoots, settles, then breathes
    if f < 12:
        bs, ba = 0.72 + 0.16 * (f / 12), clamp(f / 9)
    elif f < BLAST:
        bs, ba = 0.88 + 0.04 * ((f - 12) / (BLAST - 12)), 1.0
    elif f < 50:
        bs, ba = 0.92 + 0.40 * ((f - BLAST) / 5), 1.0
    elif f < 72:
        bs, ba = 1.32 - 0.32 * ease_out((f - 50) / 22), 1.0
    else:
        bs, ba = 1.0 + 0.012 * math.sin((f - 72) / 36 * math.pi * 2), 1.0
    bh = FINAL_BOLT_H * bs
    bw = bh * BOLT_W / BOLT_H
    args += ["(", asset("boltglow.png"), "-resize", f"{bw:.0f}x{bh:.0f}!",
             "-alpha", "set", "-channel", "A", "-evaluate", "multiply", f"{ba:.3f}",
             "+channel", ")",
             "-geometry", f"+{CX - bw / 2:.0f}+{CY - bh / 2:.0f}",
             "-compose", "over", "-composite"]

    # the two halves meet in the middle
    p = ease_out(clamp((f - FLY_A) / (FLY_B - FLY_A)))
    for img, x in ((asset("w_super.png"), sx_start + (sx_end - sx_start) * p),
                   (asset("w_desktop.png"), dx_start + (dx_end - dx_start) * p)):
        args += ["(", img, "-alpha", "set", "-channel", "A", "-evaluate", "multiply",
                 f"{clamp(p * 1.6):.3f}", "+channel", ")",
                 "-geometry", f"+{x:.0f}+{ty}", "-composite"]

    # the flash, on top of everything
    if BLAST <= f < BLAST + 9:
        k = (f - BLAST) / 9
        fs = 1100 + 1400 * k
        args += ["(", asset("flash.png"), "-resize", f"{fs:.0f}x{fs:.0f}!",
                 "-alpha", "set", "-channel", "A", "-evaluate", "multiply",
                 f"{(1 - k) ** 1.5 * 0.95:.3f}", "+channel", ")",
                 "-geometry", f"+{CX - fs / 2:.0f}+{CY - fs / 2:.0f}", "-composite"]

    args += [os.path.join(OUT, f"f{f:03d}.png")]
    subprocess.run(args, check=True)

print(f"  {len(os.listdir(OUT))} frames -> {OUT}")
