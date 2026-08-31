# Debugging Asteroids Discrete Sound: A Reference-Capture Case Study

> A war story from migrating Atari **Asteroids** (1979) onto Phosphor's discrete
> sound framework. It starts with sounds that are missing or wrong, goes a few
> rounds of fixing things by ear, hits a wall, and then pivots to capturing
> **ground-truth audio from MAME** and comparing it numerically against
> Phosphor's output. The pivot immediately surfaced two real bugs that ears had
> missed. The tooling built along the way is reusable for every future board.

> **THE TOOLING DESCRIBED HERE IS RETIRED, and the story is kept because the
> reasoning is.** `analyze_wav.py` and the `machines/examples/*_capture.rs`
> binaries are gone; the timeline they hand-synced between three files is now one
> committed scenario, driven on our side by `sndcmp` and on MAME's by a
> single-effect Lua driver, and compared by `disasm audiodiff`. Every command in
> this document that names a deleted file is history, not instructions. The
> current path is in
> [`tools/sound-reference/README.md`](../tools/sound-reference/README.md).
>
> Two of the conclusions below have also been overtaken by later work. The
> per-effect table this rig produced came from a driver that did NOT park the
> main CPU, on the reasoning that a silent attract mode is not writing sound
> registers; that turned out to be false, and five of the eight Asteroids voices
> were captured as one short burst per frame. And the numbers are read as
> absolute levels in places where MAME's output multiplier makes that meaningless.
> See `docs/designs/discrete-sound-fidelity.md`.

## Context

Asteroids generates sound from a discrete analog board — seven effect paths
(thrust, saucer, saucer-fire, ship-fire, explosion, thump, life) summed into one
mono output — driven by a handful of memory-mapped registers. Phosphor models it
with [`AsteroidsDiscreteSound`](../machines/src/asteroids_sound.rs) wrapping a
`DiscreteCircuit` from the [discrete sound framework](designs/discrete-sound-framework.md).

The first implementation was built from a *secondhand summary* of MAME's
`asteroid_a.cpp` netlist (produced by a sub-agent). It compiled, the unit tests
passed, and audio drained. Then we listened.

## Round 1: fixing the obvious by ear

The first report from the user: **"I don't hear thrust or saucer sounds for a
start."** Those are *sustained* effects (held while thrusting / while a saucer is
on screen), so a missing sustained sound points at a wiring bug, not timing.

Reading the **actual** MAME driver (not the summary) found it immediately:

```cpp
map(0x3c00, 0x3c07).w("audiolatch", FUNC(ls259_device::write_d7));
```

The 74LS259 addressable latch uses **`write_d7`** — the latched bit value comes
from data line **D7**, while the address selects which of the 8 latch lines.
Phosphor was reading **D0** (`data & 1`). Every latch-driven effect (thrust,
saucer, saucer-select, ship-fire, life) was therefore wired to the wrong bit and
never turned on. One-character fix:

```rust
0x3C00..=0x3C07 => self.sound.write_audio_latch_bit((addr & 7) as u8, data & 0x80 != 0),
```

**Lesson 1: read the schematic/driver, not a summary of it.** The agent summary
had the effect structure roughly right but got the bit-level details wrong, and
bit-level details are exactly what makes sound work or not.

While in the real netlist, two more by-ear fixes followed once sound returned:

- **Thrust was a harsh buzz.** A make-up gain of `60×` slammed the resonant
  ~90 Hz band-pass into hard clipping (peaks pinned at 32767). Dropped to a gain
  that reaches full scale without hard-clipping.
- **Saucer was a siren.** The warble deviation swept ±920 Hz (≈290–2130 Hz)
  instead of MAME's gentle ±460 Hz around 1210 Hz.

## Round 2: the wall

The next report: **"Saucer and thrust are still wrong. Thrust is too
high-pitched. Saucer pitch is too high and way too harsh."**

This is where ear-debugging breaks down. "Too high" and "too harsh" are
perceptual; translating them into Hz and filter coefficients is guesswork, and
each iteration costs a full build-run-listen round trip. The user, refreshingly
honest, added: **"I am pretty bad at assessing tones, etc."**

We needed numbers.

## The pivot: capture MAME ground truth

The user asked the key question: *"Is there a way in MAME with a lua script to
play these sounds and capture them?"* Yes — and it turned a subjective argument
into an objective measurement. Getting there took clearing several hurdles.

### Hurdle 1 — "the attract mode has no sound"

The plan was to capture each effect in isolation with `-wavwrite`. First attempts
produced a silent WAV. The instinct was "wavwrite is broken," but the real
explanation was simpler: **Asteroids' attract mode is genuinely silent** (it never
enables an effect), so a plain capture *should* be silent. A user-supplied WAV of
actual gameplay confirmed `wavwrite` works fine (40% non-zero, peak 23919).

### Hurdle 2 — driving the hardware from Lua

