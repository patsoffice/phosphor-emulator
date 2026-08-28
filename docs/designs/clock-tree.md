# Design: Unified Clock Tree

> **Status: proposed (rev 3 — corrects rev 2).** Gives every board one declared
> crystal set and derives its sub-clocks from it, replacing scattered
> `ClockDivider` fields and hand-rolled Bresenham accumulators. Rev 3 drops rev
> 2's "deterministic frame scheduler" — the master-rate frame loop it proposed
> was a 8–20× iteration-count regression on half the registered boards, and the
> frontend half of it has already shipped elsewhere. Rev 3 also corrects rev 2's
> crystal table, which was wrong on its two flagship boards, and replaces its
> invented two-crystal examples with the six real ones.

## Context

Every arcade board is a clock tree: a crystal is divided down to the CPU, the
video dot clock, the sound CPU, and the chip clocks hanging off them. Scanline
timing and sound-chip rates are consequences of the same division.

The codebase already knows this. Every board's timing block documents its
crystal in a comment:

```rust
// machines/src/ccastles.rs:338          // machines/src/tkg04.rs:45
// Master clock: 10 MHz XTAL             // Master clock:  61.44 MHz
// CPU clock: 10 MHz / 8 = 1.25 MHz      // CPU clock:     61.44 / 5 / 4 = 3.072 MHz
// Pixel clock: 10 MHz / 2 = 5 MHz       // Pixel clock:   61.44 / 10 = 6.144 MHz
```

The crystal is documented in prose in 20-odd files and **stored in a type in
none of them**. What the type stores is the leaf:

```rust
// core/src/core/machine.rs:47
pub struct TimingConfig {
    pub cpu_clock_hz: u64,
    pub cycles_per_scanline: u64,
    pub total_scanlines: u64,
    pub display_width: u32,
    pub display_height: u32,
    pub display_aspect: Option<(u32, u32)>,
}
```

Sub-clocks are then re-derived per board, each against `TIMING.cpu_clock_hz`,
in three different idioms:

```rust
// 1. ClockDivider — 14 fields across 11 boards
ClockDivider::new(179, 1000)                                   // gottlieb.rs:485   sound 6502
ClockDivider::new(SOUND_CLOCK / 16, TIMING.cpu_clock_hz as u32) // docastle.rs:896  SN76489

// 2. Hand-rolled Bresenham — 5 more sites, one of which must fire twice per cycle
board.sound_cycle_accum += SOUND_CLOCK;                        // congo_bongo.rs:395
while board.sound_cycle_accum >= TIMING.cpu_clock_hz { ... }   // 4 MHz sound Z80 vs 3.041 MHz main
self.clock_acc += self.tms_clock_hz as u64;                    // atari_system1_sound.rs:90
while self.clock_acc >= step { ... }

// 3. Rounded integer constants standing in for a ratio between two crystals
cycles_per_scanline: 254,   // docastle.rs:98  — true value 253.97
cycles_per_scanline: 261,   // mrdo.rs:71      — true value 261.06, comment says so
```

`ClockDivider` itself (`core/src/core/clock.rs:23`) is fine and stays: a
Bresenham `numerator/denominator` with a `phase_accum: u32` saved and the ratio
`#[save_skip]`ped, and a `set_ratio` (`:61`) that folds `phase_accum %=
denominator` so a retune can't stall.

### The real crystal table

Rev 2's table was wrong on three of its four rows. Corrected, and extended to
the cases that actually motivate the design:

