# Design: Input Movies — Recording and Replaying Gameplay

> **Status: proposed.** A deterministic input-movie format that records a
> cabinet's exact input trace and replays it headlessly with a byte-identical
> framebuffer, plus the `frames.toml` integration that makes it pin *gameplay*
> rather than attract mode.
>
> Split out of an earlier `deterministic-timeline` draft that bundled this with
> a frontend rewind. The two share no types, no traits and no crates, so they
> are separate epics; see [`rewind.md`](rewind.md) for the other half and for
> the one interaction between them.

## Context

The workspace owns three time axes and is missing a fourth.

* **Video time** — `harness/tests/golden/frames.toml` pins a SHA-256 of each
  machine's oriented RGB frame (plus the vector display list for vector games)
  at a fixed frame count. `disasm frameshot` / `imgdiff` make a regression
  discussable as a picture.
* **CPU/bus time** — per-cycle CPU state machines, a concrete `Bus`, the
  `debug_trace.rs` event ring and `watchpoint.rs` conditions, surfaced headless
  by `disasm trace`.
* **`Harness` time** — `harness/src/harness.rs:Harness` boots a registered
  machine and steps frames, applying `PressSpec`/`MotionSpec` scripted input.
* **Input time** — the sequence of human actions that produced those frames.
  Missing.

### The gap, stated accurately

It would be wrong to say determinism is unenforced. `golden_frame_test` pins an
exact frame hash for all 40 registered machines at fixed frame counts, and
`save_state_rom_test` proves state-replay equivalence under a deliberate
divergence. Run-to-run determinism from reset **is** enforced, and enforced well.

The real gap is about *coverage*:

**Every golden entry is input-free.** Forty `[[frame]]` entries; three load an
NVRAM fixture; **zero** script any input. `press` appears in the file only as a
header comment describing a capability nothing uses. So the suite pins attract
loops, title screens and demo playback — never a frame produced by a human
playing the game. Sprites in motion under player control, collision, scoring,
input-driven sound triggers and every code path behind a coin are unguarded.

The second gap is that a trace which *can* be scripted today is limited to
digital pulses. `PressSpec`/`MotionSpec` cannot express a trackball or spinner
trace at all, which is exactly the input the ten analog machines need —
ccastles, foodf, gridlee, irobot, marble, missile_command, quantum, roadrunner,
starwars, tempest.

## Goals

1. Record every input that reaches a machine, and replay it headlessly with a
   byte-identical framebuffer.
2. Make a replay cheap to share and commit: one small file plus the already
   required ROM set.
3. **Pin gameplay frames in `frames.toml`**, so the frame regression suite
   guards machines under player control rather than only in attract mode.
4. Preserve every existing contract: golden hashes, save-state round trips, and
   the concrete-bus performance profile stay unchanged.
5. Keep the storage layer SDL-free, so `phosphor-harness` and `phosphor-disasm`
   decode a movie without the frontend.

## Non-goals

* Netplay or rollback.
* Branching / editable timelines.
* Video or audio encoding — a movie stores inputs; `frameshot --audio_out`
  remains the way to materialise a WAV.
* Changing any chip's synthesis or bus timing to *achieve* determinism. This
  doc exposes where determinism fails; it does not repair hardware models.

## The recording seam

Every input a machine receives arrives through `InputConfigurable`
(`core/src/core/machine.rs:395`), which is three methods:

```rust
fn input_controls(&self) -> &'static [InputControl];
fn handle_input(&mut self, event: InputEvent);
fn release_all_inputs(&mut self);          // overridable
```

The frontend touches it from exactly two generic functions —
`input::dispatch` (`frontend/src/input.rs:569`) and `input::resync` (`:711`) —
both taking `machine: &mut M where M: InputConfigurable + ?Sized`.

So the recorder is a **tee wrapper**, not a subscriber:

```rust
// frontend/src/movie.rs
struct Recording<'a, M: ?Sized> {
    inner: &'a mut M,
    sink: &'a mut MovieRecorder,
}

impl<M: InputConfigurable + ?Sized> InputConfigurable for Recording<'_, M> {
    fn input_controls(&self) -> &'static [InputControl] { self.inner.input_controls() }

    fn handle_input(&mut self, e: InputEvent) {
        self.sink.push_event(e);
        self.inner.handle_input(e);
    }

    fn release_all_inputs(&mut self) {
        self.sink.push_release_all();
        self.inner.release_all_inputs();
    }
}
```