To capture isolated effects we drive the sound registers directly on a timeline
rather than playing the game. Because attract is silent, there's no need to halt
the CPU — just poke the latches each frame. The
[driver script](../tools/sound-reference/drive_asteroid_sound.lua) holds each
effect for a 2 s window:

```lua
local timeline = {
  { 1.0,  "thrust",       function(m) m:write_u8(0x3c00 + 3, 0x80) end },
  { 3.0,  "saucer_small", function(m) m:write_u8(0x3c00 + 0, 0x80) end },
  { 5.0,  "saucer_large", function(m) m:write_u8(0x3c00 + 0, 0x80); m:write_u8(0x3c00 + 2, 0x80) end },
  -- ... life, explosion, thump, ship_fire, saucer_fire ...
}
```

The first driver did nothing because it grabbed the memory space in
`emu.register_start`, which is **deprecated and a no-op** in MAME 0.287 — so the
handle stayed `nil`. Lazy-initializing inside the frame callback fixed it.

### Hurdle 3 — MAME 0.287's new audio routing

MAME 0.287 ships a reworked audio system. Its per-game config had:

```xml
<sound_map tag=":mono">
  <node_mapping node="" db="0.000000" />
</sound_map>
```

an **empty output node** — the speaker mapped to nothing — which made captures
silent until bypassed with a fresh `-cfg_directory`. Verbose logging
(`-verbose`) confirmed CoreAudio otherwise opened a real sink, which is why the
machine ran but the recorder saw nothing in some configurations.

### The analyzer

A small Python analyzer (`analyze_wav.py`, since deleted) segmented the capture
by time and reported, per effect, the **dominant FFT peak** and the **spectral
centroid** (a robust "brightness/where-the-energy-sits" measure that survives
swept tones better than a single peak bin).

```
mame asteroid -nothrottle -seconds_to_run 18 -video none \
     -autoboot_script tools/sound-reference/drive_asteroid_sound.lua \
     -wavwrite /tmp/asteroid_ref.wav
python3 tools/sound-reference/analyze_wav.py /tmp/asteroid_ref.wav
```

Segmenting one capture of eight back-to-back windows is the part that did not
survive. `disasm audiodiff` measures all of the above and more, per capture, and
the timeline now lives in a committed scenario rather than in this script's
segment list.

### MAME ground truth

| effect | peak Hz | centroid Hz | note |
|---|---|---|---|
| thrust | **82** | 128 | a *very low* rumble |
| saucer (small) | **1531** | 2899 | genuinely high |
| saucer (large) | 615 | 2200 | SEL drops it |
| life | 3000 | — | |
| explosion | 26 + noise | 910 | broadband |
| thump | **53** | 939 | low boom |
| saucer_fire | 630 | — | |

This **reframed the user's feedback**:

- Thrust is *supposed* to be ~82 Hz — extremely low. "Too high-pitched" wasn't
  pitch at all; it was the clipping harshness reading as brightness.
- The small saucer really is ~1530 Hz. The pitch wasn't wrong — the **waveform**
  was. MAME uses a `DISCRETE_TRIANGLEWAVE` (mellow); Phosphor used a square
  (bright odd harmonics that read as "higher and harsher"). The earlier "fix"
  that *lowered* the saucer to 560 Hz had actually moved it away from the
  reference.

**Lesson 2: don't tune perceptual qualities by ear. Measure.** The ear said
"saucer too high"; the reference said the pitch was correct and the timbre was
wrong. Opposite diagnoses.

## Closing the loop: compare Phosphor to MAME

Ground truth for MAME isn't enough — we need an apples-to-apples comparison.
`machines/examples/asteroid_capture.rs` (since deleted) drove
`AsteroidsDiscreteSound` through what was meant to be the **identical timeline**
and wrote a WAV, analyzed by the same script. "Meant to be" is doing work in that
sentence: keeping this file's timeline in step with the Lua driver's and the
analyzer's segment list was manual, and it is why a scenario is one committed
file now.

```
cargo run -p phosphor-machines --example asteroid_capture
python3 /tmp/analyze_asteroid_wav.py /tmp/phosphor_asteroid.wav
```

The first comparison caught **two bugs the ear had missed entirely**:

| effect | MAME | Phosphor (first pass) | diagnosis |
|---|---|---|---|
| explosion | rms 4901 | **rms 0** | stuck at DC |
| thump | 53 Hz | **210 Hz** | 4× too high |

### Bug: explosion stuck at DC

The explosion noise generator seeded its 16-bit **XNOR** LFSR with `0xFFFF`. For
an XNOR LFSR the **all-ones state is the lock state** — it feeds back ones
forever and never changes. The output was a constant (DC), not noise.

