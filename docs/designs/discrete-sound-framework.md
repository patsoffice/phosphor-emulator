# Design: Discrete Sound Device Framework

> **Status: proposed.** This document describes a reusable framework for
> discrete and board-level analog sound paths. Asteroids is the first migration
> target: its discrete sound writes are currently stubbed, and its latch-driven
> plus data-driven inputs exercise the framework without Donkey Kong's harder
> shared-analog mixer. Lunar Lander follows as a compact register-driven check,
> and Donkey Kong — the migration that motivated this work — lands once the
> primitives are proven. Tracked in beads epic
> `phosphor-emulator-discrete-sound-framework-l7o`.

## Context

Several arcade boards in Phosphor generate sound through discrete analog or
mixed analog/digital circuitry rather than a single programmable sound chip.
The current implementations fall into three groups:

- Donkey Kong has a hard-coded `DkongDiscrete` device that approximates only a
  few effects.
- Asteroids, Asteroids Deluxe, and Lunar Lander have sound-related bus writes
  that are stubbed or disabled with `no_audio`.
- Other machines use programmable devices such as POKEY, AY-8910, Namco WSG,
  DACs, Votrax, or SSIO boards, sometimes with board-level analog mixing that
  is only approximated today.

Donkey Kong is the clearest illustration of why this matters. `DkongDiscrete`
generates walk, jump, and stomp samples directly at 44.1 kHz, then `Tkg04Board`
adds those samples to a resampled DAC value:

```text
current DK path:

I8035 DAC -> AudioResampler ----\
                                +-> late i16 clamp -> frontend
hard-coded effect sample -------/
```

That shape is useful as a placeholder, but it is not how the hardware works.
The board has multiple voltage-producing paths, diode and resistor mixing, RC
charge/discharge behavior, filters, and an amplifier/output path. Those stages
change loudness, tone, DC offset, transient shape, and decay behavior. They are
part of the emulation target, not a post-processing detail.

Asteroids and Lunar Lander are the first migration targets because they
exercise different styles of discrete input while staying simpler than DK's
shared-analog path, so they prove out the framework before the harder DK
migration:

- Asteroids uses direct writes for explosion and thump data, an addressable
  audio latch, and a noise reset pulse.
- Lunar Lander uses a compact sound register plus a noise reset pulse.

The framework should make those implementations share primitives without
forcing every board into the same circuit.

## Goals

1. Model discrete sound as reusable, typed Rust circuit components.
2. Keep concrete board devices explicit and readable.
3. Support per-component timing rather than assuming every node runs at the
   output sample rate.
4. Treat mixers and filters as board-level audio primitives.
5. Allow programmable chip outputs to enter board-level analog paths when a
   schematic routes them that way.
6. Preserve deterministic save/load behavior.
7. Expose useful debug state without dumping every internal node.
8. Use schematics as the authority and local MAME source as reference material.
9. Keep the frontend audio contract unchanged: mono `i16` samples drained by
   `AudioSource::fill_audio`.

## Non-Goals

- This is not a general-purpose SPICE simulator.
- This is not a text netlist parser in v1.
- This is not a macro-for-macro port of MAME's `DISCRETE_*` system.
- This does not rewrite POKEY, AY-8910, Namco WSG, Votrax, or SSIO internals.
- This does not require bit-identical numeric audio output versus MAME.
- This does not add stereo or multi-output routing in v1 unless a concrete
  machine requires it.

## Proposed Architecture

Add a new core module for reusable building blocks:

```text
core/src/device/discrete/
```

The module owns the generic circuit runtime, primitive components, scheduler,
and test helpers. Boards continue to own concrete sound devices that wrap a
`DiscreteCircuit` and expose board-specific methods.

### Core Types

Use typed handles rather than stringly typed node names:

```rust
pub struct LogicInputId(u16);
pub struct DataInputId(u16);
pub struct PulseInputId(u16);
pub struct ExternalSourceId(u16);
pub struct NodeId(u16);

pub struct DiscreteCircuitBuilder {
    // Builds immutable topology and allocates typed handles.
}

pub struct DiscreteCircuit {
    // Owns runtime inputs, node state, scheduler phases, and resampler.
}
```

The builder API should be explicit Rust, not a text DSL:

```rust
let mut b = DiscreteCircuitBuilder::new(board_clock_hz, output_sample_rate);

let thrust = b.logic_input("THRUST");
let volume = b.data_input("VOLUME", 0);
let noise = b.lfsr_noise("NOISE", 12_000.0, lfsr_desc);
let shaped = b.rc_low_pass("THRUST_RC", noise, ohms(2_200.0), farads(1e-6));
let gated = b.multiply("THRUST_GAIN", shaped, volume);

b.output(gated, OutputGain::unity());
let circuit = b.build();
```

Exact names are negotiable during implementation. The design requirement is
that circuit definitions are typed, searchable Rust code with named inputs and
nodes.

### Node Storage and Dispatch

Store the graph as a contiguous `Vec<Node>` where `Node` is a closed enum over
the v1 primitive set (`Node::Lfsr`, `Node::RcLowPass`, `Node::ResistorMixer`,
and so on), plus a single `Node::Custom(Box<dyn CustomComponent>)` escape-hatch
variant. This is a deliberate "closed core, open escape hatch" choice:

- The common primitives dispatch statically through a `match` the compiler can
  inline, and they live inline in a cache-friendly array. The whole graph is
  iterated linearly each tick with no per-node heap indirection. This is what
  keeps the analog step rate within the real-time budget (see Evaluation
  Model).
- Save/load over a closed enum is exhaustive by construction: one tagged `match`
  per node, and the compiler flags any new variant that forgets to serialize.