When armed, the frontend passes `&mut Recording { .. }` where it passes
`machine` today. **`dispatch` and `resync` are not modified**, and no future
input path can bypass the recorder without also bypassing the trait.

`release_all_inputs` is recorded as its own record rather than decomposed into
per-control button releases. Machines holding conditioned analog state
(ccastles, marble, missile_command) override it to clear trackball accumulators
that the default loop does not touch; decomposing would silently drop that.

*Alternative considered:* subscribe to `BindingSet` resolution. Rejected — it
misses `resync`, which emits real `InputEvent`s after a reset or state load with
no corresponding SDL event, and it would not cover `release_all_inputs` at all.

## What is recorded

`(frame, InputEvent)` in delivery order. Nothing aggregated.

`RelativeCounter::add_delta` (`core/src/core/input.rs:78`) is:

```rust
pub fn add_delta(&mut self, delta: f32) { self.pending += delta as i32; }
```

The truncation happens on every call. Two 0.6-deltas contribute 0; one summed
1.2-delta contributes 1. On Tempest and Quantum, `DrainPolicy::ClampDrop`
discards the remainder rather than carrying it, so the error does not average
out over subsequent frames — it is a permanent divergence.

**How much this currently bites — measured, not assumed.** With the default
binding sensitivity, it does not. SDL's `xrel` is an integer and `DEFAULT_SCALE`
is 1.0, so mouse deltas arrive whole and `trunc(a) + trunc(b) == trunc(a + b)`
holds trivially. Decoding a four-minute Marble Madness capture: all 35,178
analog records were whole numbers, 127 distinct values, and **zero** of its
15,954 multi-delta frames would have summed differently. The divergence becomes
real only when a sensitivity other than 1.0 is set (`BindingSet::set_scale`,
persisted per machine in `state.toml`) or a sub-unit input device appears.

So per-event is the *conservative* choice rather than a currently active fix. It
is still the right one — it costs little (the record block deflates ~4.6× on a
real analog trace, 82.8 KB for four minutes) and it stays correct under a
sensitivity change instead of becoming silently wrong — but the earlier framing
of this section overstated the case, and the honest version is above.

Float values are stored as `f32::to_bits`, so the replayed value is bit-identical
and truncates identically.

**Zero relative deltas are not recorded.** Both `RelativeCounter` and
`AnalogAxis` apply one as `pending += 0`, so it is a provable no-op, and the
frontend emits an X *and* a Y event for every mouse motion — meaning any
straight-line movement records a zero on the other axis. That was 4,173 of
35,178 records (12%) on the capture above. A zero `Absolute` is kept: it is a
*position*, a centred stick, and dropping it would lose a real state change.

### Frame indexing

`frame` is the number of completed frames at delivery time. On replay, every
record with `frame == N` is delivered, in order, before `run_frame()` for frame
N.

This is exact, not an approximation. The SDL event pump and `run_frame` are
sequential in the frontend loop, and `resync` runs only after reset, state load
or focus change. Nothing calls `handle_input` mid-frame, so there is no
sub-frame case to quantise and no need for a reserved sub-frame cycle field.

## File format

Single binary file, `*.phmi` ("PHosphor Movie Input"), little-endian
throughout, matching `StateWriter`.

```text
magic b"PHMI" | version:u16 | header_len:u32 | header
records_len:u32 | deflate(records)
trailer: sha256(all preceding bytes), 32 bytes
```

```rust
struct MovieHeader {
    machine: String,          // registry name, e.g. "marble"
    rom_digest: [u8; 32],     // sha256 over the loaded set's members, name-sorted
    controls: Vec<String>,    // stable_names; records index into this
    dip: Vec<u8>,             // power-on dip_bank_value() in bank order
    nvram: Option<Vec<u8>>,   // inline CMOS fixture, if the run used one
    host_sample_rate: u32,
    frames: u32,
}

enum MovieRecord {
    Button     { frame: u32, ctl: u16, pressed: bool },
    Absolute   { frame: u32, ctl: u16, bits: u32 },
    Relative   { frame: u32, ctl: u16, bits: u32 },
    ReleaseAll { frame: u32 },
    Dip        { frame: u32, bank: u8, value: u8 },
    Marker     { frame: u32, label: String },
}
```

Four header fields deserve their rationale:

* **`rom_digest`** is the check that matters most. A movie recorded against one
  dump replayed against another must fail loudly rather than desync into a
  plausible-but-wrong frame hash. It hashes the **member files** of the loaded
  set: each name and body length-prefixed, walked in sorted name order because
  a `RomSet` is a `HashMap`. It does *not* hash the registry's `rom_names`:
  those name the archive to open, not the dump inside it. Version 1 of this
  format hashed the name list by mistake, which both missed a wrong dump and
  rejected movies that had merely outlived a reordering of the list; version 2
  exists to draw the line between the two meanings, and version-1 files are
  rejected rather than reinterpreted.
* **`controls` is an index table.** Records carry a `u16` index, not an
  `InputId` and not a per-record string. Indexing by stable name means a movie
  survives `InputId` renumbering; interning it means a 36k-event trackball
  movie does not carry 36k copies of `"track_x"`.
* **`dip`** is pinned because coinage, lives and difficulty change gameplay. A
  movie that does not pin its DIP bytes is not reproducible. Mid-session changes
  are `Dip` records.
* **`host_sample_rate`** is recorded and, on replay, **set** via
  `set_host_sample_rate` before the machine is constructed — not compared and
  rejected. Rejecting on mismatch would make a committed movie fail on any host
  whose card opens at a different rate, defeating goal 2. Headless replay has no
  real device, so setting it is free, and the ordering rule
  ([`audio-output-path.md`](audio-output-path.md) §3: set the rate *before*
  building the machine) is already the one the frontend obeys.

Note what is **absent**: `SAVE_VERSION`. A movie contains no save state, so it
has no reason to couple to the save format. That decoupling is what lets a
committed movie survive [`tlv-save-state.md`](tlv-save-state.md).

*Alternative considered:* a text `frames.toml`-style table. A two-minute
trackball trace is tens of thousands of records; text bloats and invites
truncation. `disasm movie info` is the human view.

## Replay

```rust
// harness/src/harness.rs, additive
impl Harness {
    pub fn build_with_movie(machine: &str, path: &str, movie: &Path) -> Result<Self, String>;
}
```

`build_with_movie` reads the header, verifies `machine` and `rom_digest`, calls
`set_host_sample_rate`, builds and resets the machine, loads inline NVRAM,
applies `dip`, resolves `controls` to `InputId`s through the machine's own
control table, and seeks a cursor to frame 0. `run_frame()` drains records while
`records[cursor].frame == self.frame`, delivering in order, then steps the
machine.

The existing `PressSpec`/`MotionSpec` path is untouched and stays as CLI sugar.

## CLI

```text
disasm replay --movie <m.phmi> [--frames N] <roms>   # replay; optional frameshot
disasm movie info  <m.phmi>                          # header + per-frame counts + markers
disasm movie check <m.phmi> <roms>                   # replay + hash, CI without a PNG
```

There is deliberately **no** `disasm record`: headless recording has no human to
record, and the scripted-input path already covers what a headless author would
want to express.

## Golden-frame integration — the payoff

`frames.toml` gains one optional field:

```toml
[[frame]]
machine = "marble"
movie   = "movies/marble-level1.phmi"
frames  = 4200
shows   = "Level 1, ball mid-way up the second ramp, enemy marble closing"
size    = [512, 384]
frame   = "sha256:…"
```

Movies live in `harness/tests/golden/movies/`, beside the existing `nvram/`
fixtures. An entry with a `movie` replays it instead of running input-free.
Every existing guard applies unchanged — registry coverage, the
no-uniform-frame check, and the ROM-less PNG/hash cross-check.

Two honest notes:

* A gameplay pin is **no more fragile** than an attract pin — both are exact
  functions of the ROM set and the frame count — but it will **fail more often**,
  because it covers more machine state. That is the point of doing it, and it
  raises the value of `shows` and the committed reference PNG, which carry the
  human judgement that the frame was right when pinned.
* Not every machine needs one. Prefer gameplay pins where attract mode guards
  least: the analog machines first (marble, tempest, missile_command, ccastles),
  then machines whose attract loop is a static title screen.

## Size

Digital input is negligible. Mouse motion is the cost: Marble Madness at roughly
ten `MouseMotion` events per frame is ~36k records per minute, on the order of
250–300 KB uncompressed. Two mitigations, both cheap:

* **Keep committed clips short.** The boot phase records *zero* records, so
  seeking past a self-test costs nothing in file size — a 3000-frame boot plus
  20 s of play is a 20 s file.