Why didn't a test catch it? The `every_effect_is_audible` test measured **raw
RMS**, and a large DC level sails past an "is it audible" threshold. The MAME
comparison analyzer removes the mean before measuring, so it reported `rms 0` and
exposed the lie. The fix was a one-line seed change (`0xFFFF` → `0`, matching
MAME's reset value), and the regression test was hardened to measure **AC RMS**
(mean removed) so a stuck-DC signal can never false-pass again.

**Lesson 3: "is it non-zero" is not "is it sound." Test AC content, not DC.**

### Bug: thump 4× too high

Phosphor's thump VCO swept ~30–210 Hz; MAME peaks at ~53 Hz. Re-ranged to
~20–55 Hz.

### The saucer, settled

With a new **triangle-wave primitive** added to the framework (it was in the
design's primitive list but unbuilt), the saucer uses a mellow triangle at MAME's
actual pitch. Small-saucer peak matched to the Hz.

### Final comparison

| effect | MAME (peak / centroid) | Phosphor (peak / centroid) |
|---|---|---|
| thrust | 82 / 128 | 86 / 121 |
| saucer small | 1532 / 2899 | 1532 / 3003 |
| saucer large | 615 / 2200 | 1305 / **2213** |
| life | 3000 | 3000 |
| explosion | 26 / 910 | 31 / **887** |
| thump | 53 / 939 | 55 / 839 |
| saucer_fire | 630 | 630 |

Every effect lines up within tolerance. (The saucer-large *peak bin* differs but
the centroid matches — an artifact of taking the single loudest bin of a swept
tone; the spectral content is the same.)

## Lessons

1. **Read the primary source.** A secondhand netlist summary got the effect
   *shapes* right but the *bit-level* details (D7 vs D0, divider mapping) wrong —
   and those details are the difference between working and silent.
2. **Don't tune perceptual qualities by ear.** "Too high / too harsh" pointed at
   pitch; the real issues were clipping (thrust) and waveform (saucer). Capture
   ground truth and measure.
3. **Test AC, not DC.** A raw-RMS "is it audible" check false-passed a DC-locked
   explosion. Removing the mean before measuring exposed it.
4. **Know your generator's degenerate states.** An XNOR LFSR locks at all-ones;
   seed it anywhere else (0 is the natural reset).
5. **Build the comparison harness.** The single highest-leverage move was making
   Phosphor and MAME emit the *same timeline* so one analyzer compares them. It
   converted "sounds wrong" into a numeric diff.

## Reproducing this today

The three-part rig this section used to list is gone, and so is the hand-syncing
between its parts that was its main defect. The equivalent now:

```bash
# Phosphor side, from the committed scenario
cargo run -p phosphor-sound-compare -- capture asteroids/thrust --out /tmp/ours.wav

# MAME side, on the same timeline, one voice at a time, main CPU parked
AST_EFFECT=thrust mame asteroid -rompath ~/ws/mame-runtime/roms -nothrottle \
    -seconds_to_run 4 -video none -samplerate 192000 \
    -autoboot_script tools/sound-reference/drive_asteroid_single.lua \
    -wavwrite /tmp/ref192.wav
ffmpeg -i /tmp/ref192.wav -af aresample=44100:resampler=soxr /tmp/ref.wav

# Compare
cargo run -p phosphor-disasm --bin disasm -- \
    audiodiff /tmp/ours.wav /tmp/ref.wav --range-b 0.95:3.0
```

Three of the differences are the point rather than housekeeping. The driver
**parks the main CPU**, because the game clears the audio latch as housekeeping
about 0.3 ms after this driver sets it and every latch-driven voice above was
captured chopped. It drives **one voice** rather than eight back to back, because
adjacent windows share a boundary the analysis has to guess at and no envelope
survives that. And it captures at **192 kHz**, because MAME's discrete engine
simulates at the audio rate and a 48 kHz capture reports its own edge
quantisation as circuit content.

See [`tools/sound-reference/README.md`](../tools/sound-reference/README.md) for
the current rig and how to adapt it to another board.

## Open threads

- **Thrust still doesn't rumble enough** by ear, even though its band centre
  (~85 Hz) matches MAME. The prime suspect used to be **MAME's post-processing**,
  on the reasoning that the new audio system applies an `<audio_effects>` chain
  (Filters, Compressor, Reverb, Equalizer) after the discrete mix, so a
  compressor and a low shelf would be thickening the sub-bass in the played sound
  but not in a `-wavwrite` capture. **That suspect is cleared.** `-wavwrite` taps
  the speaker mix *before* the chain, so the capture is raw netlist output; and
  the chain the per-game config lists is MAME's fixed default, in which the
  compressor and the reverb are off, the equalizer is flat, and the only active
  stage is a 20 Hz high-pass. There is nothing between the netlist and the
  speaker to explain a missing low end on either side. See the design doc's "The
  reference records the mix, not the mix you hear".

  So the deficit is a real difference between the two circuit models, and the
  next step is the netlist rather than an output stage: read the thrust path in
  MAME's `asteroid.cpp` discrete blocks against the schematic, and probe per
  stage. Modelling an output-stage shelf or compressor to match the played sound
  is off the table, since the played sound does not have one.
- **Reference-probe tests as CI.** The Phosphor-vs-MAME centroid comparison could
  be checked in as a tolerance test (gated/local-only, since it needs MAME +
  ROMs), per the design doc's testing strategy.
