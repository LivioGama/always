#!/usr/bin/env python3
"""Synthesise the Always UI sound families (original audio, no samples).

Usage:  python3 gen.py [outdir] [family ...]
Needs:  python3 + numpy, ffmpeg on PATH.
Output: <outdir>/<family>/<event>.aiff  (48 kHz, 24-bit PCM, stereo, matches
        the /System/Library/Sounds/*.aiff format Always plays today via afplay)

Every sound is three layers:
  1. transient  - 2-4 ms band-passed noise tick (tactile "touch", no click)
  2. body       - tuned partials, each as a micro-detuned L/R pair (warmth, width)
  3. air        - short decorrelated-noise convolution tail (stereo shimmer)
Levels are normalised to TARGET_MAX_M (max momentary loudness, EBU R128).
"""
import os, subprocess, sys
import numpy as np

SR = 48000
TARGET_MAX_M = -30.0      # LUFS momentary max, see README "Loudness"
PEAK_CEIL_DB = -12.0      # leaves +6 dB headroom for Always' afplay -v 2.0 ("high")
RNG = np.random.default_rng(7)  # deterministic output

def hz(note):
    names = {"C": -9, "C#": -8, "D": -7, "D#": -6, "E": -5, "F": -4, "F#": -3,
             "G": -2, "G#": -1, "A": 0, "A#": 1, "B": 2}
    n, o = note[:-1], int(note[-1])
    return 440.0 * 2 ** ((names[n] + 12 * (o - 4)) / 12)

def cents(c):
    return 2 ** (c / 1200)

# ---------------------------------------------------------------- primitives
def env(n, attack_ms, tau_ms, hold_ms=0.0):
    t = np.arange(n) / SR * 1000
    a = np.clip(t / attack_ms, 0, 1)
    a = 0.5 - 0.5 * np.cos(np.pi * a)                      # raised-cosine attack
    d = np.exp(-np.clip(t - attack_ms - hold_ms, 0, None) / tau_ms)
    return a * d

def partial(n, f, amp, attack_ms, tau_ms, detune_c=0.0, glide=None, phase=0.0, hold_ms=0.0):
    """Stereo partial. detune_c splits L/R by +-detune_c cents. glide=(ratio_start, ms)."""
    t = np.arange(n) / SR
    out = np.zeros((n, 2))
    if glide:
        r0, gms = glide
        g = r0 + (1 - r0) * (1 - np.exp(-t * 1000 / gms))   # exponential settle onto pitch
    else:
        g = np.ones(n)
    ph = lambda c: 2 * np.pi * np.cumsum(f * cents(c) * g) / SR + phase
    centre = np.sin(ph(0.0))
    # R2: both channels keep the in-tune centre; only a -9 dB detuned "chorus" voice
    # differs L vs R. Mono sum never nulls (R1 beat-null at ~80 ms), width stays gentle.
    for ch, sgn in ((0, -1), (1, 1)):
        out[:, ch] = centre + 0.35 * np.sin(ph(sgn * detune_c))
    out /= 1.35
    return out * (amp * env(n, attack_ms, tau_ms, hold_ms))[:, None]

def fm_partial(n, f, amp, ratio, index, idx_tau_ms, attack_ms, tau_ms, detune_c=0.0):
    """Two-op FM: bright onset that mellows (index decays) - glass/bell character."""
    t = np.arange(n) / SR
    out = np.zeros((n, 2))
    idx = index * np.exp(-t * 1000 / idx_tau_ms)
    op = lambda fc: np.sin(2 * np.pi * fc * t + idx * np.sin(2 * np.pi * fc * ratio * t))
    centre = op(f)
    # R3: same centre + detuned-chorus topology as partial() (R2 left FM at LR_corr 0.56)
    for ch, sgn in ((0, -1), (1, 1)):
        out[:, ch] = centre + 0.35 * op(f * cents(sgn * detune_c))
    out /= 1.35
    return out * (amp * env(n, attack_ms, tau_ms))[:, None]

def bandnoise(n, lo, hi):
    x = RNG.standard_normal(n)
    X = np.fft.rfft(x)
    fr = np.fft.rfftfreq(n, 1 / SR)
    X[(fr < lo) | (fr > hi)] = 0
    y = np.fft.irfft(X, n)
    return y / (np.abs(y).max() + 1e-12)