* **Deflate the record block.** `flate2` is already in the workspace.

## Recording flow in the frontend

A movie must start from a known state, so **`F2` is reset + arm recording**, not
"start capturing from wherever we are." Always valid, and the resulting artifact
depends only on the ROM set.

The friction is playing through the self-test on every take — Galaga is ~3000
frames, roughly 50 seconds. The unthrottled mode the clock loop already knows
about (`frontend/src/emulator.rs:966`) covers this if it is reachable from a
binding; **confirm a fast-forward action exists before Phase 3**, because
without it the capture workflow is unpleasant enough that nobody will use it.

*Alternative considered:* prepend a `save_state` blob so recording can start
mid-session. Rejected for v1 — it re-couples the movie to `SAVE_VERSION`,
inflates a ~1 KB artifact to tens of KB, and every save-format change would
invalidate the committed corpus.

## Interaction with rewind

[`rewind.md`](rewind.md) is a separate epic and neither blocks the other. The
only overlap is that a rewind mid-take would break a movie's "flat forward trace
from reset" invariant. **v1 rule: rewind is disabled while a recording is
armed.** That keeps both epics independent — whichever lands second adds the
one-line guard. Truncating an in-progress movie on rewind is the obvious later
refinement, once both exist.

## Testing

* `harness/tests/movie_test.rs` — registry-driven over `create_bare`, plus
  ROM-gated cases:
  * record a synthetic event stream, replay twice, assert identical
    `save_state()` **and** `render_frame()`;
  * record-then-replay equivalence against a live run;
  * negatives: `rom_digest` mismatch, machine mismatch, unknown control name,
    a record past `frames`.
* One committed gameplay movie and its `frames.toml` entry, which closes the
  loop between the two harnesses.
* **No golden-hash change.** A movie-less run is today's path verbatim.

## Phasing

1. **`harness/src/movie.rs`** — format, `MovieRecorder`, `MoviePlayer`,
   `Harness::build_with_movie`, `disasm replay` / `movie info` / `movie check`.
   No frontend, no ROMs needed to test.
2. **Determinism gate** — `movie_test.rs` as above.
3. **Frontend capture** — the `Recording` wrapper, `HostAction::MovieRecord`,
   write-on-stop via `*.tmp` + rename. Gated on a fast-forward binding existing.
4. **Golden gameplay pins** — the `movie` field in `frames.toml`, then capture
   for the analog machines first.

Phases 1–2 are worth landing alone: they give `disasm replay` and a determinism
gate before anything can record.

## Risks and open questions

* **Audio drain and save-state equivalence.** Headless `Harness` never calls
  `fill_audio`, so `SampleRing` accumulates and eventually drops-oldest with an
  `overruns()` counter. If any of that reaches a machine's `Saveable`, the
  replay-twice test passes in-process but a movie replayed under different drain
  conditions will not match on the save-state half of the fingerprint. The frame
  hash is unaffected either way. **Check this in Phase 2**; discovering it in
  Phase 4 is expensive.
* **Which machines get gameplay pins.** Not all 40. Proposal: the ten analog
  machines plus any machine whose attract loop is a static screen. Each pin is a
  judgement call recorded in `shows`.
* **`set_debug_entropy`.** `MachineDebug::set_debug_entropy`
  (`core/src/core/machine.rs:482`) installs a recorded entropy sequence for
  lockstep comparison against a reference emulator. It is never used by normal
  emulation and the default ignores it, so it is not a replay hazard today — but
  a movie replayed against a machine with entropy installed would diverge.
  Document it as unsupported rather than trying to capture it.

## References

* Recording seam: `core/src/core/machine.rs:395` (`InputConfigurable`),
  `frontend/src/input.rs:569` (`dispatch`), `:711` (`resync`).
* Analog truncation: `core/src/core/input.rs:78` (`add_delta`), `:121-131`
  (`DrainPolicy::ClampCarry` / `ClampDrop`).
* Replay host: `harness/src/harness.rs:93` (`Harness::build`).
* Golden suite: `harness/tests/golden_frame_test.rs`,
  `harness/tests/golden/frames.toml` (40 entries, 3 `nvram`, 0 scripted input).
* Rate-negotiation ordering: [`audio-output-path.md`](audio-output-path.md) §3.
* Save-format decoupling: [`tlv-save-state.md`](tlv-save-state.md).
* Companion epic: [`rewind.md`](rewind.md).
