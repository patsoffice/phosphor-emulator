# Sound reference capture rig

Tools for comparing Phosphor's discrete sound output against MAME's, by driving
both through an identical timeline of register writes and measuring the result.
Born out of the Asteroids migration — see the case study in
[docs/debugging-asteroids-discrete-sound.md](../../docs/debugging-asteroids-discrete-sound.md).

## Contents

- `drive_asteroid_sound.lua` / `drive_llander_sound.lua` / `drive_dkong_sound.lua`
  — MAME autoboot Lua that pokes a board's sound registers on a timeline (one
  effect per window). The Atari boards have silent attract modes so they just
  drive the registers; the DK driver parks the main Z80 in a spin loop to isolate
  the discrete walk/jump/stomp from the DAC music.
- `analyze_wav.py` — segments a capture by time and prints, per effect, the
  dominant FFT peak and the spectral centroid (DC removed). Reads WAVs with the
  stdlib `wave` module; needs only `numpy`. Pass `--llander` / `--dkong` (or
  `--segments=a:b:label,...`) to select a board's timeline.
- The Phosphor side lives in the `machines/examples/` capture binaries
  ([`asteroid_capture.rs`](../../machines/examples/asteroid_capture.rs),
  [`llander_capture.rs`](../../machines/examples/llander_capture.rs),
  [`dkong_capture.rs`](../../machines/examples/dkong_capture.rs)), which drive the
  device through the same timeline.

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

Lunar Lander is the same flow with the `llander` driver/example and `--llander`:

```bash
mame llander -nothrottle -seconds_to_run 12 -video none \
     -autoboot_script $(pwd)/tools/sound-reference/drive_llander_sound.lua \
     -wavwrite /tmp/llander_ref.wav
cargo run -p phosphor-machines --example llander_capture
/tmp/sndvenv/bin/python tools/sound-reference/analyze_wav.py --llander \
    /tmp/llander_ref.wav /tmp/llander_phosphor.wav
```

Galaxian uses the `galaxian` driver/example and `--galaxian`. The Lua parks the
main Z80 (so the running game stops writing its own sound registers) and pets the
watchdog, then drives the discrete board's registers (pitch 0x7800, LFO
0x6004-7, sound latch 0x6800-7) one voice per window: tune, wolf-whistle, fire,
hit:

```bash
mame galaxian -nothrottle -seconds_to_run 10 -video none \
     -autoboot_script $(pwd)/tools/sound-reference/drive_galaxian_sound.lua \
     -wavwrite /tmp/galaxian_ref.wav
cargo run -p phosphor-machines --example galaxian_capture
/tmp/sndvenv/bin/python tools/sound-reference/analyze_wav.py --galaxian \
    /tmp/galaxian_ref.wav /tmp/galaxian_phosphor.wav
```

The two tables should line up per effect (compare the centroid column; the single
peak bin is unstable for swept tones).

## Adapting to another board

For Lunar Lander, Donkey Kong, etc.: copy the Lua driver and swap the register
map + timeline, change the `SEGMENTS` list in `analyze_wav.py` to match, and add a
sibling capture example. Notes on MAME 0.287 gotchas (silent attract, deprecated
`emu.register_start`, empty `sound_map` node routing) are in the case study.
