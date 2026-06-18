#!/usr/bin/env python3
"""Segment a timeline-driven sound capture and report, per effect, the dominant
pitch and spectral centroid. Works on both the MAME reference capture and a
Phosphor capture (machines/examples/asteroid_capture.rs) using the same timeline.

Only needs numpy (the WAV is read with the stdlib `wave` module):
    python3 -m venv /tmp/sndvenv && /tmp/sndvenv/bin/pip install numpy
    /tmp/sndvenv/bin/python analyze_wav.py ref.wav phosphor.wav
"""
import sys
import wave
import numpy as np

# (start_s, end_s, label) — matches drive_asteroid_sound.lua / asteroid_capture.rs.
ASTEROID_SEGMENTS = [
    (1, 3, "thrust"),
    (3, 5, "saucer_small"),
    (5, 7, "saucer_large"),
    (7, 9, "life"),
    (9, 11, "explosion"),
    (11, 13, "thump"),
    (13, 15, "ship_fire"),
    (15, 17, "saucer_fire"),
]

# matches drive_llander_sound.lua / llander_capture.rs.
LLANDER_SEGMENTS = [
    (1, 3, "thrust_full"),
    (3, 5, "thrust_low"),
    (5, 7, "tone3k"),
    (7, 9, "tone6k"),
    (9, 11, "explosion"),
]

# matches drive_dkong_sound.lua / dkong_capture.rs.
DKONG_SEGMENTS = [
    (1, 3, "walk"),
    (3, 5, "jump"),
    (5, 7, "stomp"),
]

SEGMENTS = ASTEROID_SEGMENTS  # default; override with --llander or --segments

def parse_segments(spec):
    out = []
    for part in spec.split(","):
        a, b, label = part.split(":")
        out.append((float(a), float(b), label))
    return out


def wavfile_read(path):
    with wave.open(path, "rb") as w:
        sr, nch, width = w.getframerate(), w.getnchannels(), w.getsampwidth()
        raw = w.readframes(w.getnframes())
    dtype = {1: np.int8, 2: np.int16, 4: np.int32}[width]
    data = np.frombuffer(raw, dtype=dtype)
    if nch > 1:
        data = data.reshape(-1, nch)[:, 0]
    return sr, data


def analyze(path, segments):
    sr, data = wavfile_read(path)
    data = data.astype(float)
    print(f"{path}  ({sr} Hz, {len(data) / sr:.1f}s)")
    print(f"{'effect':14s} {'peak Hz':>9s} {'centroid Hz':>12s} {'rms':>8s}")
    for a, b, name in segments:
        s, e = int((a + 0.4) * sr), int((b - 0.4) * sr)
        x = data[s:e]
        if len(x) < sr // 4:
            print(f"{name:14s}  (no data)")
            continue
        x = x - x.mean()  # remove DC so a stuck-DC signal reads as silence
        spec = np.abs(np.fft.rfft(x * np.hanning(len(x))))
        freq = np.fft.rfftfreq(len(x), 1 / sr)
        band = freq > 20
        peak = freq[band][np.argmax(spec[band])] if spec[band].size else 0.0
        centroid = np.sum(freq * spec) / np.sum(spec) if np.sum(spec) else 0.0
        rms = np.sqrt(np.mean(x**2))
        print(f"{name:14s} {peak:9.1f} {centroid:12.1f} {rms:8.0f}")


if __name__ == "__main__":
    args = sys.argv[1:]
    segments = ASTEROID_SEGMENTS
    paths = []
    for a in args:
        if a == "--llander":
            segments = LLANDER_SEGMENTS
        elif a == "--dkong":
            segments = DKONG_SEGMENTS
        elif a.startswith("--segments="):
            segments = parse_segments(a.split("=", 1)[1])
        else:
            paths.append(a)
    for p in paths or ["/tmp/asteroid_ref.wav"]:
        analyze(p, segments)