def transient(n, amp, lo=2500, hi=7000, ms=3.0):
    m = int(SR * ms / 1000) * 4
    x = np.zeros((n, 2))
    if amp <= 0:
        return x
    for ch in (0, 1):
        b = bandnoise(m, lo, hi) * env(m, 0.4, ms / 2.5)
        x[:m, ch] = b * amp
    return x

def lowpass(x, fc):
    X = np.fft.rfft(x, axis=0)
    fr = np.fft.rfftfreq(x.shape[0], 1 / SR)
    X *= (1 / np.sqrt(1 + (fr / fc) ** 4))[:, None]          # 2nd-order-ish magnitude
    return np.fft.irfft(X, x.shape[0], axis=0)

def air(x, wet, decay_ms, bright=6000):
    """Short stereo 'room' via convolution with decorrelated exponential noise IRs."""
    if wet <= 0:
        return x
    m = int(SR * decay_ms * 5 / 1000)
    ir = np.stack([bandnoise(m, 300, bright), bandnoise(m, 300, bright)], 1)
    ir *= np.exp(-np.arange(m) / SR * 1000 / decay_ms)[:, None]
    ir[: int(SR * 0.004)] *= np.linspace(0, 1, int(SR * 0.004))[:, None]  # 4 ms pre-delay ramp
    ir /= np.sqrt((ir ** 2).sum(0))
    n = x.shape[0] + m
    y = np.zeros((n, 2))
    y[: x.shape[0]] = x
    for ch in (0, 1):
        y[:, ch] += wet * np.fft.irfft(np.fft.rfft(x[:, ch], n) * np.fft.rfft(ir[:, ch], n), n)
    return y

def place(buf, x, at_ms):
    i = int(SR * at_ms / 1000)
    buf[i:i + x.shape[0]] += x[: buf.shape[0] - i]
    return buf

def width(x, w):
    """Mid/side width: w=0 mono, 1 unchanged."""
    m = (x[:, 0] + x[:, 1]) / 2
    s = (x[:, 0] - x[:, 1]) / 2 * w
    return np.stack([m + s, m - s], 1)

def finish(x, tail_db=-54):  # R5: trim file at -54 dB rel (was -60)
    a = np.abs(x).max(1)
    pk = a.max()
    start = int(np.argmax(a > pk * 1e-4))                   # trim to first sample > -80 dB rel
    x = x[start:]
    a = a[start:]
    end = np.where(a > pk * 10 ** (tail_db / 20))[0][-1] + int(SR * 0.004)
    x = x[: min(end, len(x))]
    f = int(SR * 0.004)
    x[-f:] *= np.linspace(1, 0, f)[:, None]
    return x

# ---------------------------------------------------------------- note voices
def voice(fam, f, n_ms, at=0.0, amp=1.0, **kw):
    """One struck note in a family's timbre."""
    p = dict(fam["voice"]); p.update(kw)
    # R4: voice buffer always long enough for ~-60 dB natural decay (R3 still faded at -45 dB)
    n_ms = max(n_ms, p["attack"] + p.get("hold", 0) + 7 * p["tau"])
    n = int(SR * n_ms / 1000)
    x = np.zeros((n, 2))
    if p.get("fm"):
        x += fm_partial(n, f, amp, p["fm_ratio"], p["fm_index"], p["fm_idx_tau"],
                        p["attack"], p["tau"], p["detune"])
    else:
        x += partial(n, f, amp, p["attack"], p["tau"], p["detune"], p.get("glide"), hold_ms=p.get("hold", 0))
    for ratio, rel_db, tau_scale in p["overtones"]:
        # R3: overtones follow the fundamental's glide (R2 cancel warbled without it)
        x += partial(n, f * ratio, amp * 10 ** (rel_db / 20), p["attack"] * 0.7,
                     p["tau"] * tau_scale, p["detune"] * 1.5, None if p.get("fm") else p.get("glide"))
    x += transient(n, amp * 10 ** (p["tick_db"] / 20), *p.get("tick_band", (2500, 7000)))
    # R3: 12 ms raised-cosine fade at the end of every voice buffer (R2 truncated tails)
    k = int(SR * 0.012)
    x[-k:] *= (0.5 + 0.5 * np.cos(np.linspace(0, np.pi, k)))[:, None]
    return x, at

