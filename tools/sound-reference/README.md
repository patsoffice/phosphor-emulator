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
  stdlib `wave` module; needs only `numpy`. Pass `--llander` / `--dkong` /
  `--galaxian` (or `--segments=a:b:label,...`) to select a board's timeline.
- The Phosphor side lives in the `machines/examples/` capture binaries
  ([`asteroid_capture.rs`](../../machines/examples/asteroid_capture.rs),
  [`llander_capture.rs`](../../machines/examples/llander_capture.rs),
  [`dkong_capture.rs`](../../machines/examples/dkong_capture.rs)), which drive the
  device through the same timeline.

## Running the analyzer (numpy)

The analyzer needs numpy. On **NixOS** a `pip`-installed wheel fails at import
(its bundled C extensions can't find `libstdc++.so.6` / `libz.so.1`), so use the
nix-provided, properly-linked Python. Drop into a shell that has numpy and then
run the `python3 …` commands below as-is:

```bash
# NixOS (preferred): a nixpkgs python with numpy already wired up.
nix-shell -p 'python3.withPackages(ps: [ps.numpy])'

# Elsewhere: a venv works — then prefix the python3 calls with /tmp/sndvenv/bin/.
python3 -m venv /tmp/sndvenv && /tmp/sndvenv/bin/pip install -q numpy
```

## Usage

```bash
# 1. MAME reference (run from the MAME working dir with the asteroid romset)
mame asteroid -nothrottle -seconds_to_run 18 -video none \
     -autoboot_script $(pwd)/tools/sound-reference/drive_asteroid_sound.lua \
     -wavwrite /tmp/asteroid_ref.wav

# 2. Phosphor capture (writes /tmp/phosphor_asteroid.wav)
cargo run -p phosphor-machines --example asteroid_capture

# 3. Compare (from a numpy-capable shell — see above)
python3 tools/sound-reference/analyze_wav.py \
    /tmp/asteroid_ref.wav /tmp/phosphor_asteroid.wav
```

Lunar Lander is the same flow with the `llander` driver/example and `--llander`:

```bash
mame llander -nothrottle -seconds_to_run 12 -video none \
     -autoboot_script $(pwd)/tools/sound-reference/drive_llander_sound.lua \
     -wavwrite /tmp/llander_ref.wav
cargo run -p phosphor-machines --example llander_capture
python3 tools/sound-reference/analyze_wav.py --llander \
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
python3 tools/sound-reference/analyze_wav.py --galaxian \
    /tmp/galaxian_ref.wav /tmp/galaxian_phosphor.wav
```

The two tables should line up per effect (compare the centroid column; the single
peak bin is unstable for swept tones).

## When the per-effect table isn't enough

The centroid/RMS table is a fast first pass, but it averages over a window and so
misses two things that the ear hears immediately: **decay length** and
**tonal-vs-noisy character**. The Galaxian pass needed all of the following.

- **Spectrograms — look, don't just measure.** When a sound is "wrong" but the
  centroids match, render a spectrogram and *view it*. A bell/ring shows as a few
  steady horizontal lines; an explosion as a broadband vertical wash; a melody as
  stepped/swept lines. `matplotlib`'s `specgram` plus the `Read` of the PNG is the
  single most useful tool here. (NixOS: `nix-shell -p
  'python3.withPackages(ps: [ps.numpy ps.matplotlib])'`.)

- **Spectral flatness** separates a tone from noise where the centroid can't:
  `flatness = geomean(power) / mean(power)` over the band — ~0 for a pure tone,
  toward 1 for white noise. An explosion that reads as a low centroid but a near-0
  flatness is ringing like a bell, not rumbling like noise.

- **Verify the noise is actually white.** A wrongly-tapped LFSR can collapse to a
  tiny period and buzz like a pitched tone instead of hissing. Check the period
  before trusting it as a noise source — e.g. for the framework's
  `lfsr_noise(width, (tap_a, tap_b), seed)`, taps `(16, 13)` give a period of
  **28** (a ~280 Hz buzz) while `(11, 0)` give the full 2^17-1 white sequence.

- **Recorded reference samples.** Some effects are better judged against the
  original recorded MAME *samples* (the `samples/<game>.zip` WAVs MAME shipped
  before discrete emulation) than against a discrete capture. Galaxian's
  `shot.wav` / `death.wav` pinned the shoot as a ~0.6 s bright noise burst and the
  explosion as a ~2.5 s dark (~630 Hz) noise rumble — durations the windowed table
  never showed. Compare the WAV length and the RMS *envelope over time*, not just
  the steady-state spectrum.

- **Isolate an always-on voice.** Galaxian's melody note generator is always
  running, so at idle the game parks its pitch latch high enough that the note
  clock is ultrasonic (silent). Reproduce that in the timeline (park the pitch at
  `0xFF`) so a constant background voice doesn't bleed into the other windows — and
  make sure the emulated device treats that ultrasonic case as silent rather than
  aliasing it down into an audible tone.

## Adapting to another board

For Lunar Lander, Donkey Kong, etc.: copy the Lua driver and swap the register
map + timeline, change the `SEGMENTS` list in `analyze_wav.py` to match, and add a
sibling capture example. Notes on MAME 0.287 gotchas (silent attract, deprecated
`emu.register_start`, empty `sound_map` node routing) are in the case study.
