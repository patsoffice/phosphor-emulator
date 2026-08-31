# Sound reference capture rig

MAME autoboot Lua that drives a board's sound hardware on a known timeline, so a
`-wavwrite` capture can be compared against Phosphor's. This directory is now the
**reference half only**. The Phosphor half and the comparison both live
elsewhere:

```bash
# Phosphor side: drive the device through a committed scenario
cargo run -p phosphor-sound-compare -- capture llander/thrust --out /tmp/ours.wav

# Reference side: the matching Lua driver, at 192 kHz, resampled to meet it
LL_EFFECT=thrust mame llander -rompath ~/ws/mame-runtime/roms -nothrottle \
    -seconds_to_run 4 -video none -samplerate 192000 \
    -autoboot_script tools/sound-reference/drive_llander_single.lua \
    -wavwrite /tmp/ref192.wav
ffmpeg -i /tmp/ref192.wav -af aresample=44100:resampler=soxr /tmp/ref.wav

# Compare
cargo run -p phosphor-disasm --bin disasm -- \
    audiodiff /tmp/ours.wav /tmp/ref.wav --range-b 0.95:3.0
```

The timeline is committed once, as a scenario under
[`tools/sound-compare/scenarios/`](../sound-compare/scenarios/), and the driver
here matches it by hand. That is one copy fewer than the rig this replaced, which
kept the timeline in three places -- a Lua driver, a Rust capture example and a
Python analyzer's segment list -- with nothing checking them against each other.
See [docs/designs/discrete-sound-fidelity.md](../../docs/designs/discrete-sound-fidelity.md),
and the case study in
[docs/debugging-asteroids-discrete-sound.md](../../docs/debugging-asteroids-discrete-sound.md).

## Four rules, each of which was learned by getting it wrong

**`sndcmp capture` already writes the analysis window.** Its output starts at the
scenario's `analysis.start_s`, not at zero. Re-ranging it with `--range` shifts
the span and silently compares two different moments. Range the REFERENCE, with
`--range-b`, as above.

**Capture at 192 kHz and band-limit afterwards.** MAME's discrete engine
simulates a netlist at the audio sample rate, so a capture's rate is also its
simulation rate: at the 48 kHz default a few-kHz square has its edges quantised
to 20.8 us and the capture carries broadband hash the circuit does not produce.
Two Galaxian voices were written up as having residuals that were entirely this,
and one nearly bought a rebuild of a noise source that was already right. Raising
the rate raises the capture BANDWIDTH as well, so resample both sides to a common
rate before comparing anything. Note this is a property of the discrete engine
specifically; a board on the netlist solver has its own internal timestep and its
output rate is only an output rate.

**Absolute level is a calibration, not a measurement.** Both the discrete engine
and the netlist subsystem apply a hand-chosen output multiplier, so comparing
dBFS compares two independent calibrations. Compare crest factor, band shares,
attack, decay and centroid, and treat level as calibration unless you can point
at what sets it. Lunar Lander is the case where you can: its mixer normalizes by
the sum of its leg levels, which is on the drawing, and correcting ours to that
moved every voice from 25 dB out to within 0.44 dB.

**Run MAME from a scratch directory, and give every run its own cfg.** MAME
writes a `cfg` on exit as well as reading one, so a second run of the same game
does not start where the first did. On Lunar Lander the presence of a cfg moves
the thrust capture by 53 dB, reproducibly, with nothing in the command line to
say so. `verify-reference.sh` gives each of its three runs a fresh cfg directory
for exactly this reason; a hand-rolled capture loop must do the same. Running
from the repo root also drops `cfg/` and `snap/` into the working tree.

## Contents

### Single-effect drivers, on a scenario's timeline

Use these for anything that is a level, an envelope or a decay.

| driver | machine | select with |
|---|---|---|
| `drive_asteroid_single.lua` | `asteroid` | `AST_EFFECT` |
| `drive_dkong_single.lua` | `dkong` | `DK_EFFECT` |
| `drive_dkongjr_single.lua` | `dkongjr` | `DKJR_EFFECT` |
| `drive_galaxian_single.lua` | `galaxian` | `GAL_EFFECT` |
| `drive_llander_single.lua` | `llander` | `LL_EFFECT` |
| `drive_mario_single.lua` | `mariobros` | `MARIO_EFFECT` |