def render(fam, notes, total_ms):
    voices = [(voice(fam, f, dur, amp=amp, **kw)[0], at) for f, dur, at, amp, kw in notes]
    total = max([total_ms] + [at + x.shape[0] / SR * 1000 for x, at in voices])
    buf = np.zeros((int(SR * total / 1000) + 1, 2))
    for x, at in voices:
        place(buf, x, at)
    buf = lowpass(buf, fam.get("lp", 9000))
    buf = air(buf, fam["air_wet"], fam["air_ms"], fam.get("air_bright", 6000))
    buf = width(buf, fam.get("width", 1.0))
    return finish(buf)

# ---------------------------------------------------------------- families
def N(note, dur=400, at=0, amp=1.0, **kw):
    return (hz(note) if isinstance(note, str) else note, dur, at, amp, kw)

FAMILIES = {}

# Lead: "halo" - warm sine glass, key of D major (D5 = 587 Hz tonic).
# start = question (D5 -> A5, rising fifth), stop = answer (A5 -> D5, falling fifth)
FAMILIES["halo"] = dict(
    # R5: tau 90->72, hold 15->10 (shorter mic-bleed tail). R2: tau 70->90, hold 8->15, detune 3->4
    voice=dict(attack=3.0, tau=72.0, hold=10.0, detune=4.0, tick_db=-30, tick_band=(3000, 8000),
               overtones=[(2.0, -24, 0.6), (3.0, -36, 0.4), (4.02, -40, 0.25)]),
    lp=8000, air_wet=0.13, air_ms=32, air_bright=7000, width=0.8,  # R4: air 55->32 ms
    events=dict(
        start=([N("D5", 300, 0, 0.55), N("A5", 400, 45, 1.0)], 400),   # R5: gap 55->45 ms
        stop=([N("A5", 300, 0, 0.55), N("D5", 400, 45, 1.0)], 400),
        done=([N("D5", 300, 0, 0.45), N("F#5", 300, 38, 0.55), N("A5", 450, 76, 0.85),
               N("D6", 500, 76, 0.18, tick_db=-90)], 650),
        error=([N("F4", 350, 0, 0.9, tau=50, tick_db=-36, overtones=[(2.0, -14, 0.5), (3.0, -26, 0.4)]),
                N("D4", 450, 110, 1.0, tau=62, tick_db=-36, overtones=[(2.0, -14, 0.5), (3.0, -26, 0.4)])], 650),
        cancel=([N("A5", 300, 0, 0.8, tau=55, hold=0, glide=(1.06, 45.0))], 360),
        command=([N("E6", 450, 0, 0.55, fm=True, fm_ratio=2.0, fm_index=1.2, fm_idx_tau=25),
                  N("A5", 450, 0, 0.75, fm=True, fm_ratio=2.0, fm_index=1.2, fm_idx_tau=25),
                  N("E6", 400, 70, 0.6, fm=True, fm_ratio=2.0, fm_index=1.0, fm_idx_tau=25)], 600),
    ),
)

# Alt 1: "wood" - soft marimba (modal partials 1 : 3.93 : 9.2), key of F major.
FAMILIES["wood"] = dict(
    voice=dict(attack=1.5, tau=60.0, detune=2.0, tick_db=-22, tick_band=(1500, 5000),
               overtones=[(3.93, -16, 0.25), (9.2, -30, 0.1)]),
    lp=6000, air_wet=0.08, air_ms=20, air_bright=5000, width=0.6,
    events=dict(
        start=([N("F5", 300, 0, 0.6), N("C6", 350, 60, 1.0)], 450),
        stop=([N("C6", 300, 0, 0.6), N("F5", 350, 60, 1.0)], 450),
        done=([N("F5", 300, 0, 0.5), N("A5", 300, 45, 0.6), N("C6", 400, 90, 0.9)], 550),
        error=([N("A#4", 300, 0, 0.9, tau=50), N("G4", 400, 120, 1.0, tau=60)], 600),
        cancel=([N("C5", 250, 0, 0.8, tau=35)], 280),
        command=([N("F6", 350, 0, 0.7, tau=50), N("C6", 350, 0, 0.6, tau=55), N("F6", 300, 65, 0.6, tau=45)], 500),
    ),
)