| Board | CPU | Video | Sound | Notes |
|---|---|---|---|---|
| Atari System 1 | 14.31818 / 2 = 7.159090 MHz | = CPU (pixel clock) | 14.318181 / 8 = 1.789772 MHz 6502 **and** POKEY; TMS5220 off 14.318181 / 2 | **Single crystal.** Rev 2's "3.579545 MHz POKEY on a second tree" is contradicted by `atari_system1_sound.rs:35-38,45` |
| Williams | 12 / 3 / 4 = 1 MHz E | 12 × 2/3 = 8 MHz dot, 512 dots a line | — | Single crystal. Derived from the R-8731 sheet in `../schematics/williams-video-clock.md`; the 64 cycles a scanline used to be a measured 15.6 kHz |
| Atari DVG | 12.096 / 8 = 1.512 MHz | vector | — | Single crystal; frame budget is a chosen 60 Hz, not derived (`atari_dvg.rs:33-42`) |
| Crystal Castles / Missile Command | 10 / 8 = 1.25 MHz | 10 / 2 = 5 MHz | — | Single crystal, 8:1 CPU:master |
| Gottlieb System 80 | 15 / 3 = 5 MHz I8088 (`gottlieb.rs:8`) | 20 / 4 = 5 MHz pixel (`gottlieb.rs:51`) | 3.579545 / 4 = 894886 Hz 6502; Votrax VCO | **Two crystals that both land on 5 MHz** — rev 2 read one comment and called the other stale. See open questions |
| Do! Castle | 4 MHz | **9.828 / 2 = 4.914 MHz** | SN76489 @ 4 MHz | Video on its own crystal; `cycles_per_scanline` rounds 253.97 → 254 |
| Mr. Do! | 8.2 / 2 = 4.1 MHz | **19.6 / 4 = 4.9 MHz** | SN76489 @ 4.1 MHz | Same; rounds 261.06 → 261, and `mrdo.rs:66` says so out loud |
| Mario Bros. | 8 / 2 = 4 MHz | **24 / 4 = 6 MHz** | I8039 @ **11 MHz** | Three crystals; 384 px / 6 MHz × 4 MHz = 256 exactly |
| Congo Bongo | 48.66 / 16 = 3.04125 MHz | 48.66 / 8 | **4 MHz** sound Z80 | Sound CPU is *faster* than the main loop |
| Scramble | 18.432 / 6 = 3.072 MHz | | **14.318 / 8** ≈ 1.79 MHz | Two crystals |

Two conclusions rev 2 missed, and they are the whole case for this design:

* The boards that genuinely need more than one crystal are **docastle, mrdo,
  mario_bros, congo_bongo, scramble, tkg04, and gottlieb** — not System 1.
* On docastle, mrdo and mario_bros the **video clock is on a different crystal
  than the CPU**, so `cycles_per_scanline` is not a hardware constant at all.
  It is a rounded conversion between two crystals, computed by hand, in a
  comment, differently in each file. That is the error-prone thing worth
  centralising.

## Problems

### The crystal is documented, not stored

`TIMING.cpu_clock_hz` is a leaf value. Because the crystal isn't stored, nothing
can check that `cpu_clock_hz × HTOTAL / pixel_hz` really is the declared
`cycles_per_scanline`, and nothing can check that a board's sound divider really
is its sound chip's rate over its CPU's rate. Three boards round that conversion
by hand; a fourth could get it wrong tomorrow and no test would notice.

### Sub-clock ratios are hand-reduced against the wrong reference

Every divider is expressed against `TIMING.cpu_clock_hz` rather than against the
crystal the device actually hangs off. `179/1000` (Gottlieb sound) is a rounding
of `894886/5000000`; `25/192` (I8035 at 400 kHz from 3.072 MHz) is exact but
hand-computed; `SOUND_CLOCK / 16, TIMING.cpu_clock_hz` (docastle) mixes a chip
divider into a clock ratio. Each is separately plausible and separately
unverifiable.

The Votrax bug (bead `phosphor-emulator-1fg`, Gottlieb shipped a hardcoded
720 kHz with no DAC retune; now `convert_speech_clock` at `gottlieb.rs:107`)
is the failure mode: a rate that was neither derived from a declared crystal nor
atomically paired with the device's own `set_clock`.

### Retune is a per-board hand-rolled protocol

Three devices retune a clock at runtime, and each does it differently:

* **Votrax VCO** (`gottlieb.rs:682-693`) — a `votrax_clock_applied` shadow field
  compared each cycle, then `set_ratio` + `set_votrax_clock` together. The shadow
  field exists only because `ClockDivider` `#[save_skip]`s its ratio, so a state
  load has to re-derive it; the comment at `:684` says exactly that.
* **TMS5220 clock select** (`atari_system1_sound.rs:128-133`) — Port B bit 4
  picks a divisor, retuning `tms_clock_hz` and the device, against a separate
  `clock_acc` accumulator. Rev 2 didn't know this one existed.
* **Starwars TMS** — `starwars.rs:701 tms_clock_acc`, a third variant.

### Ad-hoc accumulators duplicate `ClockDivider` badly