- Board-specific oddities that are not worth a shared primitive go through
  `Node::Custom`, which carries the `CustomComponent` trait object described
  under Escape Hatch. Dynamic dispatch is paid only on these rare nodes (for
  example DK's 555 control-voltage mixer), where one extra indirection is
  irrelevant. `Custom` is the only variant that needs a tag-to-constructor
  registry on load.

Fat component state (long filter histories, wide LFSRs) is boxed per variant so
enum size stays small; the common small primitives remain inline.

### Concrete Board Devices

Concrete devices keep board intent at the call site:

```rust
pub struct DkongDiscreteSound {
    circuit: DiscreteCircuit,
    ids: DkongDiscreteInputs,
}

pub struct AsteroidsDiscreteSound {
    circuit: DiscreteCircuit,
    ids: AsteroidsDiscreteInputs,
}

pub struct LunarLanderDiscreteSound {
    circuit: DiscreteCircuit,
    ids: LunarLanderDiscreteInputs,
}
```

Wrappers expose board-facing methods:

```rust
impl DkongDiscreteSound {
    pub fn write_sound_bit(&mut self, bit: u8, value: bool);
    pub fn write_dac(&mut self, value: u8);
    pub fn set_discharge(&mut self, value: bool);
    pub fn tick(&mut self, board_cycles: u64);
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize;
}

impl AsteroidsDiscreteSound {
    pub fn write_explosion(&mut self, data: u8);
    pub fn write_thump(&mut self, data: u8);
    pub fn write_audio_latch_bit(&mut self, bit: u8, value: bool);
    pub fn pulse_noise_reset(&mut self);
    pub fn tick(&mut self, board_cycles: u64);
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize;
}

impl LunarLanderDiscreteSound {
    pub fn write_sound_register(&mut self, data: u8);
    pub fn pulse_noise_reset(&mut self);
    pub fn tick(&mut self, board_cycles: u64);
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize;
}
```

The board bus code should not know internal node IDs. It should call methods
with hardware intent.

## Timing Model

Use per-component clocks for v1.

Each component declares an update domain:

```text
BoardCycle       advances relative to the board's main clock
FixedFrequency   advances at a component-specific frequency, such as 12 kHz
OutputSample     advances once for each produced output sample
EventOnly        updates only when an input/latch changes
```

`DiscreteScheduler` advances the circuit deterministically from board ticks.
Components with lower-rate clocks use integer or Bresenham-style phase where
possible. Components that model analog state use elapsed time (`dt`) so RC and
filter behavior is not tied to an arbitrary host sample rate.

This is important for the target boards:

- DK has I8035 DAC updates, board-cycle interaction, 555-style oscillators, RC
  envelopes, and output filtering.
- Asteroids has 12 kHz noise, 3 kHz life tone, data-controlled thump and
  explosion paths, and latch-driven enables.
- Lunar Lander has 12 kHz noise, 3 kHz and 6 kHz fixed tones, and register
  controlled thrust/explosion.

The final circuit output is accumulated into an `AudioResampler<i16>` or
`AudioResampler<f32>` and exposed as mono `i16` samples to the machine.

### Performance Budget

A node graph stepped at analog rates is only viable if a single tick stays
cheap. The target is a few dozen nodes evaluated per step at hundreds of kHz,
fast enough to stay real-time alongside the rest of the machine. The storage and
dispatch choices above (contiguous `Vec<Node>`, static dispatch for common
primitives, per-component clock domains so most nodes do not run every tick)
exist to meet that budget. If a future board pushes node counts or step rates
high enough to threaten it, prefer raising the floor with cheaper per-component
clocks or analytic approximations before reaching for a faster graph runtime.

## Evaluation Model

A circuit is a directed graph: each node's inputs are the outputs of other
nodes. "Evaluating the circuit for one step" means computing every node's output
for that step. Order matters, and feedback must be handled explicitly.

Each node owns one output slot in a value array (`Vec<f64>`) parallel to the
node list. A node reads its inputs by `NodeId` index into that array, computes,
and writes its own slot.

### Acyclic paths: topological order

Most signal flow is strictly forward (source -> filter -> gain -> mixer ->
output). At `build()` time the graph is topologically sorted and the `Vec<Node>`
is stored in that order, so each step is a single linear sweep: every node runs
after the nodes it reads from, and every forward input slot already holds this
step's value by the time a consumer reads it. Eval order is memory order, so the
sweep is cache-friendly and there is no per-step sorting.

### Feedback: cycles cut into back-edges with one-step delay

Real circuits have feedback (DK's RC envelope feeding a 555 control voltage,
whose output feeds back toward that RC node). A cycle has no valid topological
order, so one edge in the loop must read a not-yet-recomputed value. At
`build()` time the graph is checked for cycles with a DFS; the minimal set of
edges needed to break each cycle is marked as *back-edges*, and the acyclic
remainder is what gets topo-sorted. During evaluation:

- A node reading a **forward edge** sees this step's freshly computed value.
- A node reading a **back-edge** sees the **previous step's** value, still held
  in the producer's output slot from the prior sweep.

A one-step lag at hundreds of kHz is far below audio frequencies and matches how
reference emulators model these loops. It is not bit-accurate, which is already
a non-goal. The builder must surface feedback rather than resolve it silently:
creating a cycle should be an explicit, visible act so an author knows a
one-step delay was introduced.

### Cross-clock-domain holding uses the same mechanism

A node on a slower clock domain (`FixedFrequency`, `EventOnly`) does not run
every step. On steps where it does not run, its output slot simply retains the
previous value. Consumers on faster domains therefore read a held value, which
is exactly sample-and-hold behavior. The held-slot array does triple duty:
forward evaluation, back-edge feedback delay, and cross-domain holding — one
mechanism, three uses.

### Save-state implication

Because the output-slot array carries held values and back-edge feedback state,
it is part of the runtime state and must be saved. A save taken mid-loop that
omitted the slots would reload with stale feedback and diverge for the first few
steps. The slot array is listed alongside RC voltages and filter memory under
Save State.

Determinism is within a single build and run: node state is `f64`, saved as raw
bits via `StateWriter::write_f64`. Bit-identical output is not promised across
recompiles or platforms, only deterministic replay within a run.

## Primitive Components

The v1 primitive set should be just broad enough for DK, Asteroids, and Lunar
Lander. Add more only when a real machine needs them.

### Inputs

- Logic input.
- Inverted logic input.
- Data input.
- Scaled data input.
- Pulse input.
- Edge detector.
- External source input for chip or DAC streams.

### Sources

- Fixed square wave.
- Variable square wave.
- Triangle wave.
- Ramp.
- LFSR noise.
- Sample-and-hold.

### Math and Routing

- Gain.
- Add 2/3/4/N.
- Multiply.
- Switch/on-off gate.
- Clamp.
- Invert.
- Data bit decode.

### Analog Approximations

- RC charge/discharge.
- RC low-pass.
- RC high-pass or coupling capacitor.
- First-order low/high-pass filter.
- Second-order band-pass and Sallen-Key-style approximation.
- Resistor mixer.
- Diode mixer.
- DAC/resistor ladder.
- 555 astable.
- 555 astable with control voltage.
- 555 constant-current approximation.

### Escape Hatch

Provide a `CustomComponent` trait for circuit-specific behavior that is too
specialized to justify a reusable primitive immediately:

```rust
pub trait CustomComponent {
    fn reset(&mut self);
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64;
    fn save_state(&self, w: &mut StateWriter);
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError>;
}
```

A `CustomComponent` is held by the `Node::Custom` enum variant (see Node Storage
and Dispatch); it is the only node kind that dispatches dynamically and the only
one that needs a tag-to-constructor registry on load. Use it sparingly. A good
v1 candidate is DK's 555 control-voltage mixer, which MAME models with a custom
component because the circuit is more specific than a simple mixer.

## Mixers and Filters

Mixers and filters are part of the emulated circuit. They should not be treated
as final PCM post-processing when the real board routes signals through shared
analog stages.

A mixer combines electrical-style node outputs using hardware-inspired scaling
and sometimes nonlinear behavior. A filter is stateful frequency shaping over
time. Filters change tone, decay, DC offset, and transient response.

### Mixer Types

Simple adder:

- Sums inputs with optional gains.
- Useful for high-level composition and early tests.
- Should not replace resistor or diode mixing when those details affect output.

Resistor mixer:

- Computes input contribution from resistor values.
- Useful for final output mixers and chip-plus-discrete board paths.
- Supports optional pull-up/pull-down or load resistance where needed.

Diode mixer:

- Models diode forward drop or threshold behavior.
- Useful for DK jump/stomp paths where noise or VCO output is combined through
  diodes.
- It can start as a simple configurable forward-drop approximation.

DAC/resistor ladder:

- Converts digital bits or a byte into analog voltage/current.
- Useful for Asteroids thump data, DK DAC paths, and Namco 54XX-style output
  circuits.

External source mixer input:

- Accepts samples from existing chip devices or DAC devices.
- Useful when the board schematic mixes chip outputs with discrete sources
  through shared analog circuitry.

### Filter Types

RC low-pass:

- Smooths fast changes.
- Removes harsh high-frequency content.
- Useful after square waves, noise, and DAC steps.

RC high-pass or coupling capacitor:

- Removes DC offset.
- Lets transients through while blocking steady voltage.
- Useful at output coupling and amplifier boundaries.

RC charge/discharge:

- Models capacitor envelope behavior.
- Useful for one-shots, decays, trigger recovery, and reset/discharge paths.

First-order filter:

- Generic low/high-pass approximation when exact topology is not worth
  spelling out.

Second-order filter:

- Supports band-pass and Sallen-Key-style approximations.
- Asteroids thrust uses noise shaped through band-pass and low-pass stages.

Output coupling/filter:

- Represents final board/cabinet amplifier shaping before PCM output.

### Target Signal Paths

Donkey Kong should be modeled as a shared analog path:

```text
stomp path -> diode/RC shaping ----\
jump path  -> VCO/diode/RC shaping  \
walk path  -> VCO/RC shaping         -> resistor mixer -> amp/filter -> output
DAC path   -> DAC/filter/VR gain    /
```

Asteroids should be modeled as several sources into a final output mix:

```text
thump   -> DAC ladder -> 555/filter ----\
saucer  -> VCO/warble/filter             \
fire    -> decay square/filter            -> output mix -> gain -> output
thrust  -> LFSR noise -> filters          /
explode -> sampled noise -> RC filter ---/
life    -> 3 kHz square ----------------/
```

Lunar Lander should be modeled as a compact register-driven circuit:

```text
12 kHz noise -> RC filter -> thrust gain -> band/low-pass -> output mix
                         \-> explosion gate/enhancement --/
3 kHz tone -----------------------------------------------/
6 kHz tone -----------------------------------------------/
```

## Non-Discrete Audio Boundary

Mixers and filters are generally useful for audio, but v1 should not rewrite
existing programmable sound chips around the discrete framework.

Programmable devices keep their internal emulation:

- POKEY owns its tone/noise/poly counters, channel mixing, and chip-specific
  behavior.
- AY-8910 owns its tone/noise/envelope channels.
- Namco WSG owns its wavetable voices.
- Votrax owns its speech signal path.
- SSIO owns its Z80 plus AY-8910 board behavior.

The discrete framework may still model board-level analog output paths that
those devices feed. Multiple POKEY or AY-8910 chips should not become discrete
circuits, but their outputs can become external source nodes when the real
board mixes them through shared resistor networks, volume controls, filters,
or final amplifier stages:

```text
POKEY 0 output --\
POKEY 1 output --- resistor/volume mixer -> output filter -> mono audio
discrete source -/
```

Use the board-level mixer/filter path only when the schematic or observed
hardware behavior justifies it. Otherwise, existing chip-specific mixing should
remain local to the chip or board.

## Asteroids Migration

Asteroids is the first migration target. It validates latch-driven and
data-driven inputs.

Current Phosphor map has these stubs:

```text
0x3600         explosion sound write
0x3A00         thump sound write
0x3C00-0x3C07  audio latch write
0x3E00         noise reset write
```

Target mapping:

- `0x3600`: explosion write.
  - Bits 2-5 become explosion volume/data.
  - Bits 6-7 select the explosion pitch divider.
  - MAME-observed divider mapping: `00 -> 12`, `01 -> 6`, `10 -> 3`,
    `11 -> 5`.
- `0x3A00`: thump write.
  - Bit 4 enables thump.
  - Low nibble controls thump DAC data.
- `0x3C00..=0x3C07`: addressable audio latch.
  - Bit 0: saucer sound enable.
  - Bit 1: saucer fire enable.
  - Bit 2: saucer select.
  - Bit 3: thrust enable.
  - Bit 4: ship fire enable.
  - Bit 5: life enable.
- `0x3E00`: noise reset pulse.

Once implemented, remove `no_audio` from `AsteroidsSystem` delegation and
route `AudioSource` through the sound device.

## Lunar Lander Migration

Lunar Lander should validate compact register-driven circuits. It reuses the
Atari noise and latch primitives established by the Asteroids migration.

Current Phosphor map has these stubs:

```text
0x3C00  sound register write
0x3E00  noise reset write
```

Target mapping:

- `0x3C00` sound register:
  - Bits 0-2: thrust volume.
  - Bit 3: explosion enable.
  - Bit 4: 3 kHz tone enable.
  - Bit 5: 6 kHz tone enable.
- `0x3E00`: noise reset pulse.

The circuit includes:

- 12 kHz LFSR noise.
- RC filtering.
- Thrust noise gain controlled by 3-bit data.
- Explosion gating.
- 3 kHz fixed tone.
- 6 kHz fixed tone.
- Final mix and output filter/gain.

Once implemented, remove `no_audio` from `LunarLanderSystem` delegation.

## Donkey Kong Migration

Donkey Kong is the final v1 migration target. It is the migration that motivated
this framework and the hardest one: a shared analog mixer, DAC routed into the
analog path, and discharge behavior. It runs once the mixer/filter primitives
exist and does not depend on the Atari migrations.

Current limitations:

- Only walk, jump, and stomp are approximated.
- Effects are generated directly as finished 44.1 kHz samples.
- DAC output is resampled separately and mixed late as PCM.
- The shared analog mixer, output filters, and discharge behavior are not
  represented.
- DK Jr. cannot reuse the current implementation cleanly even though it shares
  the board family.

Target behavior:

- Replace `DkongDiscrete` with `DkongDiscreteSound`.
- `Tkg04Board::write_sound_control_bit` feeds named circuit inputs.
- I8035 DAC writes feed the DK circuit DAC input.
- Discharge/control lines feed the appropriate circuit inputs.
- The DK circuit owns stomp, jump, walk, DAC, mixer, amplifier/filter, and
  resampler state.
- `Tkg04Board::fill_audio` drains from the DK sound device.
- Save/load covers the circuit runtime state.

The wrapper should keep board integration simple:

```rust
pub fn write_sound_control_bit(&mut self, bit: u8, value: bool) {
    self.sound_control_latch.write(bit, value);
    self.sound.write_sound_bit(bit, value);
}
```

## Follow-On Candidates

### Donkey Kong Jr.

Donkey Kong Jr. is a high-priority follow-on because it shares the TKG-04 board
in Phosphor, but MAME has a separate `dkongjr_discrete` circuit. It should
validate board-family variants without hard-coding DK-specific paths.

The implementation should reuse the shared primitives and board integration
style from DK, while allowing a different circuit topology and input mapping.

### Mario Bros.

Mario Bros. should follow DK and DK Jr. MAME models Mario Bros. with a
netlist-style sound circuit rather than the older `DISCRETE_SOUND_START`
macros. That makes it a good test of whether the typed Rust builder can express
larger schematic/netlist-style Nintendo audio without introducing a text DSL.

Mario should not block v1. It should inform whether the framework needs better
hierarchical subcircuits or reusable circuit fragments.

### Asteroids Deluxe

Asteroids Deluxe has chip-plus-discrete behavior: POKEY plus discrete
thrust/explosion-style audio. It is a good follow-on for external chip source
inputs and board-level mixing/filtering.

### Galaga and Namco 54XX Boards

Galaga does use a discrete output circuit, but it is tied to the Namco 54XX
custom explosion generator outputs rather than being a pure latch-driven analog
board like Asteroids.

Treat Galaga, Bosconian, Dig Dug-family variants, Xevious, and Pole Position as
a separate category:

```text
custom digital device -> 54XX output nibbles -> discrete DAC/filter/mixer path
```

Do not fold this into DK/Atari v1. Add it later once Namco 54XX behavior and
output routing are modeled.

### Other Future Candidates

MAME has many older Atari titles with `DISCRETE_SOUND_START` definitions,
including Battlezone/Red Baron-style boards and other 1970s discrete-heavy
games. Promote those only when the corresponding Phosphor machine exists or is
being added.

## Save State

All circuits and stateful components must implement `Saveable`.

Save:

- Input values and latches.
- External source sample latches where needed.
- Oscillator phase.
- Ramp/envelope state.
- LFSR registers.
- RC capacitor voltages.
- Filter memory.
- Node output slots (held values and back-edge feedback state — see Evaluation
  Model).
- Scheduler phases/dividers.
- Resampler state.

Do not save:

- Transient audio output buffers.
- Immutable graph topology.
- Static component values.
- Debug-only trace buffers.

Save formats should be versioned. Static circuit topology is reconstructed from
the concrete device constructor, then runtime state is loaded into it.

## Debugging

Every concrete sound device should implement `Debuggable`.

Expose:

- Board-facing inputs and registers.
- Important effect enables.
- LFSR state for noise generators.
- Selected output nodes, such as final mix level or effect output levels.
- Resampler phase/count where useful.

Avoid exposing every internal node by default. Large node dumps become noise in
the UI and are expensive to keep stable. If deeper inspection is needed, add a
targeted debug helper or trace feature rather than bloating
`debug_registers()`.

Examples of useful DK debug registers:

```text
LATCH
WALK
JUMP
STOMP
DAC
DISCHARGE
MIX_OUT
```

Examples of useful Asteroids debug registers:

```text
SAUCER
SAUCER_FIRE
SAUCER_SEL
THRUST
SHIP_FIRE
LIFE
THUMP_DATA
THUMP_EN
EXPLODE_DATA
EXPLODE_PITCH
NOISE_LFSR
```

## Reference Policy

Schematics are authoritative for topology and component values.

Local MAME source under `~/ws/mame` is reference material for:

- Cross-checking input mappings.
- Understanding undocumented board behavior.
- Finding practical approximations for components.
- Generating tolerance-based reference traces when useful.

Relevant MAME files for the initial design and follow-ons:

```text
src/mame/nintendo/dkong_a.cpp
src/mame/nintendo/mario.cpp
src/mame/nintendo/nl_mario.cpp
src/mame/atari/asteroid_a.cpp
src/mame/atari/asteroid.cpp
src/mame/namco/galaga_a.cpp
src/mame/namco/namco54.cpp
src/devices/sound/discrete.*
```

Do not copy MAME structure mechanically. Phosphor should use idiomatic Rust
types, explicit board wrappers, and focused primitives.

## Testing Strategy

### Primitive Tests

- LFSR sequence and reset behavior.
- Fixed square wave phase and frequency.
- Variable square wave frequency changes.
- Ramp behavior and reset behavior.
- RC charge/discharge approaches expected voltage within tolerance.
- Low-pass/high-pass filter step response sanity checks.
- Band-pass/second-order response sanity checks.
- Sample-and-hold captures only on selected edge.
- Resistor mixer scaling.
- Diode mixer threshold behavior.
- DAC ladder output monotonicity and expected endpoints.
- External source input sample handling.
- Save/load restores oscillator, filter, LFSR, and scheduler state.

### Donkey Kong Tests

- Sound latch writes update the expected DK circuit inputs.
- DAC writes affect the mixed output path.
- Reset clears runtime circuit state while preserving static configuration.
- Save/load preserves circuit input and runtime state.
- Audio drains after running a frame.
- Existing TKG-04 save-state tests are extended to include the new sound state.

### Asteroids Tests

- `0x3600` maps explosion volume and pitch correctly.
- `0x3A00` maps thump enable and data correctly.
- `0x3C00..=0x3C07` updates audio latch inputs.
- `0x3E00` pulses noise reset.
- Audio drains after running a frame.
- Save/load preserves latch and circuit state.

### Lunar Lander Tests

- `0x3C00` maps thrust volume, explosion, 3 kHz tone, and 6 kHz tone.
- `0x3E00` pulses noise reset.
- Tone-only cases produce deterministic non-silent output.
- Thrust/explosion cases produce non-silent output.
- Save/load preserves register and circuit state.

### Board-Level Chip Mixing Tests

When a board uses external chip source nodes:

- Multiple chip outputs enter the shared mixer with expected gain/scaling.
- Discrete and chip sources can be mixed through the same output path.
- The board still drains audio through the existing `fill_audio` API.

### Reference Probe Tests

Optional local-only tooling can compare short Phosphor traces against local
MAME/reference captures. These checks should be tolerance-based. Analog
approximations and filter implementation details may differ slightly even when
the audible and behavioral result is correct.

Manual listening and WAV inspection are useful for review, but should not be
the only validation.

## Implementation Phases

Tracked in beads epic `phosphor-emulator-discrete-sound-framework-l7o`. Phase
order is Asteroids-first to de-risk the framework on the simpler Atari boards
before the Donkey Kong migration; the Donkey Kong phase depends only on the
mixer/filter primitives, so it can proceed in parallel with the Atari boards.

### Phase 1: Framework Skeleton (`…-l7o.1`)

- Add `core/src/device/discrete/`.
- Implement builder, typed handles, runtime circuit (`Vec<Node>` enum plus
  `Node::Custom`), scheduler, the evaluation model (topo sort + back-edges), and
  basic source/math/input primitives.
- Add unit tests for deterministic primitives.

### Phase 2: Mixer and Filter Primitives (`…-l7o.2`)

- Add RC charge/discharge.
- Add RC low-pass/high-pass.
- Add first-order and second-order filters.
- Add resistor mixer, diode mixer, and DAC ladder.
- Add external source input.

### Phase 3: Asteroids Migration (`…-l7o.3`)

- Add `AsteroidsDiscreteSound`.
- Wire the existing `0x3600`/`0x3A00`/`0x3C00`/`0x3E00` stubs to the device.
- Remove `no_audio` for Asteroids once audio drains correctly.
- First real consumer: validates latch-driven and data-driven inputs.

### Phase 4: Lunar Lander Migration (`…-l7o.4`)

- Add `LunarLanderDiscreteSound`.
- Wire the existing `0x3C00`/`0x3E00` stubs to the device.
- Remove `no_audio` for Lunar Lander once audio drains correctly.
- Reuses the Atari noise/latch primitives from the Asteroids migration.

### Phase 5: Donkey Kong Migration (`…-l7o.5`)

- Replace `DkongDiscrete` with `DkongDiscreteSound`.
- Move DK DAC and discrete effects into one circuit output path.
- Update TKG-04 audio, save-state, reset, and debug integration.
- Preserve the existing frontend-facing machine API.

### Phase 6: Follow-Ons

- Donkey Kong Jr.
- Mario Bros.
- Asteroids Deluxe.
- Namco 54XX analog output circuits for Galaga-family hardware.

Out of v1 scope and tracked separately. Only add new primitives when a follow-on
requires them.

