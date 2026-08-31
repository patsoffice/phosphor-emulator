#!/usr/bin/env python3
"""Compare two captures of the same session — ours against a MAME reference.

`disasm audiodiff` answers "how do these two captures of one effect differ", for
a *register-driven* scenario whose window is declared in advance, and it is the
documented path for anything repeatable. This answers a different question, which
is why it survived the rig it came from: given two recordings of the *same
gameplay*, where do they
differ, and in what way?

That question is what a discrete-sound migration actually runs into. A netlist
can be wrong in several independent ways that a single number cannot separate:

  * the SOURCE is wrong          — LFSR taps, oscillator divider, envelope shape
  * the FILTER is wrong          — RC corner, Q, order
  * the MIX is wrong             — one voice too loud relative to the others
  * the OUTPUT STAGE is wrong    — overall gain, DC offset, clipping

The views below are chosen to tell those apart:

  band energy   a mix or filter error moves energy between bands; a gain error
                does not (it scales every band equally)
  centroid      brightness in one number, for a quick regression check
  flatness      tone vs noise: an LFSR with a short period reads as a tone
  RMS envelope  attack and decay, which a windowed spectrum averages away
  spectrogram   everything the numbers miss — look at it

Usage:
    compare_wav.py OURS.wav MAME.wav [--label-a ours] [--label-b mame]
                   [--png OUT.png] [--events]

Needs numpy; --png additionally needs matplotlib. On NixOS:
    nix-shell -p 'python3.withPackages(ps: [ps.numpy ps.matplotlib])'
"""

import argparse
import sys
import wave

import numpy as np

BAND_EDGES = [0, 150, 400, 1000, 3000, 8000]


def load(path):
    """Read a mono float64 signal and its rate; stereo is averaged down."""
    with wave.open(path) as w:
        if w.getsampwidth() != 2:
            sys.exit(f"{path}: expected 16-bit PCM, got {w.getsampwidth()*8}-bit")
        data = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float64)
        if w.getnchannels() == 2:
            data = data.reshape(-1, 2).mean(axis=1)
        return data, w.getframerate()


def spectrum(x):
    x = x - x.mean()
    return np.abs(np.fft.rfft(x * np.hanning(len(x))))


def centroid(x, rate):
    sp = spectrum(x)
    freqs = np.fft.rfftfreq(len(x), 1 / rate)
    return float((freqs * sp).sum() / max(sp.sum(), 1e-9))


def flatness(x):
    """Geometric over arithmetic mean power: ~0 for a tone, ->1 for noise.

    Separates a bell from a rumble where the centroid cannot — a wrongly tapped
    LFSR collapses to a short period and buzzes like a tone while keeping a
    plausible centroid.
    """
    p = spectrum(x) ** 2 + 1e-12
    return float(np.exp(np.log(p).mean()) / p.mean())


def band_energy(x, rate):
    """Percent of total energy per band. Scale-invariant, so a pure gain
    difference leaves it unchanged and a filter or mix error does not."""
    power = spectrum(x) ** 2
    freqs = np.fft.rfftfreq(len(x), 1 / rate)
    edges = BAND_EDGES + [rate / 2]
    total = max(power.sum(), 1e-9)
    return [
        100 * power[(freqs >= edges[i]) & (freqs < edges[i + 1])].sum() / total
        for i in range(len(edges) - 1)
    ]


def band_labels(rate):
    edges = BAND_EDGES + [rate / 2]
    return [f"{edges[i]:.0f}-{edges[i+1]:.0f}" for i in range(len(edges) - 1)]


def envelope(x, rate, hop_ms=50):
    hop = max(1, int(rate * hop_ms / 1000))
    n = len(x) // hop
    return np.sqrt((x[: n * hop].reshape(n, hop) ** 2).mean(axis=1)), hop / rate


def onsets(env, dt, rel=4.0, floor=200.0):
    """Frame indices where the envelope jumps — sound events.

    Onsets are what make two *gameplay* captures comparable at all: the two
    emulators drift apart in time, so fixed windows end up comparing different
    effects. Matching event N to event N is the alignment that survives drift.
    """
    out = []
    for i in range(1, len(env)):
        if env[i] > floor and env[i] > rel * max(env[i - 1], 1.0):
            if not out or (i - out[-1]) * dt > 0.15:
                out.append(i)
    return out


