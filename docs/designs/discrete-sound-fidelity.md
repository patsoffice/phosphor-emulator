# Design: Discrete Sound Fidelity Tooling

> **Status: proposed.** A ladder of Rust tooling for making discrete sound
> demonstrably correct: a WAV differ that can gate, a ROM-less audio sanity test
> over the whole registry, isolated per-effect capture, per-voice and
> filter-section probes, construction-time parameter tuning, and — last — a
> bounded parameter fit reported as evidence rather than as a patch.
>
> Supersedes and replaces the two earlier drafts, `discrete-sound-fitting.md`
> and `discrete-sound-reference-tooling.md`. Absorbs
> `phosphor-emulator-audiodiff-76wx`, which had independently arrived at the
> bottom two rungs.

## Context

Three separate efforts converged on the same problem from different heights.

**The manual rig** (`tools/sound-reference/`) is what actually fixed the
Galaxian voices. It is three files per board in three languages: a MAME autoboot
Lua that pokes sound registers on a timeline, a `machines/examples/*_capture.rs`
that drives the Phosphor device on what is meant to be the same timeline, and
`analyze_wav.py` (108 lines) that segments both captures and prints a dominant
peak, a centroid and an RMS per window. Four boards have all three parts;
`xevious_capture.rs` has no Lua counterpart. `compare_wav.py` (210 lines) was
added later and already covers band energy, centroid, flatness, an RMS envelope
and a spectrogram for *same-session* comparisons.

**`phosphor-emulator-audiodiff-76wx`** observed that none of this can ever
become a gate: it needs numpy, a nix-shell whose setup takes a README section,
and a human to run it. Everything it measures is arithmetic already done in
Rust, over audio already produced in Rust — `disasm frameshot --audio-out` and
`disasm replay --audio-out` both write 16-bit mono WAVs today, and
`disasm imgdiff` plus `gfxsheet::write_png` are the established, actually-used
shape for "compare two artifacts and gate on the delta".

**The two design drafts** looked past the differ at the iteration loop. To try a
different value for a filter or a 555's timing resistor you edit
`machines/src/<board>_sound.rs` and rebuild. A three-parameter sweep at ten
points each is a thousand rebuilds, so in practice nobody sweeps — they guess
twice and stop.

### What the framework does and does not give us

`core/src/device/discrete/` is a good base: deterministic topology, typed input
handles, mono `i16` drain, save/load, and `DiscreteCircuit::value(node)` /
`name(node)` already public for debug views. Reading any node's current value is
therefore *already possible*; what is missing is streaming one over time.

What it does not give us is any way to recover schematic quantities from a built
circuit. Builder methods collapse their arguments on the way in:

| Builder call | Stored on the node | Recoverable? |
|---|---|---|
| `rc_low_pass(name, src, ohms, farads)` | `tau: ohms * farads` | Product only; R and C individually lost |
| `rc_high_pass(name, src, ohms, farads)` | `tau: ohms * farads` | Product only |
| `rc_disc5(.., r, c)` | `charge_exp: 1-exp(-dt/(r*c))` | Product only |
| `ne555_astable(.., r1, r2, c, vcc, ..)` | `exp_charge`, `exp_discharge`, `v_charge`, thresholds | 2 equations, 3 unknowns |
| `op_amp_band_pass(.., r_in[], rf, c1, c2, ..)` | `a1, a2, b0, b2, in_gain` | Non-injective; many R/C sets map to one biquad |
| `resistor_mixer(taps, load_ohms)` | per-tap conductances + folded total | Taps invertible; `load_ohms` lost |
| `ne555_cc(.., r, c, ..)` | `r`, `c` verbatim | Yes |
| `second_order(.., f0, q)`, `gain`, `lfsr_noise` | verbatim | Yes |

Two consequences, and they point the same way.

A coefficient-space result is **unusable as a recommendation**: reporting
`THRUST_RC.tau: 2.2e-3 → 3.9e-3` does not tell you what to type, because the
source reads `rc_low_pass("THRUST_RC", thrust_noise, 2_200.0, 1e-6)`. For
`op_amp_band_pass` it is worse — four biquad coefficients do not determine `rf`,
`c1`, `c2` at all.

A coefficient-space result is also **less constrained than reality**. Real
boards are built from preferred-value parts. Fitting `tau` freely explores a
continuum no parts bin can produce, which makes overfitting easy and the result
unfalsifiable.

So: tune in schematic space, in the units written at the call site.

## Decisions

These are the points where the two drafts disagreed. Each is settled here.

### 1. Tuning happens at construction, not on a live circuit

The fitting draft proposed `DiscreteCircuit::set_param(addr, value)`, mutating a
built circuit and recomputing derived coefficients in place while leaving
runtime state alone. Rejected. It raises questions with no correct answer: does
changing a 555's timing resistor restart the cap integration? Is a biquad's
`x1/x2/y1/y2` history still meaningful under new coefficients? Does restoring
the old value restore the old state? Does the previous candidate's audio remain
in the resampler?

The optimizer does not need live mutation — it needs many *clean* evaluations,
and constructing a circuit is trivially cheap next to simulating and analyzing
seconds of audio. Every candidate is therefore:

```text
resolve overrides -> construct fresh device -> reset -> run scenario
                  -> drain audio -> score -> drop
```

### 2. Parameters have explicit, stable IDs — not node-name addresses

The fitting draft addressed parameters as `"<NODE_NAME>.<arg>"`, on the stated
grounds that node names "are already unique per circuit". **They are not.**
`DiscreteCircuitBuilder::push_node` pushes into a `Vec<String>` with no
uniqueness check of any kind, and nothing else enforces one. Name addressing is
a silent-collision hazard, and it also welds parameter identity to topology, so
renaming a node or splitting a filter stage breaks every recorded fit.

Parameters are registered explicitly by the sound constructor with stable IDs
(`dkong.walk.c`). That also buys what auto-recording cannot: only meaningful
values enter the search space, custom components participate on equal terms,
shared constants can deliberately map to one parameter, and bounds, tolerance
and parameter class live beside the value.

### 3. The Rust tool does not generate or run MAME Lua

The fitting draft proposed `sndcmp gen-lua <board>` so the MAME driver could not
drift from the Phosphor timeline. Rejected, for the reason the repository
learned two commits ago when `disasm movie mame` was reverted: a tool welded to
MAME's Lua API, device tags and memory spaces has a narrow useful envelope and a
wide maintenance surface.

Keeping MAME outside the boundary means the same comparison path serves MAME
output, real-hardware recordings, original sample recordings, another emulator,
and a previous Phosphor revision. Drift is caught instead by validating the
*observable* contract between a scenario and a reference manifest — duration,
trigger and release times, detected onset, one-shot versus sustained shape — and
by recording the capture script's hash in the manifest.

Reviewed Lua capture aids stay in the repository as reference-production tools.

### 4. Comparison lives in `disasm`; the fitting harness is its own crate

Both drafts put everything in a new `tools/sound-compare` crate. That is right
for the fitting harness and wrong for the differ. `disasm` already writes the
WAVs, already renders PNGs, already has `imgdiff` as the sibling command, and is
already the tool people run.

- **`disasm audiodiff`** — compare two WAVs, gate on tolerances.
- **Shared DSP** in `phosphor_core::audio::analysis` — FFT/STFT, envelope,
  band energy, decay, flatness, alignment. Core already owns `AudioResampler`;
  a hand-rolled radix-2 FFT (~40 lines, in keeping with a codebase that
  hand-rolls a 101-tap FIR and a Bresenham resampler) keeps core's dependency
  set unchanged. Both `disasm` and `machines`' tests already depend on core.
- **`tools/sound-compare`** (`phosphor-sound-compare`, binary `sndcmp`) — the
  scenario runner, tuning, sensitivity and fit. Depends on core and machines;
  never on the frontend or SDL. A workspace member, not a default member.

### 5. Volume is three separate questions

"Match the volume" is ambiguous, and conflating the three is how a wrong effect
gain gets normalized away:

1. **Normalized shape/timbre** — with DC removed and a common gain applied, do
   pitch, harmonics, spectrum, attack and decay match?
2. **Absolute digital level** — at controlled capture gain, do RMS, peak,
   integrated energy and envelope level match in dBFS?
3. **Relative board balance** — are effects balanced against one another and
   against DAC or PSG music the way the board intends?

Normalized and raw results are always reported separately, and absolute level is
excluded from fitting unless the reference manifest declares it trusted.

### 6. Every parameter is classified, and the class governs interpretation

- **Schematic** — a resistor, capacitor, voltage, clock or gain network sourced
  from board documentation. Stays within documented part tolerance.
- **Model** — a behavioral approximation with no one-component equivalent, such
  as a synthetic envelope time constant. Wider bounds, but the report must say
  the value is behavioral rather than physical.
- **Calibration** — output scaling or mix trim mapping modeled voltage into
  finite PCM range. `DAC_GAIN = 0.55` and `STOMP_GAIN = 7.0` in
  `machines/src/dkong_sound.rs` are exactly this. Fittable only when absolute
  capture level is trusted.

### 7. A fit result is machine-applicable, and write-back is classified

The point of the exercise is to get a fitted value back into the emulator
without a human retyping it, so the loop closes in two places.

**Every fit writes a `tuning.toml` the emulator can load.** The same
`TuningOverrides` the optimizer feeds to a candidate device can be loaded by a
normal machine build, so a fit can be *heard in the real emulator* immediately,
with no rebuild and no source edit. This is the round trip, eliminated: fit,
then run the machine against the fit, then A/B it against the default by
dropping the flag.