`congo_bongo.rs:494`, `atari_system1_sound.rs:68`, `starwars.rs:701`,
`scramble.rs:179`, `device/discrete/mod.rs:810`. The congo_bongo one is not
gratuitous — its sound Z80 at 4 MHz against a 3.041 MHz main loop genuinely
fires more than once per main cycle, which `ClockDivider::tick() -> bool` cannot
express. Any replacement has to handle that case.

### Live ratios aren't introspectable

`ClockDivider` has good unit tests (`clock.rs:91`), but the set of live clocks on
a running board isn't visible to `debug_registers()` or the profiler. Diagnosing
"is the sound chip running at the right rate?" means reading the constructor.

## Non-problems (dropped from rev 2)

Rev 2 listed these; they are either already solved or were misreadings.

* **"The frontend throttle is naive and drifts."** Shipped.
  [`audio-output-path.md`](audio-output-path.md) is *implemented*, and
  `emulator.rs:1272-1288` now paces each frame by
  `frame_pace_trim(ring.len(), ring.capacity())` against the audio ring's fill
  level, accumulating `next_frame_time += paced`. That is a closed clock loop
  against the sound card, which is strictly better than pacing off a
  crystal-derived nominal — the host's audio clock is the one that matters and
  it isn't the crystal. Rev 2's Phase 5 is deleted.
* **"`AudioSource::audio_sample_rate()` can diverge from the resampler's input
  rate."** These are different quantities by design.
  `audio_sample_rate()` (`core/src/core/machine.rs:155`) reports the rate
  `fill_audio` *emits* — machines return `host_sample_rate()`
  (`gridlee.rs:937`, `gottlieb.rs:69`). Rev 2's proposal to redefine it as a
  domain's Hz would report the chip rate to the frontend and break the
  negotiated-output-rate contract. Deleted.
* **"Every board re-implements `for _ in 0..cycles_per_frame`."** Not true of
  the boards rev 2 cited. `atari_system1.rs:332` and `williams.rs:254` already
  hoist the frame-position test into a scanline-outer/cycle-inner
  `run_scanlines`, with `tick()` kept as the debugger's off-boundary path. That
  structure is a deliberate optimisation and rev 2's unified per-master-tick
  loop would have thrown it away.
* **"Marble and Road Runner duplicate `TIMING`."** They share it —
  `marble.rs:673,676` and `roadrunner.rs:741,744` both reference
  `atari_system1::TIMING`.

## Goals

1. Every board declares its crystals once, in a type, and derives its CPU,
   video, and sound-chip rates from them.
2. Derived rates are checkable: a test proves each board's declared `TIMING`
   agrees with its declared tree, and reports the rounding error where the CPU
   and video clocks are on different crystals.
3. One Bresenham model for sub-clocks, able to express a domain faster than the
   stepping domain (congo_bongo).
4. Runtime retune through one call that folds phase, saves the new ratio, and is
   paired with the device's `set_clock` — removing the `_applied` shadow fields.
5. Introspectable: `debug_registers()`/`overlay_stats()` can list every live
   domain with its ratio, Hz, and phase.

**Non-goals.** Crystal jitter. Changing `AudioResampler`'s FIR quality or the
audio output path. Changing the frame loop's iteration rate or structure.
Deleting `TimingConfig` or `pub const TIMING` (see Scope).

## Proposed Architecture

### The tree does bookkeeping, not scheduling

This is the central correction to rev 2. Rev 2 made master ticks the loop unit:
`for _ in 0..ticks_per_frame { let mask = tree.tick(); let ctx =
FrameCtx::from_master(...); }`. The CPU:master ratios in the table above make
that 8× the loop iterations on ccastles, missile_command and atari_dvg, 16× on
gridlee and congo_bongo, and 20× on tkg04 — each iteration additionally carrying
a `u64` divide and modulo that today doesn't exist on the hot path at all. On
Crystal Castles that is 163,840 iterations per frame becoming 1,310,720.
Design priority 3 rules it out, and `phosphor-bench` would have caught it.

Rev 3 keeps the loop exactly where it is. The tree stores the crystal and the
derivation; a domain still steps once per **CPU cycle**, with the same single
add-and-compare `ClockDivider` does today. Ratios to the crystal are retained
for `hz()`, for validation, and for the debugger — not for stepping.

