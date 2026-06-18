# Sound reference capture rig

Tools for comparing Phosphor's discrete sound output against MAME's, by driving
both through an identical timeline of register writes and measuring the result.
Born out of the Asteroids migration — see the case study in
[docs/debugging-asteroids-discrete-sound.md](../../docs/debugging-asteroids-discrete-sound.md).

## Contents

- `drive_asteroid_sound.lua` — MAME autoboot Lua that pokes the Asteroids sound
  registers on a timeline (one effect per 2 s window). Attract mode is silent, so
  it just drives the latches; no CPU halting needed.
- `analyze_wav.py` — segments a capture by time and prints, per effect, the
  dominant FFT peak and the spectral centroid (DC removed). Reads WAVs with the
  stdlib `wave` module; needs only `numpy`.
- The Phosphor side lives in
  [`machines/examples/asteroid_capture.rs`](../../machines/examples/asteroid_capture.rs),
  which drives `AsteroidsDiscreteSound` through the same timeline.

## Usage

```bash
# 1. MAME reference (run from the MAME working dir with the asteroid romset)
mame asteroid -nothrottle -seconds_to_run 18 -video none \
     -autoboot_script $(pwd)/tools/sound-reference/drive_asteroid_sound.lua \
     -wavwrite /tmp/asteroid_ref.wav

# 2. Phosphor capture (writes /tmp/phosphor_asteroid.wav)
cargo run -p phosphor-machines --example asteroid_capture

# 3. Compare (numpy in a venv)
python3 -m venv /tmp/sndvenv && /tmp/sndvenv/bin/pip install -q numpy
/tmp/sndvenv/bin/python tools/sound-reference/analyze_wav.py \
    /tmp/asteroid_ref.wav /tmp/phosphor_asteroid.wav
```

The two tables should line up per effect (compare the centroid column; the single
peak bin is unstable for swept tones).

## Adapting to another board

For Lunar Lander, Donkey Kong, etc.: copy the Lua driver and swap the register
map + timeline, change the `SEGMENTS` list in `analyze_wav.py` to match, and add a
sibling capture example. Notes on MAME 0.287 gotchas (silent attract, deprecated
`emu.register_start`, empty `sound_map` node routing) are in the case study.
