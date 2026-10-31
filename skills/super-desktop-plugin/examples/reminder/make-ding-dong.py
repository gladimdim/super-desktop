# Writes ding-dong.wav: two short "phone vibrating" buzzes, then a two-tone
# door chime (E5, then C5) with bell-like overtones and a soft decay.
# Usage: python3 make-ding-dong.py ding-dong.wav
import math, struct, sys, wave
RATE = 44100
out = []

def silence(sec):
    out.extend([0.0] * int(RATE * sec))

def buzz(sec):
    n = int(RATE * sec)
    for i in range(n):
        t = i / RATE
        env = min(1.0, i / (RATE * 0.01), (n - i) / (RATE * 0.02))
        # A rough low hum with a fast wobble, like a vibration motor.
        wobble = 0.6 + 0.4 * math.sin(2 * math.pi * 28 * t)
        v = math.tanh(2.5 * math.sin(2 * math.pi * 150 * t)) * 0.5 + 0.25 * math.sin(2 * math.pi * 300 * t)
        out.append(0.22 * env * wobble * v)

def bell(freq, sec, start):
    n = int(RATE * sec)
    partials = [(1.0, 1.0, 1.0), (2.0, 0.45, 1.6), (3.0, 0.22, 2.4), (4.2, 0.12, 3.2)]
    base = int(RATE * start)
    while len(out) < base + n:
        out.append(0.0)
    for i in range(n):
        t = i / RATE
        attack = min(1.0, i / (RATE * 0.004))
        v = sum(a * math.exp(-t * 2.2 * d) * math.sin(2 * math.pi * freq * r * t) for r, a, d in partials)
        out[base + i] += 0.28 * attack * v

buzz(0.18); silence(0.09); buzz(0.18); silence(0.25)
start = len(out) / RATE
bell(659.25, 1.6, start)          # ding
bell(523.25, 2.0, start + 0.55)   # dong

peak = max(abs(x) for x in out) or 1
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(RATE)
    w.writeframes(b"".join(struct.pack("<h", int(32767 * 0.85 * x / peak)) for x in out))