**Source write-back is offered only where it is decidable.** The registration
macro captures the value's expression with `stringify!` alongside its
`#[track_caller]` location, giving each `ParameterSpec` a `writeback` field:

- `Literal` — the value is a float literal, either a module-scope
  `const NAME: f64 = 33e-9;` or a literal argument at a builder call site such
  as `b.rc_low_pass("THRUST_RC", thrust_noise, 2_200.0, 1e-6)`. Rewriting it is
  a mechanical, unambiguous edit, and the tool can emit an applicable diff.
- `Derived` — the value is an expression: `LVL_EXPLOSION / LVL_TOTAL`,
  `18_432_000.0 / 6.0 / 2.0`, `v1 - v0`. Pasting a fitted float over it would
  destroy the relationship the expression encodes — the right edit is to a term
  inside it, and which term is a judgment the tool does not have. Report only.

This is decidable mechanically, at registration, with no source parsing and no
guessing. It also matters more than it might sound: in `dkong_sound.rs` — the
first target — nearly every tunable is already a module-scope const with a
literal initializer (`WALK_C`, `JUMP_C`, `WALK_LP_HZ`, `DAC_GAIN`,
`STOMP_GAIN`), so the common case is the automatic one.

An earlier draft proposed locating the call site by grepping for the node-name
string literal. That does not work: `asteroids_sound.rs` builds node names with
`format!("{name}_AC")`, so the name never appears as a literal in the source at
all. It is a further count against name-addressed parameters (Decision 2).

## Reference policy

The standing convention is that datasheets and schematics are the authority and
MAME is reference material. An auto-fitter is in obvious tension with that, so
the position is stated rather than left implicit:

**A fit result is a diagnostic, not a target.**

- A fitted value **within component tolerance** of the schematic value (±5% or
  ±10% resistors, ±20% electrolytics) is a plausible refinement and may be
  applied on the tool's recommendation.
- A fitted value **outside tolerance** is a bug report. It says the two circuits
  differ in a way a scalar is compensating for, and applying it buries the real
  defect under a fudge factor.

This is what happened with Asteroids thrust: the band centre already matched the
measured peak and the complaint was missing low-end weight. No filter value
fixes a missing output stage, and an unconstrained fitter would have happily
mangled the band-pass chasing the spectrum.

It also bounds the tooling's value honestly. It is very good at "which of these
two plausible values is right", and structurally unable to answer "what is
missing" — which is what the probes in Part 4 exist for.

## What is automatic, end to end

The intended loop, with no Python anywhere in it and no rebuild between
candidates:

```bash
# once: prove the reference is what it claims to be
sndcmp validate-reference $PHOSPHOR_SOUND_REFS/dkong/jump.toml

# fit, and write the result somewhere the emulator can read
sndcmp fit dkong/jump $PHOSPHOR_SOUND_REFS/dkong/jump.toml \
    --params dkong.jump.c,dkong.jump.envelope_tau \
    --out-tuning /tmp/dkong-fit.toml

# hear it in the real machine, no rebuild
cargo run -p phosphor-frontend -- dkong $ROMS --tuning /tmp/dkong-fit.toml

# land the ones that are literals; the rest come with a located report
sndcmp writeback /tmp/dkong-fit.toml --apply
```

Three things are worth being precise about, because "identical, automatically"
is achievable in one sense and not in another.

**The search is fully automatic and fully Rust.** One command takes a WAV and a
scenario and returns fitted values. No numpy, no nix-shell for numpy, no human
reading a table of centroids and deciding what to type next. That is Phase 6 and
it is the straightforward part.

**Getting the values back into the emulator is automatic** — via `tuning.toml`
for trying it, and via `sndcmp writeback` for landing it wherever the parameter
is a literal (Decision 7). Where the value is a derived expression the tool
stops and reports, because there the correct edit is genuinely ambiguous.

**"Identical" is not the target, and the design deliberately refuses to chase
it.** Four separate ceilings, none of which more compute removes:

1. *Sample equality is impossible and meaningless here.* Two independent
   emulators never share LFSR seed, oscillator phase or startup delay. The
   objective is phase-blind by construction — matching spectrum and envelope, not
   waveforms. A fitter that scored raw waveform error would optimize toward seed
   coincidence and away from circuit fidelity.
2. *Structural gaps cannot be fitted away.* If a board is missing an output
   stage, no scalar makes it match, and an unconstrained fitter will mangle a
   filter trying. This is why an out-of-tolerance result is reported as a bug
   rather than applied — it is the tool telling you the remaining error is not
   the kind it can fix.
3. *Some parameter sets are degenerate.* `op_amp_band_pass` maps five schematic
   values onto four coefficients, so its schematic space is under-determined by
   construction; the fit will be ill-conditioned and must say so instead of
   returning confident numbers.
4. *The reference may not be ground truth.* Two ways it can fail, both found
   rather than hypothesized: a capture whose driver never asserted what it
   claimed, and a capture reporting the reference simulation's own edge
   quantisation. The third candidate, that `-wavwrite` taps after the
   `<audio_effects>` chain and a fit would be chasing MAME's compressor and EQ,
   was Phase 0 and did not happen. The tap is before the chain.

So the honest claim is: **automatic, closed-loop, Python-free fitting to the
best match the modeled topology can express** — and a clear report when the
remaining error means the topology itself is what needs work. In practice that
is the more useful of the two outcomes, because "no value of this capacitor
explains the reference" is a finding you cannot get by listening.

## Goals

1. Compare two captures from one Rust command that exits non-zero past
   tolerance, so audio can gate.
2. Catch gross audio defects across the whole registry with no MAME, no ROMs and
   no reference WAV.
3. Replace the Python analyzers with tested Rust analysis covering pitch,
   spectrum, noise character, attack, decay, clipping, DC and level.
4. Isolate one effect, one voice, or one filter section, so a difference can be
   localized instead of merely detected.
5. Change selected schematic/model/calibration values without recompiling.
6. Rank parameters by measured sensitivity before optimizing anything.
7. Report recommendations with evidence, confidence, and a schematic-plausibility
   verdict.
8. Close the loop without a human retyping numbers: a fit is loadable by the
   emulator as-is, and applicable to source where the value is a literal.
9. Zero cost to the emulation hot loop.
10. Keep MAME optional and outside builds and CI. No Python in any repeatable
    path.

## Non-Goals

- A SPICE simulator, or fitting circuit *topology* — node counts, wiring, LFSR
  taps. Only scalar values.
- Treating MAME as more authoritative than schematics.
- Sample-for-sample equality between independent noise sources or oscillators.
- Rewriting source for parameters whose value is a derived expression rather
  than a literal, or applying any edit without `--apply`. See Decision 7.
- Bit-identical output against another emulator. The objective is deliberately
  phase-blind; see "What is automatic, end to end".
- Launching or controlling MAME.
- Replacing listening tests. The tool makes listening directed and repeatable.
- Fitting an unbounded number of parameters at once.
- A general audio-analysis library. Only what these comparisons need.
- Committing ROMs, or requiring MAME in CI.

## Part 1 — `disasm audiodiff` and the shared analysis module

The everyday tool, and the sibling of `imgdiff`.

```text
disasm audiodiff A.wav B.wav [--png spec.png] [--json] [--tolerance ...]
```

Reports duration and rate, RMS and the gain ratio between files, DC offset,
clipped-sample count, silent fraction, spectral centroid, spectral flatness, and
a band-energy table with deltas. Exits non-zero when tolerances are exceeded.

The band deltas are the column to read first: they are scale-invariant, so a pure
gain error leaves them at zero and anything large is a filter, mix or source
difference. That single property is what separates the four ways a netlist goes
wrong — source, filter, mix, output stage — and it is why `compare_wav.py`
already leads with it.

Spectrograms need no plotting dependency: STFT, log magnitude, colormap, then
the existing `gfxsheet::write_png`. That also composes — `disasm imgdiff` can
then diff two spectrograms.

`phosphor_core::audio::analysis` holds the arithmetic:

| Group | Contents |
|---|---|
| Signal integrity | DC offset, peak dBFS, clipping fraction, crest factor, leading activity |
| Level and time | AC RMS dBFS, integrated squared energy, RMS envelope, attack time, T20/T40 decay, duration above threshold |
| Frequency and timbre | fundamental (spectral + autocorrelation), dominant peaks, centroid, rolloff, flatness, band-energy ratios, harmonic ratios |
| Distance | multi-resolution log-magnitude STFT distance, envelope L1 |
| Alignment | onset detection, envelope cross-correlation within a bound |

Short arcade effects rarely have the dynamic range for a reliable T60, so
several decay summaries beat one nominal number. Pitch combines spectral and
autocorrelation estimates — a single largest FFT bin is unstable for swept tones
and noisy resonances.

This part is largely a *port*: `compare_wav.py` already computes band energy,
centroid, flatness, envelope and spectrogram. The design work is the alignment
and distance layers, not the metrics.

## Part 2 — Registry-driven audio sanity test

The cheapest rung, and the only one that needs nothing external. Boot every
machine in `registry::all()`, capture N frames, and assert:

- `|DC|` is a small fraction of full scale;
- output is not permanently saturated;
- output is not permanently silent, unless the machine declares it.

This is the audio counterpart of `golden_frame_test`'s guards, and it would have
caught both halves of `phosphor-emulator-audio-dc-offset-g7p4` — Donkey Kong's
DC offset and Joust's saturation — with no reference capture and no human
listening. It follows the existing registry-suite pattern, including a
`the_registry_is_not_empty` guard so the file cannot pass vacuously; there is a
precedent for the process-wide-rate handling in `machines/tests/audio_rate_test.rs`.