Each drives ONE voice, asserted once, on the timeline of the matching scenario
under `tools/sound-compare/scenarios/`, and each honours `SND_VERIFY`.

### Multi-effect drivers, for a listen

`drive_asteroid_sound.lua`, `drive_dkong_sound.lua`, `drive_galaxian_sound.lua`,
`drive_llander_sound.lua`, `drive_dkong_gameplay.lua`.

These walk several voices through 2 s windows back to back. That is fine for
hearing whether a board sounds right and useless for measuring one: no single
event is isolated, and two adjacent windows share a boundary the analysis has to
guess at. The Python analyzer that used to segment them by time is gone, and with
it the only reason to prefer them.

`drive_dkong_sound.lua`, `drive_llander_sound.lua` and `drive_dkong_gameplay.lua`
do not honour `SND_VERIFY`, so nothing measured against those three is verified.
For Lunar Lander use `drive_llander_single.lua`, which does, and which also parks
the CPU where the older driver does not.

### `verify-reference.sh`

Proves a driver responds to its own timeline before anything is measured against
it. Run it after touching a driver. A driver has to honour `SND_VERIFY` for the
checks to mean anything, and one that does not cannot be checked at all.

```
LL_EFFECT=thrust tools/sound-reference/verify-reference.sh \
    tools/sound-reference/drive_llander_single.lua llander -seconds_to_run 4
```

Two knobs, and both exist because the check refused a valid reference once:

- `SND_SKIP_S` moves the measurement window onto the event. An effect that is
  over before the default 2.0 s leaves the window holding nothing, and the null
  check then compares two silences and passes for any driver at all. It refuses
  that rather than reporting ok, which is how it was caught: the two Asteroids
  fire voices are pulsed to the fourteen frames the game holds their latch line,
  so they are over by 1.23 s.
- `SND_MIN_PEAK` lowers the floor for a genuinely quiet voice. Lunar Lander's two
  alert tones sit at 0.0057 of full scale, which is what their 9.2 leg level
  against the explosion's 1000 comes to, and the default floor of 0.01 refused
  them. Lower it only for a reason of that shape.

### What the two checks CANNOT see

Both are relative, and neither is a check on exclusivity:

- **null** removes the stimulus and requires silence.
- **sensitivity** moves the schedule 30 ms and requires the capture to change.

A board running at half speed passes both. A capture chopped at the frame rate by
the game clearing a latch passes both. And contamination arriving through a
SHARED SOURCE passes both, because the null run has every voice gated off: on
Lunar Lander the game strobing the noise register's reset put a strong periodic
component through a band-pass with a Q of 7.6, and the null capture was still
exactly 0.0.

So a driver that parks the CPU should **check that the CPU stayed parked** rather
than claiming it. `drive_llander_single.lua` reads the program counter back each
frame and says so at the end, pass or fail; that is two lines and it is the only
thing that caught the above. The cheap detector otherwise is that a capture
modulated at the machine's frame rate is contaminated, since no voice on these
boards is periodic at 60 Hz on its own.

### `compare_wav.py`

Kept for exploratory one-offs, where editing a script beats rebuilding. It
answers a different question from `audiodiff`: given two recordings of the same
*gameplay*, where do they diverge. It is not the documented path for anything you
intend to repeat, and it needs numpy:

```bash
nix-shell -p 'python3.withPackages(ps: [ps.numpy ps.matplotlib])'
```

## Adapting to another board

Write the scenario first, under `tools/sound-compare/scenarios/<target>/`, then
an adapter under `tools/sound-compare/src/targets/`, then copy the nearest
`*_single.lua` and swap the register map. Keep the driver's timeline and the
scenario's identical by hand and say in the driver which scenario it matches.

Then, before believing a single number:

1. `cargo run -p phosphor-sound-compare -- verify <scenario>` for our side.
2. `verify-reference.sh` for MAME's.
3. Check something computable from the schematic alone -- a divider output, a
   counter tap, a fixed oscillator -- lands where the schematic puts it, on BOTH
   sides. A spectral comparison cannot tell "the model is wrong" from "the model
   is being run wrong", and Galaxian shipped every voice an octave low because
   nothing asked.

Notes on MAME Lua gotchas (silent attract modes, deprecated
`emu.register_start`, empty `sound_map` node routing) are in the case study.
