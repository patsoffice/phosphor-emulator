# Design: Concrete Bus Dispatch

> **Status: prototypes landed, rollout in progress.** Pac-Man (`66a4f84`…`498374b`)
> and the Galaga family (`3d8886b`) are converted and measured. The rollout is
> tracked as child issues of `concrete-bus-dispatch-blzz`. This document is
> written from the prototype results, not before them.

## Context

`Bus` is a generic trait — associated `Address` and `Data` types, and every CPU's
`execute_cycle` is generic over `B: Bus<…>`. None of that generality reaches the
machines. Every board owns its CPUs *and* its bus state in one struct, so
`cpu.execute_cycle(bus, …)` cannot borrow-check: the CPU is reached through the
same `&mut self` the bus needs. `bus_split!` works around it by reborrowing the
struct through a raw pointer and coercing to `&mut dyn Bus`:

```rust
bus_split!(self, bus => {
    for _ in 0..TIMING.cycles_per_frame() {
        self.board.tick(bus);          // bus: &mut dyn Bus<Address = u16, Data = u8>
    }
});
```

Two costs follow. The `unsafe` is the visible one. The one that shows up in a
profile is that every read, write, opcode fetch and interrupt poll in every
running machine is an indirect call the optimiser cannot see through — and, more
than the call itself, a barrier it cannot move board state across.

The fix is to stop asking for the split: put CPU state and bus state in
different fields, and the borrow checker does the rest, at a concrete type.

## What the prototypes measured

`phosphor-bench`, release, 400 frames × 5 reps, best of 8 alternating runs of a
baseline and a converted binary on an idle host:

| machine | CPUs         | dyn   | concrete | change |
|---------|--------------|-------|----------|--------|
| galaga  | 3× Z80       | 0.887 | 0.759    | −14.5% |
| xevious | 3× Z80       | 1.164 | 1.037    | −10.9% |
| digdug  | 3× Z80       | 2.091 | 1.956    | −6.5%  |
| pacman  | 1× Z80       | 0.748 | 0.706    | −5.6%  |

(ms/frame of emulation, excluding render and audio.)

The gain tracks how much of the frame is CPU cycles: Galaga steps three CPUs per
cycle and gains the most; Dig Dug spends much of its frame in the background
tilemap renderer, which this change does not touch, so the same board-level win
is diluted.

Codegen confirms the mechanism rather than inferring it. Disassembling the Z80
cycle instantiations in the release binary: the Pac-Man instantiation contains
**0 indirect calls**, against **48** in the `&mut dyn Bus` one. Bus dispatch
becomes a direct call to `bus_read_common`/`bus_write_common` — those are too
large to inline, so the *work* of an access is unchanged; what disappears is the
indirect branch and the optimisation barrier around it.

### The finding that decides the shape

Splitting the bus view *inside* the cycle loop — the obvious translation of the
existing per-cycle `tick_frame_boundary` — made Galaga **6% slower than the
trait object it replaced** (0.883 → 0.938 ms/frame). A bus view several pointers
wide, re-formed 50,688 times a frame, costs more than the indirect call it saves.
Hoisting the split out of the loop turned that into the −14.5% above.

**Split once per run, never per cycle.** This is the single most important rule
in the rollout: a conversion that gets it wrong is a regression, not a win, and
the golden frames will not catch it because the pixels are identical.

## Target shape

Two shapes, chosen by whether the game adds bus-visible state to the board.

### 1. The board *is* the bus (Pac-Man)

When a game adds nothing to the shared board's address decoding, the board
implements `Bus` and the machine holds the CPU beside it:

```rust
pub struct PacmanSystem {
    #[debug_cpu("Z80")] pub cpu: Z80,
    #[debug_bus]        pub board: NamcoPacBoard,   // impl Bus
}

fn run_frame(&mut self) {
    for _ in 0..TIMING.cycles_per_frame() {
        namco_pac::tick(&mut self.cpu, &mut self.board);   // disjoint fields
    }
}
```

This is the fastest shape — the bus is one pointer — and the one to prefer.
Game-specific decoding that is really *board* behaviour (Pac-Man's A15 mirror,
its IM2 vector latch) should move to the board rather than force shape 2.

### 2. A bus view (Ms. Pac-Man, Galaga, Dig Dug, Xevious)

When the game genuinely interposes state — a decode latch, a scroll latch, an
EAROM, banked ROMs — the bus is a view struct of disjoint field borrows, built
by a `split()` that the borrow checker verifies:

```rust
struct GalagaBus<'a> {
    board: &'a mut NamcoGalagaBoard,
    starfield_scroll_x: &'a mut u8,
    /* … */
}

fn split(&mut self) -> (&mut GalagaCpus, GalagaBus<'_>) { … }

fn run_frame(&mut self) {
    let (cpus, mut bus) = self.split();          // once per frame, not per cycle
    for _ in 0..cycles { namco_galaga::tick(cpus, &mut bus); }
}
```

Rendering state stays on the machine, outside the view, so a frame-boundary
render still needs `&mut self` — see "Frame-boundary renders" below.

### The per-family tick

The per-cycle sequence stays shared, as a free function generic over a
family-specific bus trait, so it monomorphises per game:

```rust
pub trait NamcoPacBus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut NamcoPacBoard;
}

#[inline]
pub fn tick<B: NamcoPacBus>(cpu: &mut Z80, bus: &mut B) {
    bus.board().begin_cycle(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}
```

The board's old `tick(&mut self, bus)` splits into `begin_cycle` (pre-CPU board
work, plus any CPU state it samples) and `end_cycle` (post-CPU device work and
the clock advance). Anything read *between* CPUs — Galaga's sub/sound reset latch,
which a mid-cycle write can change — must stay read between them, not hoisted
into a gate computed up front.

### Frame-boundary renders

Machines that render on the cycle that completes a frame kept a per-cycle
`tick_frame_boundary` so the debugger's single-step path also refreshes the
picture. That is exactly the per-cycle split to avoid. The replacement runs to
the boundary under one borrow:

```rust
let mut remaining = cycles;
while remaining > 0 {
    let run = (cycles - self.board.clock % cycles).min(remaining);
    { let (cpus, mut bus) = self.split(); for _ in 0..run { tick(cpus, &mut bus); } }
    remaining -= run;
    if self.board.clock.is_multiple_of(cycles) { self.render_video(); }
}
```

This renders on the same cycle as before even when the debugger has left the
clock off-phase, and still splits once per run.

## Supporting changes already made

- **`#[debug_bus]` on the `BusDebug` derive** (`057a13d`). Separating the CPU
  from the board splits what the debugger needs to see. `#[debug_bus]` on the
  board field merges the board's devices, CPUs, maps and watchpoints into the
  machine's impl; local entries come first, so a `#[debug_cpu]` on the machine
  keeps CPU index 0 and device indices stay contiguous across the join. The
  derive also drives read/write/poke arms from `#[debug_map(cpu = N)]` rather
  than from the CPU list, so a board that no longer owns its CPU still answers
  debug reads for it.
- **`impl_board_delegation!(…, split_cpu)`**. `debug_tick()` becomes the
  machine's inherent `step_cycle()`, and `debug_bus()` is the machine itself.
- **`SAVE_VERSION` 4 → 5**. Moving CPUs between structs moves the CPU block
  within a machine's state. Pac-Man's layout happened to be preserved; Galaga's
  was not, so the version was bumped and old files are rejected rather than
  misread. Later rollout steps that change layout again do **not** need another
  bump within the same release — but do check whether a machine's block order
  actually moved.

## Open problems for the rollout

### `Cpu::reset` takes `&mut dyn Bus + 'static`

`BusMasterComponent::Bus` is an associated *type* (`dyn Bus<Address = u16, Data =
u8>`), so a borrowed view struct cannot be passed to `Cpu::reset` — the lifetime
cannot be named. The Z80 ignores the bus at reset, so both prototypes sidestepped
it (Pac-Man resets against the board; Galaga uses `Z80::hardware_reset`).

**The 6502, 6809 and 68000 fetch their reset vector through the bus.** Their
boards cannot convert until `Cpu::reset` is generic over the bus type:

```rust
fn reset<B: Bus<Address = Self::Address, Data = Self::Data> + ?Sized>(
    &mut self, bus: &mut B, master: BusMaster);
```

This is a phosphor-core change touching every CPU and every reset call site, and
it makes `Cpu` non-object-safe — no `dyn Cpu` exists in the workspace today, so
that costs nothing. **It is the first rollout task, and it blocks every non-Z80
board.**

### Boards with a second bus master