Machines that are legitimately silent need a declared allowance. `no_audio` no
longer exists on the registry, so this test introduces the declaration it needs
— per-machine, in the registry, reviewable, and not a test-local skip list.

## Part 3 — Isolated scenarios and target adapters

### Isolated effects are the fitting unit

Each reference capture contains one isolated effect. Sustained effects have a
clear enable and release; one-shots are triggered once and allowed to decay
completely:

```text
walk:   silence -> enable and hold -> disable -> tail
jump:   silence -> one short trigger pulse -> full tail
stomp:  silence -> one short trigger pulse -> full tail
```

Repeated one-shot pulses are useful for listening and useless for fitting —
overlapping decays hide the envelope and the integrated energy of one event. The
current Donkey Kong captures retrigger, which is why they cannot answer decay
questions.

### Scenarios

Repository-owned, describing hardware intent rather than emulator methods or
MAME addresses:

```toml
# tools/sound-compare/scenarios/dkong/jump.toml
schema = 1
id = "dkong/jump"
target = "dkong-discrete"
duration_s = 3.0
output_rate_hz = 44100

[[action]]
at_s = 0.5
control = "jump"
value = true

[[action]]
at_s = 0.55
control = "jump"
value = false

[analysis]
start_s = 0.45
end_s = 3.0
align_from_s = 0.4
align_to_s = 1.0
```

Initial action shapes: set a boolean or numeric control; pulse for a duration;
hold over an interval; linear or stepped ramp; reset; set an external-source
baseline such as a silent DAC or PSG input. Explicit events, not per-frame
callbacks, so trigger edges are exact and independent of any producer's frame
rate.

This replaces all three hand-synced copies of the old timeline. The
`machines/examples/*_capture.rs` binaries are deleted as each board migrates,
and `analyze_wav.py` goes with the last one.

### Adapters

Discrete devices have different clocks and hardware-facing APIs, and a universal
"write register" trait would erase useful intent while still failing to cover
external DAC/PSG streams. One small adapter per target instead:

```rust
trait SoundTargetFactory {
    fn id(&self) -> &'static str;
    fn controls(&self) -> &'static [ControlSpec];
    fn parameters(&self) -> Vec<ParameterSpec>;
    fn create(&self, tuning: &TuningOverrides) -> Result<Box<dyn SoundTarget>>;
}

trait SoundTarget {
    fn sample_rate(&self) -> u32;
    fn reset(&mut self);
    fn set_control(&mut self, id: &str, value: ControlValue) -> Result<()>;
    fn advance_to(&mut self, sample_index: u64);
    fn drain_audio(&mut self, out: &mut Vec<i16>);
}
```

The adapter owns board-specific timing — Asteroids and Lunar Lander translate
elapsed time into board CPU cycles, Galaxian advances its fixed internal
simulation from main-CPU cycles, Donkey Kong advances once per output/DAC sample
holding the DAC baseline. The scenario layer never imports concrete machine
types.

### Direct-device fitting, full-machine validation

The optimizer drives the concrete sound device: fast, deterministic,
ROM-independent, and isolated. The complete machine is then used after a
candidate change to validate bus wiring, real game pulse widths and trigger
order, board-level mixing, effect-to-music balance, and negotiated output rate.

Donkey Kong shows why both levels are needed. `Tkg04Board` box-filters the I8035
DAC and feeds each sample into `DkongDiscreteSound`, which owns the DAC
reconstruction filter and the walk/jump/stomp mix; driving the device with a
zero DAC exercises the modeled analog path while excluding unrelated CPU
activity. Only the full machine proves the board drives it correctly.

### Analysis rate is not the reference's rate

Phosphor captures at a canonical rate — initially `DEFAULT_HOST_SAMPLE_RATE` —
and the reference is resampled to meet it, using
`phosphor_core::audio::AudioResampler` rather than a second unrelated resampler.

Constructing the circuit at whatever rate the reference WAV happens to carry
would change the model under test. `dkong_sound.rs` builds board = sim = output
= host rate by construction, so for the first target this is not hypothetical.
Devices whose analog behavior materially changes with host rate should
eventually move to a stable internal simulation rate — but that is a fidelity
defect this tooling *reveals*, not a prerequisite for building it.

## Part 4 — Probes: per-voice, per-node, per-section

Output-only evidence shows that a model is wrong without showing where it went
wrong. Four facilities, in increasing power. The fourth was added after the
Donkey Kong work made its absence the binding constraint — see "What the first
board taught" below.

**Per-voice solo render.** Render one voice or node to its own WAV instead of
the mix. This is the difference between "our dkong sounds wrong" and "our dkong
walk voice's RC decay is too fast", and it is the single most useful diagnostic
here. `DiscreteCircuit::value(node)` already exposes any node's current value,
so this is a small step: construct the circuit with the selected probe as an
alternate output. Since the optimizer already constructs a fresh device per run,
that costs the normal runtime nothing.

**Named probes.** A curated set per device, with IDs describing circuit intent
so they survive local topology changes — walk LFO/control voltage, walk 555 raw
output, walk post-coupling, jump envelope, jump 555, stomp raw noise, each
post-filter effect, filtered DAC, final mix. Curated, not every internal node
promoted to stable API. Phosphor probes are diagnostic; they are not assumed to
correspond one-to-one with MAME's internal nodes.

**Impulse and step response.** Feed a known impulse or white noise through a
filter section and compare its magnitude response against the same section
elsewhere. This is the only measurement that cleanly separates a *source* error
(LFSR taps, oscillator divider, envelope shape) from a *filter* error (RC corner,
Q, order) — a spectral diff of mixed output fundamentally cannot, because both
move the same bands.

**Node-level comparison against the reference implementation.** Render a named
node on our side and the corresponding node in MAME's netlist, and diff them.
MAME's discrete engine can log named nodes (`DISCRETE_CSVLOG`,
`DISCRETE_WAVLOG`); ours can already render one by name. What is missing is the
node map between them and a command to capture both.

This is the facility whose absence stalled the first board, and it is worth more
than everything below it in this document. See
`phosphor-emulator-discrete-node-compare-2b7f`.

### What the first board taught

Donkey Kong was the first real use of this design, and it revised three of its
assumptions.

**Read the reference implementation before measuring anything.** The design
frames MAME as a source of reference *captures*. It is also a source of
reference *topology*, and that is the more valuable half. Hours went into
inferring circuit structure from spectra — a sustained-vs-triggered envelope, a
wobble oscillator's existence, whether an envelope multiplies or diode-mixes —
while `dkong_a.cpp` sat on disk with the answers. Reading it took minutes and
was decisive every time. The reference policy is unchanged (schematics remain
the authority, and MAME is not to be cited in comments) but the working order
should be: read the topology, model it, then measure to check.

**Fitting is the wrong tool while the topology is unknown, and actively
harmful.** Every value fitted before the structure was right was either wrong or
right for the wrong reason: a stomp gain fitted against overlapping decays, a
walk pitch fitted against a two-second hold the game never produces, a wobble
oscillator deleted because its per-step variation looked like measurement noise.
Each fit made the output measure better while the model got further from the
board, and each had to be undone. A fitted scalar standing in for a missing
mechanism is the failure this document warns about in the reference policy — it
turns out to be the *common* case, not the exceptional one.

**The reference has to be verified as an experiment, not just as a capture.**
The document's reference policy asks whether the reference is *authentic*. It
does not ask whether the reference ran the experiment we think it ran, and that
is where the largest single error in this work came from.

Every timeline-driven MAME driver read the clock as
`manager.machine.time.seconds`. That is the attotime's integer seconds field,
not elapsed time — it reads `2` for the whole of the third second. So every
comparison against a fractional timeline held for a full second: a jump captured
as a 50 ms pulse was really a 1 s assertion, and the release edge never happened
at all. The correct accessor is `manager.machine.time:as_double()`.

Nothing about the capture looks wrong. It contains a plausible note of plausible
length that starts when it should. It was only caught by an experiment on the
reference itself — capturing with two different hold lengths and finding the
results byte-identical, then with the line never asserted and finding silence.
The jump note it produced sat a fifth below the real one, and every attempt to
reconcile Phosphor with it was chasing a control voltage the board never holds.

So: before trusting a reference, prove it responds to the stimulus. Vary the
one parameter under test and confirm the capture changes; drive the null case
and confirm it goes quiet. A capture that ignores its own timeline is worse than
no capture, because it looks like evidence. This belongs in the reference policy
alongside authenticity, and it applies to `sndcmp`'s own scenarios equally.

The consequence for sequencing is in the phase list below: node comparison moves
ahead of `fit`.

### What finishing the first board taught

Donkey Kong was then taken to completion — all three effects and the music built
from the board rather than fitted. Four more things came out of it.

**A fitted filter corner is the *usual* disguise for a missing stage.** Every
one of Donkey Kong's voices carried one, and in each case the corner had no
counterpart on the board at all:

| voice | the fitted stand-in | what was actually there |
|---|---|---|
| stomp | a 175 Hz low-pass | a counter dividing the noise's edges by eight |
| walk | a 700 Hz low-pass | an oscillator *chopping* the envelope, not multiplying it |
| jump | a normalized 555 level | absolute volts compared against a 5 V lid |
| music | a 0.55 attenuation | the DAC's 100 ms signal-decay circuit, undriven |

Each measured plausibly. Each was standing in for a mechanism, and the giveaway
was always the same: the fitted value had no schematic counterpart. That test —
*can I point at the part?* — is faster than any spectral comparison and it never
gave a false answer here.