```rust
// core/src/core/clock_tree.rs

/// Stable handle into a tree's domain table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DomainId(u8);

/// Which crystal a domain hangs off. Most boards have one; docastle, mrdo,
/// mario_bros, congo_bongo, scramble, tkg04 and gottlieb have two or three.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RootId(u8);

#[derive(Saveable)]
#[save_version(1)]
pub struct ClockDomain {
    #[save_skip] name: ClockDomainName,  // fieldless enum tag, not &'static str
    #[save_skip] root: RootId,
    /// Ratio to `root`'s crystal — the auditable hardware statement.
    #[save_skip] root_num: u32,
    #[save_skip] root_den: u32,
    /// Ratio to the *stepping* domain, precomputed from the above. This is what
    /// `advance` uses, so a step costs exactly what `ClockDivider::tick` costs.
    /// Saved (not skipped) so a retuned domain reloads at its retuned rate.
    step_num: u32,
    step_den: u32,
    phase_accum: u32,
}

impl ClockDomain {
    /// Advance one step-domain cycle; returns how many times this domain fired.
    /// Normally 0 or 1; >1 when the domain outruns the stepping domain, as
    /// congo_bongo's 4 MHz sound Z80 does against its 3.041 MHz main CPU.
    #[inline]
    pub fn advance(&mut self) -> u32 { /* Bresenham, with a while for step_num > step_den */ }

    /// Fast path for the ≤1 case, identical in cost to `ClockDivider::tick`.
    #[inline]
    pub fn tick(&mut self) -> bool { debug_assert!(self.step_num <= self.step_den); /* ... */ }

    pub fn hz(&self, root_hz: u32) -> u64 { root_hz as u64 * self.root_num as u64 / self.root_den as u64 }
}

#[derive(Saveable)]
#[save_version(1)]
pub struct ClockTree {
    #[save_skip] roots: [u32; MAX_ROOTS],   // crystals in Hz, e.g. [4_000_000, 9_828_000]
    #[save_skip] root_len: u8,
    domains: [ClockDomain; MAX_DOMAINS],    // fixed array — see Save/Load
    #[save_skip] len: u8,
    #[save_skip] step: DomainId,            // the domain the frame loop counts in (the CPU)
}

impl ClockTree {
    pub fn new(crystal_hz: u32) -> Self;
    pub fn add_root(&mut self, crystal_hz: u32) -> RootId;

    /// Declare a domain by its exact ratio to a crystal. This is the hardware
    /// statement: `add_domain(Cpu, root, 1, 8)` reads as "CPU is crystal / 8".
    pub fn add_domain(&mut self, name: ClockDomainName, root: RootId, num: u32, den: u32) -> DomainId;

    /// Nominate the stepping domain. Must be called after all domains are added;
    /// precomputes every domain's step ratio (exact — both sides are rational
    /// multiples of integer-Hz crystals, so this reduces by gcd without loss).
    pub fn set_step_domain(&mut self, id: DomainId);

    #[inline] pub fn advance(&mut self, id: DomainId) -> u32;
    #[inline] pub fn tick(&mut self, id: DomainId) -> bool;

    pub fn hz(&self, id: DomainId) -> u64;

    /// Retune at runtime (Votrax VCO, TMS clock select). Recomputes both ratios
    /// and folds `phase_accum %= step_den`.
    pub fn set_domain_hz(&mut self, id: DomainId, hz: u32);

    /// CPU cycles per scanline implied by a video domain and an HTOTAL, plus the
    /// rounding error in ppm. The one place the cross-crystal conversion lives.
    pub fn cycles_per_scanline(&self, video: DomainId, htotal: u32) -> (u64, i32);

    pub fn domains(&self) -> impl Iterator<Item = (&'static str, u64, u32, u32)>; // debug view
}
```

`ClockDivider` stays exactly as it is, for the handful of uses that aren't board
clocks. `ClockDomain` is its named, tree-owned sibling.

### What a board declares

```rust
// machines/src/docastle.rs — a genuine two-crystal board
pub fn clock_tree() -> ClockTree {
    let mut t = ClockTree::new(4_000_000);              // CPU crystal
    let vid = t.add_root(9_828_000);                    // video crystal
    let cpu = t.add_domain(Cpu,   RootId::MAIN, 1, 1);  // 4 MHz
    let dot = t.add_domain(Pixel, vid,          1, 2);  // 4.914 MHz
    let sn  = t.add_domain(Psg,   RootId::MAIN, 1, 16); // SN76489 / 16
    t.set_step_domain(cpu);
    t
}
// and the derivation that is a hand-rounded 254 today:
//   tree.cycles_per_scanline(dot, 312) == (254, -122 ppm)
```