Williams drives its blitter as `BusMaster::Dma`/`DmaVram` through the same bus.
The blitter is board state, so it sits *inside* the bus and re-enters through
`&mut self` — no split needed for that path, and it should convert like any other
board. It is called out here because it is the only place where a non-CPU master
touches the trait, and because `DmaVram` bypasses banking, so any refactor of the
Williams read path must keep those two masters distinguishable.

### The wide-bus `bus_split!` arms

`bus_split!` has three arms: `u16`/`u8` (nearly everything), `u32`/`u8` (Q*bert
on Gottlieb System 80), and `u32`/`u16` (Marble Madness and Road Runner on Atari
System 1). Nothing about
the conversion is width-specific — the arms exist only to name the trait object's
type parameters, and disappear with it. Atari System 1's `bus_addr: u32 word`
option on `impl_board_delegation!` goes away at the same time as its
`bus_split!`.

`bus_split!` itself is deleted when the last machine converts, along with the
`#[allow(unused_unsafe)]` and the safety comment that justified it.

## Rollout order

Ordered by payoff per unit of risk: shared boards with several machines first
(one conversion, several machines), Z80 boards before the CPUs that need the
`Cpu::reset` change, and the odd standalone machines last.

1. **`Cpu::reset` generic over the bus** — unblocks everything non-Z80.
2. **TKG-04** (Donkey Kong, DK Jr, Mario Bros) — Z80 + I8035 + DMA; three
   machines on one board, no reset-vector CPU.
3. **Galaxian** (Moon Cresta, Pisces) and **MCR II** (Satan's Hollow) — single
   Z80 boards, mechanical.
4. **Do Castle** — two Z80s and five registered variants in one file, eight
   `bus_split!` sites, board and machines not split into a shared module.
5. **Williams** (Joust, Robotron, Sinistar) — M6809 + M6800 + blitter; needs (1),
   and is the second-bus-master case.
6. **Atari DVG/AVG family** (Asteroids, Asteroids Deluxe, Lunar Lander, Quantum,
   Star Wars) — M6502 boards; needs (1). Star Wars has seven `bus_split!` sites.
7. **Atari System 1** (Marble Madness, Road Runner) — M68000, the `u32 word`
   arm; needs (1).
8. **Gottlieb System 80** (Q*bert) — I8088 + M6502, the `u32` arm; needs (1).
9. **Standalone leftovers** — Burger Time, Congo Bongo, Frogger, Scramble, Mr.
   Do, Gridlee, I, Robot, Tempest, Crystal Castles, Missile Command, Food Fight,
   Simple\* test systems.
10. **Delete `bus_split!`** and update `CLAUDE.md`, which still tells new machines
    to use the borrow-splitting `unsafe`.

Each step: convert, run `cargo test -p phosphor-machines` and the ROM-gated
harness suites, run the golden frames (they will not catch a per-cycle split, but
they will catch a reordered tick), and measure the board with `phosphor-bench`
before and after. A step that does not improve its board's ms/frame has almost
certainly split inside the loop.

### Invert the frame loop while you are in there

Once a board's cycle loop is concrete, the other thing worth doing to it is
moving the frame-position tests out. Every board's per-cycle work opens with a
`clock % cycles_per_frame`, a scanline-divisibility test and a ladder of position
comparisons, all evaluated on each of ~50,000 cycles to be true on a few hundred.
Splitting that into a `begin_scanline` (the boundary work) and a
`begin_cycle_inner` (no position test at all), and running scanline-outer /
cycle-inner, was worth a further 7–18% on the Namco boards — as much as the
dispatch change itself:

| machine | dyn baseline | concrete | + scanline-outer | total |
|---------|--------------|----------|------------------|-------|
| galaga  | 0.887        | 0.759    | 0.646            | −27.2% |
| xevious | 1.164        | 1.037    | 0.850            | −27.0% |
| pacman  | 0.748        | 0.706    | 0.611            | −18.3% |
| digdug  | 2.091        | 1.956    | 1.826            | −12.7% |

Keep the per-cycle `tick` as the debugger's single-step path and route any
partial scanline through it, so both paths run the identical sequence of cycles
and share one copy of each piece of work.

## What this does not fix

Concrete dispatch removes the indirect call, not the work behind it.
`bus_read_common` still does a page lookup, a watchpoint check and a trace check
on every access, and is too large to inline. The next lever — now available only
because dispatch is concrete — is an inlinable fast path for the common
backed-memory case, with the debug-observability work outlined. That is a
separate issue, and it is where the remaining headroom is.