Asteroids added a fifth entry and a variant. Its thrust carried a fitted output
gain of 0.12, and what that was covering was not a missing stage but a **broken
one**: the noise register ran a 42-state cycle where the board's runs 32767, so
the voice was a 286 Hz tone rather than noise. A band-pass with a Q of 7.6 rings
at its own centre whatever it is fed, so a ringing filter needs far more make-up
than a properly excited one, and the fitted gain was supplying it. Correcting the
register drove the same gain into hard clipping at full scale, which is how the
dependency announced itself.

Two things generalise. **A wrong shift direction in a shift register does not
fail, it runs a different polynomial**, usually a much shorter one, and a short
cycle is a tone wearing a noise source's name. `LfsrSpec::cycle_length` exists so
that is one assertion rather than an investigation; assert it for any register
whose output is supposed to be noise. And **the stage after a defect can hide it
from every metric you would think to check**: this voice's pitch, centroid and
fundamental all matched the board throughout, because they were the filter's and
not the source's. Crest factor was the only thing that could see it, which is the
second time that metric has been the one worth keeping.

**A value can be correctly derived and still be wrong, if the derivation threw
away what the model needed.** Reading the schematic afterwards showed the rest of
Asteroids' thrust chain was built from real parts all along, and that the one
value this document had written off as invented, a 1170 Ω input resistor "no
series makes", was `47 kΩ ∥ 1.2 kΩ` to four figures. R76 carries the signal in
and R100 ties the node to the reference; the builder wants both, because a
multiple-feedback band-pass takes its centre frequency and Q from their parallel
combination and its centre gain from the input resistor alone. Passing the
parallel value as a single input resistor made fc and Q exact and the gain wrong
by a factor of forty.

That is worth separating from a fitted constant, because it defeats the test that
catches those. *Can I point at the part?* had a plausible answer here: the number
was a real network's real impedance. What it was not was the argument the model
asked for. **When a primitive takes several component values, its argument list
is a topology, and an equivalent value is not equivalent to it.** The check that
would have caught this is dimensional rather than numerical: ask what each
argument is for, and whether one number can carry two jobs.

Two consequences followed the correction, and only one was the point. The gain
came right, and so did the level, which had been closed with a fitted constant
before. The second was that the forty-times overdrive had been pinning the
modelled op-amp against its rails: the band-pass node sat at 11.75 V into a 12 V
rail, and the clamping flattened exactly the peaks that make filtered noise sound
like noise. Correcting the arguments moved that node's crest factor from 3.11 to
3.31 without touching a filter. **A modelled rail is a real nonlinearity, so an
overdriven stage does not merely sound loud, it sounds wrong.** An implausible
internal level is worth checking on any stage whose primitive clamps.

**Diagnosing from the output blames the stage nearest the output.** Jump's
residual was attributed to the emitter follower on the strength of a waveform
read off the final mix, and two independently rebuilt voices seemed to confirm
it. The node dump showed the follower matched to within 7 % of its time constant
and the oscillator to within half a percent of its duty; the error was in the
envelope, three stages upstream. An output comparison cannot localise, and it
will systematically accuse the last stage before it.

**"One metric improves, another degrades" meant a missing mechanism every single
time.** It fired on jump's level-versus-spectrum, on stomp's decay-versus-band
split, and on walk's low end versus its body. Not once did it turn out to be a
constant needing a nudge. Treat it as a positive signal rather than a nuisance:
it is the model telling you where it is incomplete.

**Structurally right can be audibly worse, and that has to be said out loud.**
Walk's rebuild closed a 13-point gap below 150 Hz and made its decay exact — and
the result sounded thinner, because a band the fitted filter had been holding up
went 16 points light. It shipped anyway, because the alternative was to re-fit
the same compensation over a mechanism that is now genuinely present. But it
shipped *labelled*: the commit says it is a regression, the issue was repointed
to the new symptom, and the source comment says which side of the trade is
structural. A fidelity project accumulates these; the danger is not making the
trade, it is making it silently and then forgetting which numbers are load
bearing.

### What the second board taught immediately

Galaxian's first comparison reported all three voices badly wrong: the melody an
octave low, the hit an octave low, the fire's decay twice as long. One line
explained every number. The `sndcmp` target converted output samples to device
cycles with the board's *sound* clock (1.536 MHz, what the melody's note counter
divides) where `GalaxianSound::tick` counts *main-CPU* cycles (3.072 MHz). Every
Galaxian capture ran the board at half speed.

**Both halves of an experiment need verifying, not just the reference.** The
lesson above, prove the reference responds to its own timeline, was applied, and
the reference was fine. Nothing asked the same question of our side. `sndcmp
verify`'s two checks did not catch it and could not: the null check asks whether
an untriggered window is silent and the sensitivity check whether a 30 ms shift
changes the capture. Both are *relative*, and a board running at half speed
passes both. The checks establish that the harness responds to the stimulus;
they say nothing about the rate at which it does.

**A unit-conversion constant belongs to whichever side owns the unit.** The
device exported `SOUND_CLOCK` with a doc comment inviting a harness to use it for
exactly this conversion, on the wrong constant, since `tick` counts CPU cycles.
The harness took the invitation. `CPU_CLOCK_HZ` was right there, unexported.

**A test that builds its subject from the expression under test proves nothing.**
`a_second_of_samples_is_a_second_of_cpu_time` constructed a target with
`SOUND_CLOCK / rate` and then checked the target's cycle accumulator did not
drift, which it did not, at either clock. It now compares against the machine's
own `TIMING.cpu_clock_hz`, which is the independent authority, and against the
melody tap's schematic frequency, which is arithmetic off the netlist.

The general form: **a spectral comparison cannot distinguish "the model is
wrong" from "the model is being run wrong."** Before reading a first comparison
as evidence about the circuit, confirm that a quantity computable from the
schematic alone (a divider output, a counter tap) measures where the schematic
puts it. Galaxian's melody offered exactly that, `SOUND_CLOCK/(256-pitch)/16` or
1200 Hz at the pitch under test, and it was the first thing to check.

### The reference's own simulation rate is part of the reference

The discrete engine simulates a netlist at the *audio sample rate*, so a
capture's rate is also its simulation rate. At the 48 kHz default, a few-kHz 555
square has its edges quantised to 20.8 µs, and the capture carries broadband
hash the circuit does not produce. Measured at matched bandwidth, moving
Galaxian's simulation from 48 kHz to 192 kHz drops fire's energy above 8 kHz
from 46.4 % to 39.1 % and its spectral flatness from 0.62 to 0.37.

**Two of Galaxian's three reported residuals were entirely this**, and neither
was a small effect. Fire's 12-point shortfall above 8 kHz became 2.9 points; the
hit's 11-point band split became 1.4. Fire's had already been written up as a
topology difference in the noise source, with a plausible mechanism attached
(our LFSR is clocked at 7920 Hz where the netlist runs it at RNG_RATE/100 and
samples it with the 2V flip-flop). That rebuild was one step from starting, and
it would have "fixed" a gap that did not exist by breaking a source that was
right.

So: **capture references at 192 kHz and band-limit afterwards.** This is now the
default in `verify-reference.sh` (`SND_SIM_RATE`), and the capture commands in
the drivers carry the resample step.

Two traps inside the trap:

- Raising the rate raises the capture *bandwidth* as well as the simulation
  rate, so comparing a 48 kHz capture against a 192 kHz one measures mostly the
  bandwidth change. Resample to a common rate first. The confounded comparison
  says the reference gained high-frequency energy; the honest one says it lost
  it.
- This is a property of the **discrete engine specifically**. A board on the
  netlist subsystem has a real solver with its own internal timestep, so its
  output rate is only an output rate. Do not carry the rule across without
  checking which engine the board uses.

The general form, and it generalises past audio: **when a reference is itself a
simulation, its numerical parameters are part of the experiment.** The
reference-verification rule above asks whether the reference responded to the
stimulus. It does not ask whether the reference was converged. A capture can
respond perfectly to its own timeline and still be reporting its own
discretisation, and a residual that shrinks when you refine the reference was
never the model's.

### Establish that a metric is stable before drawing anything from it

Donkey Kong's walk produced four different conclusions from one set of captures,
in both directions, across two sessions. None of them came from new evidence.
The cause was not any single mistake but a property of the measurement: **on a
transient effect, a band share is not stable enough to compare.**

The walk is a thump that is over in about 40 ms. Measured from each side's own
onset over three windows differing by 10 ms of start or 50 ms of length, the
150-400 Hz delta swung about five points and changed sign twice, on our capture
and the board's alike. Every reading of it, favourable and unfavourable, was
noise dressed as a finding.

What stayed stable was **crest factor**, which describes pulse structure and
cannot be faked by a gain or a filter: it ordered the three builds identically
in every window. Full-window band shares were also stable, because averaging
over the whole decay is what removes the sensitivity.

So, before a number becomes evidence: **move the window and see if the number
moves.** Two offsets and one length change take a minute and would have saved
this issue twice over. A metric that is not reproducible under that test does
not become reproducible by being measured more carefully.

**Which metrics survive is a property of the voice, not of the metric.** Run on
Asteroids' thrust, a sustained rumble, the same test came out differently from
Donkey Kong's walk in both directions. Over six windows, two start shifts each
way and two length changes:

| delta, ours minus board | across six windows | verdict |
|---|---|---|
| crest factor | -1.29 to -1.55 | stable, never changes sign |
| 0-150 Hz share | +0.80 to +1.14 pp | stable, and nearly uninformative: both sides are over 98 % sub-150 |
| centroid | -0.9 to -2.7 Hz on 88 Hz | stable, about 2 % |
| level | +0.49 dB to -0.33 dB | **not stable, changes sign** |