The rounding error is now a *number in a test* rather than a figure buried in a
comment. Mr. Do!'s equivalent is +235 ppm, and its comment already admits the
count "is not a clean integer".

> **Implemented (steps 1 and 2).** The real figures are -125 ppm for Do! Castle
> and +235 ppm for Mr. Do!, not the -122 and +230 written above: 312 dot clocks
> is 253.96825 CPU cycles, and 254 against that is 125 ppm. The earlier numbers
> came from rounding the intermediate to three decimals before dividing.
> `cycles_per_scanline` computes it in exact integer arithmetic; the sign
> convention here is retained, so the ppm is the error in the *video rate* the
> integer count implies (negative means the count runs the video clock slow).

### Frame position

Rev 2's `FrameCtx`-per-master-tick is dropped. `FrameParams { cycles_per_scanline,
total_scanlines, vblank_line }` is still worth having as a shared type, because
five boards re-derive the same two expressions:

```rust
let frame_cycle = self.clock % TIMING.cycles_per_frame();
let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
```

— `atari_system1.rs:322,340`, `williams.rs:240,262`, `ccastles.rs:592,913`,
`irobot.rs`, `missile_command.rs`. Give them one `params.position(clock)`
helper returning `(scanline, is_line_start)` and leave the loop structure alone.
The scanline-hoisted boards keep calling it once per scanline; the plain-loop
boards keep calling it once per cycle, exactly as now.

### Audio

Unchanged. Boards keep `AudioResampler::new(chip_hz, output_sample_rate())` and
keep pushing one sample per sound-clock tick; the resampler's box-filter + FIR
and the ring path from [`audio-output-path.md`](audio-output-path.md) are not
touched. The only change is that `chip_hz` becomes `tree.hz(sound)` instead of a
per-board `const SOUND_CLOCK_HZ`, so the resampler's input rate and the
divider's ratio are provably the same derivation.

For a retuned device that feeds audio, `set_domain_hz` is paired with
`AudioResampler::set_input_rate` (`core/src/audio/mod.rs:300`), which preserves
buffered output and the FIR delay line — never a phase reset.

### Retune

```rust
fn write_speech_dac(&mut self, data: u8) {
    let hz = convert_speech_clock(data);          // gottlieb.rs:107
    self.tree.set_domain_hz(self.votrax, hz as u32);
    self.sound.set_votrax_clock(hz);
}
```

One call site, no `_applied` shadow field, no per-cycle comparison in
`end_cycle`, and no re-derive-after-load hack — because `ClockDomain` saves its
step ratio rather than `#[save_skip]`ping it. The same shape covers the TMS5220
clock select at `atari_system1_sound.rs:128-133` and `starwars.rs:701`.

### Save/Load

`ClockDomain` saves `step_num`, `step_den`, `phase_accum`; everything else is
`#[save_skip]` and re-provided by `clock_tree()`. A board that replaces N
`ClockDivider` fields with an N-domain tree, declared in the same order, writes
the same `phase_accum` sequence plus two `u32`s per domain — so the layout shift
is small, local, and per-board.

Two constraints from the current format:

* The save format is positional with exact version matching
  (`save_state.rs:43,262`), so any board whose field layout changes needs a
  `SAVE_VERSION` bump and invalidates existing `.state` files. Each migration
  commit should say so. If [`tlv-save-state.md`](tlv-save-state.md) lands first,
  this cost disappears; that is a reason to sequence it first but not a blocker.
* `#[derive(Saveable)]`'s array path (`macros/src/lib.rs:1042-1094`) delegates
  non-primitive elements to `Saveable::load_state`, and there is **no
  `impl Saveable for Option<T>`** in `core/src/core/save_state.rs`. Rev 2's
  `domains: [Option<ClockDomain>; 6]` would not compile. Use
  `[ClockDomain; MAX_DOMAINS]` with unused slots left inert (`step_num = 0`) and
  a `#[save_skip] len`. `MAX_DOMAINS = 8` is generous — the current maximum is
  two divider fields on any one board.