# Alt 2: "airy" - breathy synth: slower attack, upward pitch settle, wider room.
FAMILIES["airy"] = dict(
    voice=dict(attack=9.0, tau=95.0, hold=20.0, detune=6.0, tick_db=-90,
               glide=(0.985, 25.0), overtones=[(2.0, -28, 0.7), (3.0, -40, 0.5)]),
    lp=7000, air_wet=0.22, air_ms=55, air_bright=9000, width=1.0,
    events=dict(
        start=([N("E5", 350, 0, 0.6), N("B5", 450, 60, 1.0)], 600),
        stop=([N("B5", 350, 0, 0.6), N("E5", 450, 60, 1.0)], 600),
        done=([N("E5", 350, 0, 0.45), N("G#5", 350, 45, 0.55), N("B5", 500, 90, 0.85)], 700),
        error=([N("G4", 400, 0, 0.9, overtones=[(2.0, -16, 0.6)]), N("E4", 500, 130, 1.0, overtones=[(2.0, -16, 0.6)])], 750),
        cancel=([N("B5", 300, 0, 0.8, tau=55, glide=(1.02, 30.0))], 380),
        command=([N("B5", 450, 0, 0.7, fm=True, fm_ratio=3.0, fm_index=0.8, fm_idx_tau=40),
                  N("F#6", 450, 70, 0.6, fm=True, fm_ratio=3.0, fm_index=0.8, fm_idx_tau=40)], 650),
    ),
)

# Alt 3: "glass" - FM bell (inharmonic 1:3.5), crystalline, key of G major.
FAMILIES["glass"] = dict(
    voice=dict(attack=1.2, tau=85.0, detune=4.0, tick_db=-90, fm=True, fm_ratio=3.5, fm_index=1.6,
               fm_idx_tau=18, overtones=[]),
    lp=9000, air_wet=0.15, air_ms=28, air_bright=10000, width=0.8,
    events=dict(
        start=([N("G5", 350, 0, 0.6), N("D6", 450, 55, 1.0)], 550),
        stop=([N("D6", 350, 0, 0.6), N("G5", 450, 55, 1.0)], 550),
        done=([N("G5", 350, 0, 0.45), N("B5", 350, 40, 0.55), N("D6", 500, 80, 0.85)], 650),
        error=([N("C5", 400, 0, 0.9, fm_ratio=1.5, fm_index=1.0), N("A4", 450, 120, 1.0, fm_ratio=1.5, fm_index=1.0)], 650),
        cancel=([N("D5", 280, 0, 0.8, tau=45, fm_index=0.8)], 330),
        command=([N("A6", 400, 0, 0.6, fm_ratio=2.0, fm_index=1.0), N("D6", 400, 0, 0.7, fm_ratio=2.0, fm_index=1.0),
                  N("A6", 350, 65, 0.6, fm_ratio=2.0, fm_index=0.9)], 550),
    ),
)

# ---------------------------------------------------------------- io
def max_momentary(path):
    out = subprocess.run(["ffmpeg", "-hide_banner", "-nostats", "-i", path, "-af",
                          "apad=pad_dur=0.5,ebur128", "-f", "null", "-"],
                         capture_output=True, text=True).stderr
    ms = [float(l.split("M:")[1].split()[0]) for l in out.splitlines() if " M:" in l]
    return max(ms)

def write(path, x):
    tmp = path + ".f32"
    x.astype("<f4").tofile(tmp)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "f32le", "-ar", str(SR), "-ac", "2",
                    "-i", tmp, "-c:a", "pcm_s24be", path], check=True)
    os.remove(tmp)

def build(outdir, only=None):
    for name, fam in FAMILIES.items():
        if only and name not in only:
            continue
        os.makedirs(os.path.join(outdir, name), exist_ok=True)
        for ev, (notes, total) in fam["events"].items():
            x = render(fam, notes, total)
            x /= np.abs(x).max()
            p = os.path.join(outdir, name, f"{ev}.aiff")
            x0 = x * 10 ** (-20 / 20)
            write(p, x0)
            g = TARGET_MAX_M - max_momentary(p)
            gain = min(10 ** ((g - 20) / 20), 10 ** (PEAK_CEIL_DB / 20))  # never exceed peak ceiling
            write(p, x * gain)
            print(f"{name}/{ev}.aiff  {len(x)/SR*1000:.0f} ms")

if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    out = args[0] if args else os.path.dirname(os.path.abspath(__file__))
    only = args[1:] or None
    build(out, only)