So the band share that was noise on a 40 ms transient is steady on a sustained
voice, and the level that is normally the most robust thing in the report is the
one that moves: a 2 s window of a noise voice does not have a settled RMS, and
the first comparison's "levels agree to 0.06 dB" was the window being kind. The
honest claim from this set is that they agree to about half a decibel.

Crest factor was stable on both boards, and on Asteroids it is also the only
metric here with anything to say: 1.7 against the board's 3.2 at matched RMS and
matched centroid. That is the pattern to expect from this test rather than a
ranking of metrics to reuse. **Run it per voice.**

Four specific traps, each of which cost real time here:

- **A `sndcmp capture` output is already the analysis window.** Re-ranging it
  shifts the span. This produced a comparison of the board from its assert
  against ours from 50 ms before ours, and a confident wrong retraction.
- **Never align a decaying effect by cross-correlation.** The aligner slid the
  board 528 samples, 12 ms, on a 40 ms thump, and cheerfully compared our attack
  against its tail. Align each side on its own onset instead, and make both
  windows the same span of the same event by construction rather than by search.
- **A tool's own diagnostics need the same scepticism as its measurements.** The
  advice above used to end "and check the tool reports the same onset and an
  envelope alignment near zero", which assumed the aligner. It was scoring
  candidate offsets by the mean of the products of the two envelopes, so sliding
  the overlap off a quiet stretch raised the average and scored better: with the
  50 ms of silence every scenario leaves before its trigger, the aligner reported
  **-70 ms for a capture against a byte-identical copy of itself**, and it missed
  a known 50 ms delay by the same amount. Normalizing by both segments' norms
  makes the score a cosine, which Cauchy-Schwarz pins at exactly 1 at zero shift,
  and both cases became exact. Nothing about the wrong number looked wrong; it
  was found by asking the tool to align a file with itself, which took a minute
  and is now a test.
- **A sustained voice does not say where it is in time.** Even fixed, the
  alignment of a steady rumble against its own copy stands only 0.028 above a
  typical offset, where every transient tried clears 0.05 comfortably. The offset
  is therefore whatever the noise favoured, and `audiodiff` now says so beside
  the number rather than printing it bare. The general form: a statistic computed
  over a signal that cannot determine it is not a small error, it is a confident
  answer to a question the data did not contain.
- **Exclude post-decay silence from an STFT comparison.** Our output is
  digitally silent after a one-shot; a reference has a noise floor. A
  log-magnitude distance over three seconds of that reported 12.3 where the
  driven span alone reports 1.3, and it was read as a real defect.
- **A centroid shift through a filter is not a filter comparison.** The same
  filter produces different centroid shifts on two different input spectra. To
  compare two chains, measure each one's own input-to-output magnitude ratio.

### The reference records the mix, not the mix you hear

This was Phase 0, and the answer is that `-wavwrite` taps the speaker mix
**before** the `<audio_effects>` chain. Every capture ever taken here is raw
mixed netlist output. The finding is a negative one, so what is worth keeping is
the shape of the check rather than the answer.

**From the source.** The recording buffer has exactly one writer. It is filled in
`sound_manager::output_push`, at the moment a speaker device's inputs arrive,
straight out of the stream's input buffer and clamped to 16 bits. The effects run
later, in `run_effects`, over a copy in a separate buffer, and their output feeds
only the OSD output streams. There is no path from an effect back to the
recording buffer, and the field's own declaration says so. The same is true in
the 0.287 tag and in 0.288-dev, byte for byte. The Lua sound hook reads the same
pre-effects speaker buffer, so it is not an alternative tap.

**From an experiment, because a source reading is a claim about a program and
not about a file on disk.** Two captures of the same four seconds of Asteroids,
differing only in the per-game cfg: one stock, one carrying a compressor at 20:1
with +24 dB in and out and a five-band equalizer at +18 dB per band. The captures
are byte-identical. So is a third with `-volume -20`.

Three identical files prove nothing on their own, which is the trap this document
has fallen into before in the other direction: a check that cannot fail is not a
check. The control is a `<device_volume gain="0.5">` on the discrete device,
which sits in the stream mixing and therefore *upstream* of the tap. That capture
differs, and by 0.4999 in RMS. So the capture does respond to a cfg-driven mixer
change; it is the effects chain specifically that it cannot see. MAME rewrites
the cfg on exit with the compressor and EQ attributes intact, which is only
possible if it parsed them into the speaker's live effect objects.

**What MAME's stock chain actually does**, since the issue's premise was that
Asteroids' cfg "carries" a compressor and an EQ: that cfg is what MAME writes for
every game. All four effects are listed because the chain is fixed; the listing
records no non-default parameter. Compressor and Reverb default to mode 0, off.
The Equalizer is on with every band at 0 dB. Only the Filters stage does anything
at all, a 20 Hz Q=0.707 high-pass with its low-pass disabled. So the difference
between the played sound and the capture is a DC blocker, on both sides of any
comparison anyone has made by ear.

The general form: **when a reference tool has a processing chain, find the tap in
its source, then prove the tap by moving something on each side of it.** One
identical pair says the knob was ignored. An identical pair plus a control that
does move says where the tap is.

### Gate the source, not its output

Asteroids' thump was rebuilt from the schematic until every measurement of it
agreed with the board: waveform high and low levels within 1 %, duty within
0.2 points, fundamental and 85 % rolloff exact to the reported precision,
harmonic and non-harmonic energy both within a fifth of a decibel, level within
1.7 dB, and not one clipped sample in forty seconds of recorded play. It still
sounded wrong, and the report that located it was one sentence: *scratchiness
right as it starts.*

The model gated the voice by multiplying the filtered output by the enable line,
with the 555 free-running underneath. So switching thump on connected an
oscillator at whatever phase it happened to be passing, and the largest
single-sample step at the onset was 3636 where the corrected version's is 272.
The board does not do that. Its enable drives the timer's **reset** pin, which
holds the capacitor discharged and the output low, so every onset begins from
the same state. Moving the gate there is what fixed it, and it changed no
steady-state number at all.

Two things to take from it.

**A gate belongs where the circuit puts it, and on this class of part that is
almost never the output.** `oscillator → filters → multiply(enable)` is an
inviting shape and it is wrong wherever the real enable reaches a reset, a
discharge pin or a supply. The tell is that the voice is correct once running
and wrong at its edges.

**A steady-state comparison cannot see a transient defect, and the listening
test is not a formality.** That 3636-sample step sat *inside* the analysis
window and moved nothing: it is one sample in ninety thousand, so RMS, crest,
centroid and every band share absorbed it without a flicker. Every metric this
document recommends was computed, was stable across five windows, and agreed.
The defect was audible immediately. Where the metrics and the ear disagree,
suspect the metrics of not measuring the thing, and ask *when* it sounds wrong
rather than how much.

## Part 5 — Construction-time tuning

> **NOT BUILT, and deliberately so.** Parts 5 and 6 describe a fitting loop, and
> the boards done since concluded that fitting is the wrong tool for this
> problem: every value ever fitted here was later deleted, and each was standing
> in for a mechanism missing from the model. The reasoning is in "What finishing
> the first board taught" and "What the second board taught immediately" above,
> and in the close reasons on `…-l5r3.6` and `…-l5r3.7`.
>
> The text stays because two pieces keep independent worth and are the ones to
> restore first if this is ever revisited: **sensitivity ranking** (Part 6,
> "Baseline and sensitivity"), which is a structural signal rather than a
> fitting step, and **preferred-value snapping** with the identifiability
> analysis. The optimizer itself should wait for a residual that has survived a
> node dump and a schematic reading.


A small construction-only API, in `phosphor_core::device::discrete::tuning`:

```rust
pub struct ParameterSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub unit: ParameterUnit,          // Ohms | Farads | Hertz | Volts | Seconds | Ratio
    pub class: ParameterClass,        // Schematic | Model | Calibration
    pub default: f64,
    pub bounds: ParameterBounds,
    pub tolerance: Option<f64>,
    pub scale: SearchScale,           // Log | Linear
    pub preferred_series: Option<PreferredSeries>,
    pub source: SourceLocation,       // #[track_caller] or a small macro
    pub writeback: Writeback,         // Literal | Derived — see Decision 7
}
```

The registering macro captures `stringify!` of the value expression along with
the caller location, so `writeback` is decided at registration with no source
parsing: a float literal is `Literal`, anything else is `Derived`.

A sound constructor registers what it is willing to expose:

```rust
let walk_c = tuning.value(ParameterSpec::schematic(
    "dkong.walk.c", "Walk 555 timing capacitor", Farads, WALK_C, tolerance(0.20),
));

let jump_tau = tuning.value(ParameterSpec::model(
    "dkong.jump.envelope_tau", "Jump behavioral envelope time constant",
    Seconds, JUMP_TAU, bounded(0.05, 1.0),
));
```

The returned value is the override if present, otherwise the default. The normal
`new()` passes an empty context; the comparison adapter uses `new_with_tuning()`
and receives the discovered specs. `TuningContext` exists only during
construction — resolved scalars land in the same node fields the emulator
already uses, so the simulation step performs no lookups, no string handling and
no tooling branches.

### Overrides are a file, so the loop closes without a rebuild

`TuningOverrides` is a plain map of parameter ID to value, and it deserializes
from TOML:

```toml
# /tmp/dkong-fit.toml — written by `sndcmp fit --out-tuning`
schema = 1
target = "dkong-discrete"
fitted_against = "dkong/jump"
reference_sha256 = "..."

[overrides]
"dkong.jump.c" = 5.12e-8
"dkong.jump.envelope_tau" = 0.31
```