## Scope

Rev 2's Phase 6 proposed deleting `pub const TIMING`. Dropped. `TIMING` is
threaded through `machine_core_metadata!($id, $timing)` (`machines/src/lib.rs:225`)
and `impl_board_delegation!($type, $board, $timing)` (`:60`) across 34 files and
40 registered machines. Changing those macro signatures is a large mechanical
churn for no correctness gain — `TimingConfig` is a perfectly good *view*.

Instead: boards declare a tree *alongside* `TIMING`, and a registry-driven test
asserts they agree. That catches the entire bug class this design exists for,
for a fraction of the churn, and it works on a board that has declared a tree
before any of its dividers have been migrated.

Vector boards (`atari_dvg`, `atari_avg`, `starwars`, `quantum`) are out of scope
for the divider migration. `atari_dvg`'s 60 Hz is a chosen budget, not a crystal
derivation (`atari_dvg.rs:33-42`), and its only periodic event is a cycle-counted
250 Hz NMI. They can declare a tree for the validation test and keep their loops.

## Migration Plan

Each step is one commit, validated by `cargo test -p phosphor-machines` plus
`cargo test -p phosphor-harness --test golden_frame_test`.

1. **Add `ClockTree` with no callers.** `core/src/core/clock_tree.rs`:
   `ClockTree`, `ClockDomain`, `DomainId`, `RootId`, `ClockDomainName`,
   `FrameParams`. Unit tests: `1/8` fires 1 in 8; congo_bongo's `4_000_000` over
   `3_041_250` fires 4 times per 3 steps and never twice in a row wrongly;
   `set_domain_hz` folds phase and survives a save/load round trip;
   `cycles_per_scanline(dot, 312)` on a 4 MHz/9.828 MHz pair returns
   `(254, -122)`.
2. **Declare trees, migrate nothing.** Add `pub fn clock_tree()` to each board
   beside its `TIMING`, transcribing the crystal comment that is already there.
   Add the registry-driven consistency test: for every registered machine,
   `tree.hz(cpu) == TIMING.cpu_clock_hz`, and where a video domain is declared,
   `tree.cycles_per_scanline(video, htotal)` matches
   `TIMING.cycles_per_scanline` within a per-board tolerance the board states.
   **This step delivers most of the value.** No behaviour changes; the three
   hand-rounded scanline counts become asserted-with-known-error.
3. **Pilot: Gottlieb.** Replace `sound_clock` + `votrax_clock` + the
   `votrax_clock_applied` shadow field with tree domains and a single
   `set_domain_hz` at the DAC write. Two crystals declared (15 MHz CPU,
   20 MHz pixel). Verify Q\*Bert golden frames and the `convert_speech_clock`
   tests are unchanged. Keep `179/1000` as an explicit ratio in this commit.
4. **Exact Gottlieb sound ratio** — separate commit, because it *is* a
   behaviour change: `179/1000` becomes `447443/2500000` (0.013% faster sound
   CPU). Golden frames should be unaffected; the audio differs. Land it alone so
   a bisect can find it.
5. **Retune sites.** TMS5220 clock select (`atari_system1_sound.rs:128-133`) and
   `starwars.rs:701 tms_clock_acc` onto `set_domain_hz`, deleting their
   accumulators.
6. **Ad-hoc accumulators.** `congo_bongo.rs:494` (the `advance() -> u32`
   motivating case), `scramble.rs:179`. Each one commit.
7. **Remaining `ClockDivider` fields.** `atari_system1`, `tkg04`, `mcr2`,
   `mario_bros`, `docastle`, `mrdo`, `btime`, `congo_bongo`, `namco_galaga`.
   Mechanical once 2–6 have set the pattern; `gridlee.rs:357` is a resampling
   divider, not a board clock, and stays.
8. **Introspection.** `debug_registers()` and `overlay_stats()` list live
   domains via `ClockTree::domains()`.

Steps 1–2 are worth landing on their own even if 3–8 never happen.

**Steps 1 and 2 are implemented** (`0156409`, `f2df73f`). Two notes for whoever
picks up step 3. `MachineCore::clock_declaration()` is how a board's tree and
its `TimingConfig` reach a test together, emitted by `machine_core_metadata!`
from the same `$timing` expression so the pair cannot drift. And `hz()` reads a
domain's *step* ratio rather than its root ratio, which is what makes the
save-state split self-consistent: a domain restored at a retuned rate reports
the retuned rate, while `root_ratio()` stays the board's unchanged declaration.