def summarise(name, x, rate):
    print(
        f"  {name:<6s} rms={np.sqrt((x**2).mean()):8.1f}  peak={np.abs(x).max():6.0f}"
        f"  centroid={centroid(x, rate):7.1f} Hz  flatness={flatness(x):.4f}"
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--label-a", default="ours")
    ap.add_argument("--label-b", default="mame")
    ap.add_argument("--png", help="write a spectrogram comparison here")
    ap.add_argument("--events", action="store_true", help="per-onset fingerprints")
    args = ap.parse_args()

    a, ra = load(args.a)
    b, rb = load(args.b)
    if ra != rb:
        sys.exit(
            f"sample rates differ ({ra} vs {rb}). Re-capture with a matching rate "
            f"— MAME takes -samplerate {ra}."
        )
    n = min(len(a), len(b))
    if abs(len(a) - len(b)) / max(len(a), len(b)) > 0.05:
        print(
            f"NOTE: lengths differ by {abs(len(a)-len(b))/ra:.2f} s; comparing the "
            f"first {n/ra:.2f} s of each.\n"
        )
    a, b = a[:n], b[:n]

    print(f"{n/ra:.2f} s at {ra} Hz\n")
    summarise(args.label_a, a, ra)
    summarise(args.label_b, b, ra)

    gain = np.sqrt((a**2).mean()) / max(np.sqrt((b**2).mean()), 1e-9)
    print(f"\n  overall level: {args.label_a} is {gain:.2f}x {args.label_b}")

    labels = band_labels(ra)
    ea, eb = band_energy(a, ra), band_energy(b, ra)
    print("\n  band energy %  " + "".join(f"{l:>12s}" for l in labels))
    print(f"  {args.label_a:<13s}" + "".join(f"{v:12.1f}" for v in ea))
    print(f"  {args.label_b:<13s}" + "".join(f"{v:12.1f}" for v in eb))
    print("  delta        " + "".join(f"{x-y:+12.1f}" for x, y in zip(ea, eb)))
    print(
        "\n  Band deltas are scale-invariant: a pure gain error leaves them at zero,\n"
        "  so anything large here is a filter, mix or source difference."
    )

    if args.events:
        env_a, dt = envelope(a, ra)
        env_b, _ = envelope(b, ra)
        oa, ob = onsets(env_a, dt), onsets(env_b, dt)
        print(f"\n  onsets: {len(oa)} in {args.label_a}, {len(ob)} in {args.label_b}")
        print(f"  {'#':>3s} {'t_a':>7s} {'t_b':>7s} {'drift':>7s} "
              f"{'cent_a':>8s} {'cent_b':>8s} {'flat_a':>7s} {'flat_b':>7s}")
        for i, (ia, ib) in enumerate(zip(oa, ob)):
            sa = a[int(ia * dt * ra) : int((ia * dt + 0.4) * ra)]
            sb = b[int(ib * dt * ra) : int((ib * dt + 0.4) * ra)]
            if len(sa) < 256 or len(sb) < 256:
                continue
            print(
                f"  {i:3d} {ia*dt:7.2f} {ib*dt:7.2f} {(ia-ib)*dt:+7.2f} "
                f"{centroid(sa, ra):8.1f} {centroid(sb, ra):8.1f} "
                f"{flatness(sa):7.4f} {flatness(sb):7.4f}"
            )

    if args.png:
        try:
            import matplotlib
            matplotlib.use("Agg")
            import matplotlib.pyplot as plt
        except ImportError:
            sys.exit("--png needs matplotlib")

        fig, axes = plt.subplots(3, 1, figsize=(14, 11), constrained_layout=True)
        for ax, x, label in ((axes[0], a, args.label_a), (axes[1], b, args.label_b)):
            ax.specgram(x, Fs=ra, NFFT=2048, noverlap=1536, cmap="magma")
            ax.set_ylim(0, 6000)
            ax.set_ylabel(f"{label}\nHz")
        env_a, dt = envelope(a, ra)
        env_b, _ = envelope(b, ra)
        t = np.arange(len(env_a)) * dt
        axes[2].plot(t, env_a, label=args.label_a, lw=0.9)
        axes[2].plot(t[: len(env_b)], env_b[: len(t)], label=args.label_b, lw=0.9)
        axes[2].set_xlabel("seconds")
        axes[2].set_ylabel("RMS")
        axes[2].legend(loc="upper right")
        axes[2].margins(x=0)
        fig.savefig(args.png, dpi=110)
        print(f"\n  wrote {args.png}")


if __name__ == "__main__":
    main()