Any consumer that builds a machine can pass one in — `sndcmp`, `disasm`, and the
frontend behind an explicit `--tuning` flag. That is what removes the round
trip: a fitted value is audible in the actual emulator, on the actual board,
against the actual music, before anyone edits a line of Rust.

Two guards, because a stray tuning file silently altering emulation would be far
worse than the problem it solves. Overrides are **opt-in only** — never an
implicit file lookup, never an env var that could linger in a shell — and any
machine built with overrides active says so loudly at startup and in the debug
overlay. A tuning file also records what it was fitted against, so a stale one
applied to a changed scenario is visible rather than silent.

Golden frames and save states are unaffected: overrides change audio
coefficients only, and a save state already reconstructs its circuit from the
board's constructor.

### The derivation refactor

Independently of tuning, each builder's inlined coefficient math becomes a free
function in `core/src/device/discrete/derive.rs`:

```rust
pub(crate) fn rc_tau(ohms: f64, farads: f64) -> f64 { ohms * farads }

pub(crate) fn ne555_astable_coeffs(r1: f64, r2: f64, c: f64, dt: f64) -> (f64, f64) {
    (1.0 - (-dt / ((r1 + r2) * c)).exp(),
     1.0 - (-dt / (r2 * c)).exp())
}
```

This is worth doing on its own merits: it removes the duplication between
`rc_low_pass` and `low_pass_hz` (two spellings of one derivation) and gives the
coefficient math a place to be unit-tested directly, rather than only through a
built circuit's output. It is a mechanical extraction with no behavior change,
and lands with bit-identical output and green golden frames.

### Save-state

No change. Topology and static configuration are already reconstructed by the
board's circuit constructor on load, so a tool-set parameter is not restored by
a load — which is correct: a save state captures a run, not a circuit edit.
Worth a comment in `Saveable`, nothing more.

## Part 6 — Sensitivity and search

> **NOT BUILT.** See the note under Part 5. The objective's shape did survive:
> `disasm audiodiff` reports level, envelope and spectrum separately rather than
> as one aggregate score, which is what this section argued for and is what made
> "one metric improves, another degrades" readable as the missing-mechanism
> signal it turned out to be.


### Baseline and sensitivity before any optimization

Capture the default model, print metric deltas, spectrograms and integrity
warnings. Many structural defects are identifiable with no search at all.

Then perturb each selected parameter independently around its default, using its
declared scale and bounds, and report which metric families move:

```text
parameter                  shape   envelope  level   direction
dkong.jump.c               high    medium    low     larger -> lower pitch
dkong.jump.envelope_tau    low     high      high    larger -> longer tail
dkong.jump.output_gain     none    low       high    larger -> louder
dkong.walk.r1              none    none      none    unrelated
```

That ranking is itself a recommendation: it says which part of the model can
plausibly explain the observed error, before anyone spends evaluations.

### Objective

Three reported components, never one unexplained score:

```text
shape_score     phase-insensitive spectral/timbre difference
envelope_score  aligned attack/decay difference
level_score     raw level/energy difference, when provenance is trusted
```

The aggregate is a documented weighted sum used for *ranking candidates*, not
the only evidence shown.

`shape_score` is multi-resolution log-magnitude STFT distance:

```text
D = Σ_w  mean| log(|STFT_w(ref)| + ε) − log(|STFT_w(phos)| + ε) |
```

over `w ∈ {256, 1024, 4096}`. It is phase-blind, which matters because two
independent emulators are never sample-aligned and their noise sources never
correlate sample-for-sample — any time-domain distance measures alignment, not
timbre. Short windows see attack and decay, long windows see steady-state tone;
one FFT over a whole segment sees neither. Log magnitude matches perceived
loudness and stops one loud band from dominating. Envelope distance is scored
separately so a loud band cannot hide a wrong decay.

Alignment is by envelope cross-correlation within a bound derived from the
declared trigger, shifting only for analysis and reporting the offset as a
metric. Waveform cross-correlation is never used for noise-heavy effects: it
would reward seed and phase coincidence rather than circuit fidelity.

### Search ladder

The objective is cheap, non-differentiable and plausibly multi-modal. No
autodiff, no heavy dependency.

- **1 parameter** — bounded grid, then local refinement.
- **2 parameters** — coarse grid plus local pattern search; also produces a cost
  surface worth plotting.
- **3–4 parameters** — multi-start derivative-free pattern / Nelder–Mead in
  normalized coordinates (~100 lines hand-rolled).
- **More than 4** — refuse by default, and ask for a smaller hypothesis.

Ohms, farads, frequencies and time constants search in log space; signed offsets
and small linear gains search linearly. Bounds default to ×⅕ … ×5 of the
default, overridable. Every trial starts from a fresh device; serial execution
first, parallel later if it proves worth the determinism risk.

### Preferred component values

A continuous optimum of `8.2431e-3 Ω·F` is not a recommendation. Continuous
search is a diagnostic step; recommendations are re-scored at actual E6/E12/E24
values and checked against declared tolerance, reporting all four of: continuous
optimum, nearest realizable value, score at the realizable value, and schematic
compatibility.

```text
THRUST_RC.ohms   2200  ->  3900   (E24 3900, continuous 3874, +0.6%)
```

If the snapped score is much worse than the continuous one, the fit sits in a
narrow minimum — which is itself a finding, and reported as one. Series/parallel
combinations are not inferred in v1.

### Identifiability

Fitting several parameters against one scalar finds compensating errors: a
too-bright noise source plus an over-strong low-pass gives the right centroid and
the wrong everything. Mitigations, all reported:

- Keep the parameter set small and explicit — there is no "tune everything" mode.
- Report the condition number of the local Hessian estimate; an ill-conditioned
  fit means the individual values are untrustworthy even when the total is.
- Report pairwise sensitivity correlation, and warn when many combinations score
  nearly equally, or when the optimum lies on a bound.
- **Hold-out across effects.** Fit on one segment, score on another sharing the
  node — Asteroids' thrust and explosion share `NOISE`; a Donkey Kong shared
  output or DAC parameter fitted on jump must be re-scored on walk and stomp. A
  fit that improves one and degrades the other is overfitting a shared node.
- Prefer the schematic default when the evidence is indistinguishable.

## Structural diagnosis

The tooling cannot synthesize a missing netlist, but it can recognize when
scalar tuning is the wrong response:

| Observation | Likely area |
|---|---|
| Fundamental matches but harmonic ratios do not | Wrong waveform, duty cycle, clipping, or missing filter |
| Pitch contour wrong but steady pitch matches | Control-voltage / envelope topology |
| Spectrum matches but attack/decay does not | Trigger semantics or envelope model |
| Band balance responds cleanly to one RC value | Plausible component adjustment |
| Large DC with little AC | Locked source, wrong bias, missing coupling capacitor |
| Flat peaks and excessive high harmonics | Clipping or excessive gain |
| Noise has stable narrow lines | Short LFSR cycle, clocking error, unintended oscillator |
| Impulse response matches but output does not | Source error, not filter error |
| No exposed parameter materially improves the score | Missing/wrong structure, or a wrong reference procedure |
| Best fit needs out-of-tolerance schematic values | Structural mismatch compensated by a scalar |
| One effect improves while another sharing the path degrades | Overfit, or wrong shared topology |
| A metric changes sign when the window moves by a few ms | The metric, not the model. Establish stability before believing it |
| STFT large while every band agrees | Compare the driven span only; post-decay silence against a noise floor dominates a log-magnitude distance |
| Excess or shortfall confined to the top octave, on a voice with fast edges | Check the reference's simulation rate before the model |
| Residual shrinks when the reference is re-simulated at a higher rate | The reference's discretisation, not a model error |
| Every voice on one board is out by the same exact factor | A clock or divider feeding the whole device, including the harness's |

A structural finding reports the metric evidence, the sensitivity evidence, the
relevant probe captures, the schematic limits, the best *rejected* scalar fit,
hypotheses expressed as hypotheses, and a suggested next inspection.

## Reference captures and provenance

### The capture is not automatically ground truth

Phase 0's question was whether MAME 0.287's `-wavwrite` taps the mix before or
after the per-game `<audio_effects>` chain. If after, every reference WAV in the
existing rig already contained a compressor and an EQ, and fitting component
values to match it would have been fitting Phosphor to MAME's post-processing.

**Settled: `-wavwrite` taps the speaker mix BEFORE the effects chain.** Both
halves of the evidence are recorded in "The reference records the mix, not the
mix you hear" in Part 4. The reference captures are the raw mixed netlist output,
which is what the whole rig assumed. Nothing on this branch has to be requalified.

Two consequences that are not restatements of that:

- The tap is also before the master gain and before the output mixing steps, so
  `-volume` and the mute state cannot reach a capture either. `master_volume_db`
  in the manifest below is therefore bookkeeping rather than a correction to
  apply.
- MAME's *played* sound is not the capture. With a stock configuration it
  differs by a 20 Hz Butterworth high-pass, which is the only default-active
  processing in the chain. That is inaudible against anything these boards make
  above 40 Hz, but it does mean an ear comparison against MAME's speaker is
  comparing a DC-blocked signal against our capture's raw one.

### Nor is it automatically converged

A second way a capture fails to be ground truth, found on Galaxian and written
up in Part 4: for a board on the discrete engine, the capture's sample rate *is*
the simulation rate, so a 48 kHz reference reports its own edge quantisation
above a few kHz. `sample_rate_hz` in the manifest below is therefore not
bookkeeping. It is the reference's numerical resolution, and a capture taken at
the 48 kHz default should be treated as untrustworthy in its top octave
regardless of how authentic its provenance is.

### Manifest

Each reference WAV is accompanied by a manifest describing what happened,
without embedding Lua or a MAME memory map into the Rust tool:

```toml
schema = 1
scenario = "dkong/jump"
wav = "jump.wav"

[capture]
producer = "mame"
producer_version = "0.287"
romset = "dkong"
sample_rate_hz = 192000        # the SIMULATION rate too; 48000 is not converged
channels = 2
channel_policy = "downmix"     # mono | left | right | downmix
master_volume_db = 0.0
post_processing = "disabled"
config_directory = "clean"
level_trust = "trusted"        # trusted | relative-only | untrusted
lua_sha256 = "..."

[timeline]
duration_s = 3.0
analysis_start_s = 0.45
analysis_end_s = 3.0
trigger_s = 0.5
release_s = 0.55

[provenance]
captured_at = "2026-08-18T00:00:00Z"
notes = "Main Z80 parked; DAC/music path idle"
```

`sndcmp validate-reference` checks the manifest against the WAV and reports
missing or untrusted provenance. It cannot prove an external producer really
disabled processing, but it can stop a capture with unknown conditions from
silently becoming an optimization target. **The tool refuses to fit against a
capture with no manifest.**

Channel policy is explicit because the current Python behavior — silently take
the first channel — is not sufficient for a trusted absolute-level comparison.

Reference WAVs live outside the repository by default, rooted at
`PHOSPHOR_SOUND_REFS` with an explicit CLI path taking precedence.

## CLI

```text
disasm audiodiff A.wav B.wav [--png spec.png] [--json]

sndcmp targets
sndcmp scenarios [TARGET]
sndcmp validate-reference REFERENCE.toml
sndcmp capture SCENARIO --out phosphor.wav [--probe ID]
sndcmp compare SCENARIO REFERENCE.toml [--candidate WAV]
sndcmp params TARGET
sndcmp sensitivity SCENARIO REFERENCE.toml [--params ID,...]
sndcmp fit SCENARIO REFERENCE.toml --params ID,... [--out-tuning FIT.toml]
sndcmp match SCENARIO REFERENCE.toml --params ID,... [--out-tuning FIT.toml]
sndcmp writeback FIT.toml [--apply]
sndcmp validate-machine MACHINE SCENARIO REFERENCE.toml [--tuning FIT.toml]
```

`match` composes capture, compare, sensitivity and optional bounded fitting; the
lower-level commands stay available for inspecting intermediates.

A comparison writes a report directory containing `report.txt`, `report.json`,
`metrics.csv`, the normalized reference, the Phosphor capture, both
spectrograms, an envelope plot and `search.csv`. The terminal summary stays
concise; JSON and CSV carry the evidence.

### What a recommendation looks like

```text
dkong.jump.c
  class: schematic
  source: machines/src/dkong_sound.rs:...
  schematic/default: 47 nF (tolerance ±20%)
  best continuous fit: 51.2 nF
  preferred candidate: 47 nF (E12)
  score change: 0.284 -> 0.119
  interpretation: within capacitor tolerance; plausible

dkong.walk.r2
  class: schematic
  schematic/default: 27 kOhm
  best fit: 82 kOhm  (+204%)
  condition number 14.2 (well conditioned)
  hold-out (stomp): 0.191 -> 0.244  (degraded)
  interpretation: reject as a component recommendation; component tolerance
                  does not explain 204%. Suspect wrong topology, waveform,
                  control voltage, or trigger model.
```

The report uses fixed verdict terms — `plausible schematic adjustment`, `model
refinement`, `calibration pending trusted level reference`, `structural mismatch
suspected`, `reference provenance insufficient`, `ambiguous/non-identifiable` —
and must never present an optimizer result as ground truth merely because its
score is lower.

`sndcmp writeback` then turns the accepted half into an edit. For a `Literal`
parameter it emits — and with `--apply`, lands — an ordinary diff:

```diff
--- a/machines/src/dkong_sound.rs
+++ b/machines/src/dkong_sound.rs
@@
-const JUMP_C: f64 = 47e-9;
+const JUMP_C: f64 = 51e-9;
```

For a `Derived` parameter it refuses and says why, naming the expression it will
not overwrite:

```text
asteroids.explode.level  fitted 0.181 (default 0.164)
  source: machines/src/asteroids_sound.rs:324
  expression: LVL_EXPLOSION / LVL_TOTAL
  writeback: refused — this value is a ratio of mix levels, not a free scalar.
             Applying 0.181 here would silently decouple it from LVL_TOTAL.
             Edit LVL_EXPLOSION (1000.0) instead, or reconsider the mix.
```

Values rejected by the reference policy — out of tolerance, ill-conditioned, or
degrading a hold-out effect — are never written, with or without `--apply`.
`--apply` lands what the report already recommended; it is not an override for
the policy.

## Coverage catalog

`tools/sound-compare/targets.toml` is the reviewed inventory of known discrete
paths, so a newly discovered or newly added one cannot quietly drop out of the
plan:

```toml
[[target]]
id = "dkong-discrete"
machines = ["dkong"]
status = "implemented-needs-validation"
adapter = "dkong"
scenarios = ["walk", "jump", "stomp"]
authority = ["schematic"]
references = ["mame"]
```

Statuses: `missing`, `partial`, `implemented-unvalidated`,
`implemented-needs-validation`, `validated`, `blocked` (reason required). Tests
assert unique IDs, that referenced scenarios exist, that implemented adapters
have entries, that cataloged scenarios name valid controls, and that a target
cannot regress from `validated` without a visible data change. `sndcmp targets`
prints it, making the project-wide plan visible from the CLI.

Seed contents:

| Target | Current state |
|---|---|
| Donkey Kong walk/jump/stomp | Implemented; known incorrect, carries a DC offset |
| Donkey Kong Jr. TKG-04 | Shares the device, different sound program |
| Congo Bongo percussion | Implemented synthesis; no comparison |
| Asteroids / Lunar Lander | Implemented; existing Lua captures |
| Galaxian voices + Moon Cresta family | Implemented, shared board |
| Asteroids Deluxe discrete | Register writes stubbed; POKEYs work |
| Mario Bros. walk/skid | Explicitly deferred |
| Galaga / Xevious Namco 54XX | Commands discarded; explosion path silent |
| Konami/Scramble/Frogger output filters | Latched but not modeled |

## Testing

**Derivation and tuning.** Extracted `derive::*` functions reproduce the current
inline math bit-for-bit. Empty overrides reproduce the normal constructor
bit-for-bit; applying a default explicitly produces identical output. Bounds,
units, classes and duplicate IDs are validated. Log/linear conversions
round-trip. Preferred-value snapping behaves.

**Analysis against synthetic signals.** Sines at known frequency and amplitude,
squares and triangles with known harmonic ratios, white and colored noise, known
DC and clipping, exponential attacks and decays, swept tones, delayed copies for
alignment, and multiple rates and channel layouts. A 1 kHz sine reads centroid
≈1000 and flatness ≈0; white noise reads flatness →1; a known decay reads the
right T20. These are what make the analyzer trustworthy, and the Python version
had none.

**Distance sanity.** `D(x, x) == 0`, and `D` increases monotonically as a
synthetic signal is detuned.

**Overrides and write-back.** A `tuning.toml` round-trips through
`TuningOverrides`; a file of defaults produces output identical to no file at
all; an unknown parameter ID is an error, not a silent no-op. `writeback`
classification is asserted per parameter across every registered target, so a
refactor that turns a const into an expression flips the parameter to `Derived`
and fails the test rather than silently enabling a wrong edit. Applying a
`Literal` writeback to a fixture source produces the expected diff and the
result still compiles; a `Derived` one refuses.

**Scenario and adapter.** Every action names a declared control with a matching
type; events land on exact output-sample indices; reset and baseline controls
produce silence where expected; one-shot scenarios contain exactly one rising
edge; sustained ones contain the declared release; capture length is
deterministic.

**Optimizer recovery — the honest end-to-end test, needing no MAME.** Capture
the default model, construct another with one deliberately wrong exposed
parameter, fit it against the default capture, and assert recovery within
tolerance. Separate tests use deliberately wrong *waveform or envelope
structure* and assert the fitter refuses to claim a trustworthy schematic
recommendation. Both run in CI.

**Device regressions,** added once a target is accepted: AC rather than raw RMS
for audibility, pitch/envelope/energy ranges, absence of clipping or unexpected
DC, deterministic output for a fixed scenario, reset and save/load continuation,
relative effect levels where meaningful. Exact PCM hashes may be useful
diagnostics but must not be the only contract — a shared resampler improvement
can legitimately change every sample while improving the sound.

**Reference comparisons stay local and optional:**

```bash
PHOSPHOR_SOUND_REFS=/path/to/refs cargo test -p phosphor-sound-compare --test references
```

They skip clearly when references are absent. CI stays fully functional on
synthetic and Phosphor-generated fixtures.

**Performance.** Benchmark with `phosphor-bench` before and after introducing
tuning registration; no hot-loop regression is acceptable. Separately, measure
candidate evaluations per second, since that sets whether a search is practical.

## Phases

Sized so each lands independently and so value arrives before the fitting
machinery does. Beads to be created under one epic; `phosphor-emulator-audiodiff-76wx`
is absorbed by Phases 1–2 and 4.

**Phase 0 — Settle the reference. DONE for the tap point.** MAME 0.287's
`-wavwrite` taps the speaker mix before the `<audio_effects>` chain, established
from the source and confirmed by an experiment with a control; see "The reference
records the mix, not the mix you hear" in Part 4. Every capture the rig has ever
taken is raw mixed netlist output, so nothing published on this branch rests on a
compressor or an EQ. The rest of the phase's scope was overtaken: the canonical
capture procedure now lives in `verify-reference.sh` and the capture-rate rule,
the single-trigger protocol became `drive_dkong_single.lua`, and the manifest
schema above is unbuilt because `fit` was dropped along with Phase 6.