## Alternatives Considered

### Keep `TimingConfig` + per-board `ClockDivider`

Lowest churn. But nothing checks the three hand-rounded cross-crystal scanline
counts, the Votrax class of bug stays possible, and the five hand-rolled
accumulators keep multiplying. Rejected — though note that step 2 alone captures
most of the benefit at nearly this cost.

### Master-rate frame loop driven by the tree (rev 2)

Rejected on measurement grounds: 8–20× the loop iterations on most boards, plus
a per-iteration `u64` divide, plus the loss of the existing scanline hoist in
`atari_system1.rs:332` and `williams.rs:254`. The tree's value is in the
derivation, not in owning the loop.

### One `ClockTree` per crystal

Rev 2's model. It forces a board with a video crystal and a CPU crystal to hold
two trees whose relationship — precisely the rounding this design exists to
audit — is expressible in neither. Multi-root in one tree keeps the conversion
inside the type that can check it.

### `DomainMask` bitmask of what fired this tick

Rejected: one bit per domain cannot express congo_bongo's sound Z80 firing twice
in a main cycle, and computing the mask for all domains at once costs more than
letting a board step the two or three domains it has.

## Open Questions

* ~~**Gottlieb's crystals.**~~ Answered: the Q\*Bert instruction manual's parts
  list carries two crystals on the main board, 15 MHz and 20 MHz, and a third
  at 3.579545 MHz on the sound board. Both `gottlieb.rs:8` and `gottlieb.rs:51`
  are correct and neither is stale; System 80 is a three-crystal board whose two
  main-board crystals divide to the same 5 MHz, which is why nobody noticed.
* ~~**Rounding tolerance policy.**~~ Settled as proposed, with one addition. The
  tolerance is declared on the tree itself (`ClockTree::set_raster(video,
  htotal, ppm)`) rather than in the test, so it sits beside the crystals it
  bounds. A second check asserts the declared bound is within a factor of two of
  the real error, because "actual is within declared" alone cannot fail when a
  divider is tightened. Do! Castle declares 130, Mr. Do! 240, and every board
  whose clocks divide evenly declares 0.
* ~~**Does anything need a domain faster than the CPU besides congo_bongo?**~~
  Checked: no. mario_bros' I8039 looks like a second case at 11 MHz against a
  4 MHz Z80, but `SOUND_TICK_NUM/DEN = 11/60` (`mario_bros.rs:103-104`) is the
  I8039's *machine-cycle* rate — 11 MHz / 15 = 733,333 Hz, and 733333/4000000
  reduces to exactly 11/60. It ticks at most once per main cycle. congo_bongo
  is the only `advance() -> u32` case among board clocks; the tree should still
  expose the count form, but `tick() -> bool` covers every other domain.

## References

* `core/src/core/clock.rs:23` (`ClockDivider`), `:61` (`set_ratio` phase fold).
* `core/src/core/machine.rs:47` (`TimingConfig`), `:155`
  (`AudioSource::audio_sample_rate` — output rate), `:458`
  (`MachineDebug::cycles_per_frame`).
* `core/src/audio/mod.rs:179` (`AudioResampler`), `:300` (`set_input_rate`).
* `macros/src/lib.rs:1042` (Saveable array element codegen),
  `core/src/core/save_state.rs:43` (`SAVE_VERSION`).
* `machines/src/lib.rs:60` (`impl_board_delegation!`), `:225`
  (`machine_core_metadata!`).
* Cross-crystal scanline rounding: `docastle.rs:91-98`, `mrdo.rs:65-71`,
  `mario_bros.rs:77-81`.
* Multi-fire sound clock: `congo_bongo.rs:394-397`.
* Runtime retune: `gottlieb.rs:107,682-693`, `atari_system1_sound.rs:51,128-133`,
  `starwars.rs:701`.
* Scanline-hoisted frame loops: `atari_system1.rs:332,351`,
  `williams.rs:254,276`.
* Already shipped: [`audio-output-path.md`](audio-output-path.md),
  `frontend/src/emulator.rs:1272-1288` (ring-fill frame pacing).
* Prior bug: bead `phosphor-emulator-1fg`.