**Phase 1 — `analysis` module + `disasm audiodiff`.** The shared DSP in core,
the differ in disasm, spectrogram PNG via `gfxsheet::write_png`, tolerance-based
exit codes, tests against synthetic signals. Immediately useful, gates nothing
else, and retires `compare_wav.py` as the documented path.

**Phase 2 — Registry audio sanity test.** The declared-silence registry field
and the ROM-less sweep. Cheapest real defense; closes
`phosphor-emulator-audio-dc-offset-g7p4`'s class of bug.

**Phase 3 — Derivation refactor.** Extract `derive::*`; builders keep calling
them; prove bit-identical output and green golden frames. Pure refactor, valuable
even if everything after it is dropped.

**Phase 4 — The crate, scenarios, capture, probes.** `tools/sound-compare`, the
scenario schema, the Donkey Kong adapter, `capture` / `compare` /
`validate-reference`, per-voice solo render and impulse response. Delete
`dkong_capture.rs`. At this point the tooling is useful with no parameter search
at all.

**Phase 5 (tuning, overrides and sensitivity). DROPPED, not deferred.** The
override file's premise was that hand-exploring a value should not need a
rebuild. The loop we actually run is "understand the topology, express it,
rebuild", where a one-minute cargo build is nothing against reading a netlist,
and an override file mainly makes it easy to keep a value alive with no part
behind it. `TuningContext` named a real problem, that builders collapse their
arguments so a coefficient cannot be turned back into a component; the answer
that stuck was simpler than a parameter registry, which is to keep schematic
values as literals at the call site with the part designator beside them. What
is worth restoring first is sensitivity ranking, which is a structural signal
rather than a fitting step.

**Phase 5b — Node-level comparison.** Diff a named node against MAME's
corresponding netlist node rather than only the final output. Inserted here, and
ahead of `fit`, on the first board's evidence: with a five-stage chain, an output
that disagrees says nothing about which stage is at fault, and the work collapses
into editing constants and watching one metric improve while another degrades.
`…-discrete-node-compare-2b7f`.

**Phase 6 (`fit`, `writeback`, and correct Donkey Kong). DROPPED, not deferred.**
Its own deliverable, "correct Donkey Kong", was delivered without it, and so
were all three Galaxian voices. What did the work was reading the topology,
modelling it, probing per stage, and comparing against a reference proved to
respond to its own timeline.

Worth less than this document originally assumed, and dangerous earlier than its
place here. See "What the first board taught": fitting a value while the topology
is wrong produces a number that measures well and models nothing, and every such
number had to be undone once the structure was corrected. That was not the
exceptional case, it was every case: four fitted filter corners on Donkey Kong
and two fitted gains on Galaxian, none with a part on the schematic, each
standing in for a missing mechanism. An optimizer would have found excellent
values for every one of those wrong models, which is precisely the failure mode.

The parts that earned their place: the objective's shape, kept in
`disasm audiodiff`, which reports level, envelope and spectrum separately rather
than as one score. The parts to restore first if this is revisited: E-series
snapping and the identifiability analysis. The trigger for revisiting: a
residual that has survived a node dump and a schematic reading, which has not
happened yet on either board.

**Phase 7 — Remaining implemented devices.** Congo Bongo (including comparison
against MAME's samples and any original recordings), Galaxian and the shared
family board, Asteroids, Lunar Lander, Donkey Kong Jr., Moon Cresta. Delete
`analyze_wav.py` and the last capture examples. Mechanical once DK sets the
pattern.

**Phase 8 — Missing discrete paths, built with the tooling in hand.** Asteroids
Deluxe latch paths, Mario Bros. walk/skid, Namco 54XX output for Galaga and
Xevious, Konami output-filter controls. These benefit most: construction can
proceed against isolated measurements and schematic stages instead of producing
a plausible sound and tuning it afterwards. This is the first use of the tooling
for construction rather than debugging.

**Phase 9 — Project-wide audit.** Sweep every machine for ignored analog control
writes, deferred sound paths, and chip-to-speaker routing that omits board-level
mixers, coupling caps, selectable filters or output stages. Every finding enters
the catalog. Add a catalog check to the machine-development checklist and keep
the README roadmap synchronized. "All discrete sounds" is complete only when
every catalog entry is `validated` or carries a documented blocker.

## Risks

- **Overfitting to MAME.** The central risk. Addressed by the reference policy,
  parameter classes, tolerance flags, cross-effect hold-out and separate metric
  families — but the mitigation is cultural as much as technical: the output has
  to read as evidence, not as an instruction.
- **Phase 0 comes back badly.** *Retired.* The risk was that `-wavwrite` taps
  post-effects and the chain cannot be cleanly disabled, which would have made
  references much harder to produce. It taps pre-effects, so the chain never has
  to be disabled at all and no existing capture is contaminated.
- **Optimizing the wrong model.** Mitigated by sensitivity-first ordering, probe
  and impulse evidence, structural diagnostics, small parameter sets, and
  reporting bound-hitting and out-of-tolerance fits as findings.
- **The derivation refactor touches working audio code.** Mitigated by requiring
  bit-identical output and green golden frames.
- **`op_amp_band_pass` may be effectively unfittable.** Five schematic
  parameters map to four coefficients, so its schematic space is degenerate by
  construction. Expect ill-conditioning; the tool must say so rather than return
  confident numbers. Fitting `rf` alone with `c1`/`c2` pinned is the realistic
  use.
- **Noise and phase instability.** Mitigated by phase-insensitive metrics,
  envelope alignment, band statistics, deterministic seeds, and never using raw
  waveform error as the primary noise score.
- **Scope growth.** This design is a nine-phase ladder and the top rungs are the
  speculative ones. Bounded by the target catalog, one isolated scenario at a
  time, and the rule that each phase must stand on its own if the next is
  dropped. The metric list is closed: anything a board comparison does not need
  does not go in.
- **A tuning override file becomes a shadow configuration.** The thing that
  removes the round trip also makes it possible to run, demo or benchmark a
  machine that is not the machine in the source tree. Mitigated by opt-in-only
  loading, a loud startup banner, provenance recorded in the file, and the rule
  that a fit is landed or discarded rather than lived in. If overrides start
  showing up in bug reports, that is the signal the write-back path is too
  hard, not that the override path should be removed.
- **Reference asset size and licensing.** Keep external WAVs out of the
  repository by default; commit scenarios and manifests only. Do not commit
  ROMs.

## Open questions

1. Does the shared analysis DSP belong in `phosphor_core::audio::analysis`, or
   in a leaf crate both `disasm` and `sound-compare` depend on? Core is proposed
   — it already owns `AudioResampler`, both consumers already depend on it, and
   a hand-rolled FFT adds no dependency — but it does put offline analysis code
   in the hot-path crate.
2. Whether trusted reference WAVs live only in an external collection, or
   whether a small legally reviewed subset may be committed.
3. ~~The exact clean MAME invocation guaranteeing `-wavwrite` excludes optional
   post-processing for a chosen version.~~ Answered: there is no such
   invocation to find, because there is no invocation that would include it.
   `-wavwrite` records the speaker mix before the effects chain, the master gain
   and the mute state, in 0.287 and in 0.288-dev alike. A capture is not
   configuration-independent, though: anything upstream of the speaker still
   reaches it, which is why `verify-reference.sh` captures into a scratch
   `-cfg_directory`.
4. Is there a Phosphor-side equivalent of MAME's effects chain to worry about?
   `AudioResampler` sits between the circuit and the WAV; the capture path
   should tap the circuit output directly, and this should be verified rather
   than assumed.
5. Do any boards need a per-scenario `sim_rate` override to fit accurately, and
   if so is `sim_rate` itself a tunable? It changes derived 555 and biquad
   coefficients, so it is not neutral.
6. Should alternate-output reconstruction cover all initial probes, or is
   simultaneous multi-node capture needed early?
7. Hand-written optimizer, or a small maintained derivative-free crate after
   dependency review?

## References

- [docs/designs/discrete-sound-framework.md](discrete-sound-framework.md) —
  primitives, timing model, performance budget, reference policy
- [docs/designs/audio-output-path.md](audio-output-path.md) — host rate and
  resampling behavior
- [docs/debugging-asteroids-discrete-sound.md](../debugging-asteroids-discrete-sound.md)
  — the case study this generalizes
- [tools/sound-reference/README.md](../../tools/sound-reference/README.md) — the
  manual rig being replaced
- `core/src/device/discrete/` — circuit runtime and primitives
- `machines/src/dkong_sound.rs`, `machines/src/tkg04.rs` — first target
- `machines/src/asteroids_sound.rs`, `machines/src/llander_sound.rs`,
  `machines/src/congo_sound.rs`, `core/src/device/galaxian_sound.rs` — later
  targets
- `phosphor-emulator-audiodiff-76wx` — absorbed here (Phases 1, 2, 4)
- `phosphor-emulator-audio-dc-offset-g7p4` — the bug Phase 2 would have caught
- `phosphor-emulator-asteroids-sound-postprocessing-hm4` — the wavwrite tap-point
  question (Phase 0, answered: pre-effects) and Asteroids' remaining thrust
  rumble deficit
- `phosphor-emulator-uxi9`, `phosphor-emulator-pd5e` — Galaga / Xevious 54XX
  explosion sound (Phase 8)
- `phosphor-emulator-clock-tree-jv78.9` — the analogous "make live values
  introspectable" argument for clock domains
