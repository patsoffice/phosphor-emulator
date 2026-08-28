# Design: Road Runner Video Timing Conformance ROM

> **Status: in progress**, as
> `phosphor-emulator-roadrunner-video-conformance-wfop` under
> `phosphor-emulator-conformance-rom-programme-hl4t`. Second board of the
> programme, after [`williams-video-conformance.md`](williams-video-conformance.md),
> and a prerequisite instrument for [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md) W3.
>
> The ROM is at `machines/tests/roms/roadrunner_video.asm`, assembled to
> `roadrunner_video.bin` beside it, and the harness is
> `machines/tests/roadrunner_video_timing_test.rs`.
>
> All three steps have landed: `uzbt`, the skeleton that proves the Williams
> loading mechanism carries to a 68000 board on `AddressSpace32`; `m0bu`, the
> video assertions whose verdict is a word in RAM; and `78lx`, a picture through
> all three layers plus the two mid-frame assertions, which describe behaviour
> that is **wrong on purpose** and are held as a ratchet against
> `raster-sampling-fidelity.md` W3.
>
> Every figure in *Result block* below has been **measured** on a ROM-less
> roadrunner unless marked otherwise. Where a measurement corrected this
> document, it says so rather than being quietly rewritten. Two did: the poll
> path and the interrupt path turned out not to fit in one frame, and a
> count-based timeout turned out not to bound an interrupt storm. Both are under
> *Measurements*.

## Context

Williams answered "can a synthetic ROM measure when a machine draws" for one
board. `hl4t.2` asks a different question: what does a *shared* conformance-ROM
harness look like. That cannot be derived from one example, and it cannot be
derived from two examples that differ only cosmetically, which is why the survey
([`conformance-rom-board-survey.md`](conformance-rom-board-survey.md)) picked the
board that differs from Williams in every axis the harness touches:

| Axis | Williams | Road Runner |
|---|---|---|
| Main CPU | M6809, 8-bit bus | M68000 (hardware M68010), 16-bit word bus |
| Address space | `AddressSpace16` | `AddressSpace32` |
| Video model | bitmap, per-scanline render | tilemap plus motion objects, whole-frame render |
| Interrupts | one PIA, two edge inputs | four autovectored levels |
| Scratch storage | undisplayed video RAM | 8 KB of dedicated work RAM |
| Sync primitive | 4-line video counter at `$CB00` | 1-line motion-object timer IRQ3, plus a live VBLANK level |
| Watchdog | counts, nothing acts on it | reboots the machine after 8 frames |

The last two rows are the ones that force a harness contract to be honest. A
contract shaped around "poll a counter register" would not survive a board whose
beam position is only visible through a programmable interrupt.

### Why the skeleton is its own step

Everything the Williams harness does rests on properties of `AddressSpace16` and
an M6809: `debug_write` ignoring `AccessKind`, `ReadOnly` regions carrying
backing, and a CPU that fetches its reset vector through the bus. The 32-bit
half was checked *statically* during the survey and never run. Until a program
executes out of poked ROM on a bare Road Runner, the rest of the epic is
speculation, so this step deliberately asserts nothing about video: it asserts
that the loader works, that the program runs, and that it survives the watchdog.

## Mechanism: still no new plumbing

`AddressSpace32::debug_poke` (`core/src/core/address_space32.rs:466`) writes
straight to the backing store with no `AccessKind` check, and `ReadOnly` regions
are allocated backing (`:124`). `debug_write` (`:501`) is the parity wrapper the
`#[derive(BusDebug)]` plumbing calls, and for a 32-bit map the derive passes the
address untruncated (`macros/src/lib.rs:179`). So a test can patch program ROM on
a machine built with **no ROM set at all**:

```rust
let entry = registry::find("roadrunner").unwrap();
let mut m = (entry.create_bare)();                       // no ROMs, CI-safe
{
    let bus = m.debug_bus_mut().unwrap();
    for (i, b) in PROGRAM.iter().enumerate() {
        bus.write(0, i as u32, *b);                      // 8 KB image at 0x000000
    }
}
m.reset();                                               // fetches SSP and PC through the bus
```

`M68000::reset` (`core/src/cpu/m68000/mod.rs:443-462`) reads the supervisor stack
pointer from `0x000000` and the program counter from `0x000004` **through the
bus**, so both come from the poked image. The program records the stack pointer
it was handed into its result block, which is what turns "the loader worked" from
an inference into a measurement: a wrong or missing vector 0 shows up as a wrong
`R_SSP` rather than as a silent wander.

Four properties of the bare board make this work:

| Property | Where |
|---|---|
| Program ROM at `000000-07FFFF` is a backed `ReadOnly` region | `atari_system1.rs:555-561` |
| `debug_poke` ignores `AccessKind`, and `ReadOnly` regions are allocated backing | `address_space32.rs:466`, `:124` |
| `M68000::reset` fetches vectors 0 and 1 through the bus | `m68000/mod.rs:456-461` |
| `AtariSystem1Board::reset` does not clear map backing | `atari_system1.rs:1224-1243` |

## Board facts the tests are derived from

All from `machines/src/atari_system1.rs` and `machines/src/roadrunner.rs`.

| Quantity | Value | Source |
|---|---|---|
| Main CPU | M68000 core, `M68kVariant::M68010`, 7.15909 MHz | `atari_system1.rs:611-615`, `TIMING` `:290` |
| Frame | 262 scanlines x 456 cycles = 119,472 cycles, 59.92 Hz | `TIMING` `:290-297` |
| Framebuffer | 336 x 240 RGB24 | `TIMING` |
| Program ROM | `000000-07FFFF`, backed `ReadOnly` | `:555-561` |
| Reset vectors | SSP at `000000`, PC at `000004`, fetched through the bus | `m68000/mod.rs:456-461` |
| Work RAM | `400000-401FFF`, 8 KB, undisplayed | `:565-568` |
| VBLANK | scanline 240; raises IRQ4, acked by a write to `8A0001` | `VBLANK_SCANLINE` `:329`, `:1139-1141`, `:1315` |
| VBLANK level | `F60000` bit 4, **active low** (0 during blank) | `read_f60000` `:764-777` |
| IRQ levels | 6 sound response, 4 VBLANK, 3 MO scanline, 2 ADC | `interrupt_level` `:843-855` |
| Watchdog | any write to `880001` clears it | `:1314` |
| Watchdog bite | `advance_watchdog` once per frame, reboot at 8 | `:1195-1198`, `roadrunner.rs:773-775` |
| Slapstic window | `080000-087FFF`, state machine driven by every data access | `:1260-1267`, `:828-832` |
| MO timer IRQ3 | `(256 - (word0 >> 5) - vsize * 8 - 1) & 0x1FF`, one scanline | `timer_irq_at_scanline` `:797-820` |
| IRQ3 poll path | `2E0000` bit 7 | `int3_state` `:785-787` |
| Byte writes | become a word read-modify-write at the even base | module doc `:39-43`, `addressing.rs:238-258` |

### Two derived facts worth stating up front

**The watchdog is the first thing that will break a program here, and it is not
subtle in the way it fails.** `RoadRunnerSystem::run_frame` calls
`advance_watchdog()` once per frame and calls `self.reset()` when the count
reaches 8. `reset()` re-fetches the reset vectors and restarts the program, but
it does **not** clear work RAM, so a rebooting program would leave a plausible
half-finished result block behind. The ROM therefore clears its result block at
entry, which turns a reboot into "the counters started over" rather than "the
counters stalled at a believable value". That distinction is the whole reason the
skeleton counts vblanks at all.

**A byte write to a control register is two bus transactions here, not one.**
`write_byte_at` reads the containing word, merges, and writes it back. For
`880001` the read lands on `880000`, which `bus_read` does not decode and which
returns `0xFFFF` with no side effect, so the strobe is safe. It is worth knowing
because the same pattern applied to a register whose *read* has a side effect
would not be, and every byte-wide register on this board sits at an odd address.

## The ROM

### Layout

The image is a flat 8 KB poked at `000000`, so the loader is a byte-for-byte copy
and the vectors are part of the image rather than a separate segment.

| Region | Use |
|---|---|
| `000000-0003FF` | 68000 exception vector table (256 longs) |
| `000400-001FFF` | code |
| `400000-40000F` | result block |
| `401F00` | initial supervisor stack pointer (vector 0) |

Vector 0 is the stack pointer and vector 1 is the entry point. **Every other
vector points at a stray-exception handler** rather than at zero, so a mistake
lands somewhere that records itself instead of executing whatever happens to be
at address 0. The handler writes a marker and the format/vector-offset word the
68010 frame carries at `6(sp)` (`m68000/README.md:159-163`), which names the
vector, then spins strobing the watchdog so the machine stays up long enough for
the harness to read the marker out.

Nothing in the program goes near `080000-087FFF`. The slapstic state machine
advances on any data access to that window and does not fault, it quietly changes
which bank is presented, so a stray pointer there would be invisible.

### Frame synchronisation

One primitive, and it is a level rather than a counter:

- `WaitVblank` reads `F60000`, waits until bit 4 is high (out of blank), then
  waits until it is low again. It returns on the frame's transition into vblank,
  which is scanline 240.

The wait is on hardware state, never on a cycle count, for the same reason as
Williams: a constant cycle offset between two implementations cancels.

`run_frame` runs a whole frame starting at scanline 0, so scanline 240 is inside
the frame the harness is currently running. A value published just after the
vblank edge is therefore observable by the harness at the end of that same
`run_frame()`, which is the same publication rule the Williams ROM uses at line
240.

### Phase protocol

The program writes a phase index to `R_PHASE` as each stage completes and
`$5A5A` to `R_MAGIC` at the very end. A zero result block is a wedge, not a pass;
`R_PHASE` says how far it got and `R_TRAP` says whether an exception is why.

| Phase | Meaning |
|---|---|
| 1 | entry reached: stack set up, result block cleared, reset SSP recorded |
| 2 | the CPU has checksummed the whole 8 KB image through the real bus |
| 3 | the first vblank edge has been seen |
| 4 | `VB_TARGET` vblank edges seen, with the watchdog strobed at each |
| 5 | T1, the VBLANK level and the calibration everything else divides by |
| 6 | T2, the VBLANK interrupt with the ack immediate and then deferred |
| 7 | T3, the scanline interrupt at line 64, poll path |
| 8 | T3 again at line 160 |
| 9 | T4, the same pulse down the interrupt path |
| 10 | T5, a timer installed mid-frame by the line-64 interrupt |
| 11 | complete, `$5A5A` written |

### Everything is measured in poll-loop iterations, and the loop calibrates itself

This board gives a program no line counter to read. Williams had one at `$CB00`
and every expectation there could be stated directly in scanlines; here the only
beam-position primitives are a level (VBLANK) and an interrupt (the
motion-object timer). So position is counted in iterations of one shared poll
loop, and the loop's rate is measured **in the same run that uses it**: T1 counts
iterations across the 240 active lines, and iterations-per-line is that over 240.
Every later figure is divided by it.

That is what keeps a constant out of the file. Change the loop, change the CPU
clock, change the emulator's cycle counts, and every ratio is unmoved. The
measured rate is 6.508 iterations per scanline; nothing asserts against that
number, and it is recorded here only so a reader can see the resolution the
assertions have (about a sixth of a line).

Two systematic effects follow from it and are worth stating rather than
absorbing into a tolerance:

- **Sampling.** The loop samples the beam asynchronously, so any interval reads
  correct to within one sample. Two measurements of the same blank a frame apart
  came out 142 and 141. Requiring them to be *identical* was tried first and
  passed, but only by coincidence of the loop period at the time; it broke as
  soon as the loop grew. The assertion is now one sample of slack.
- **The gap between two waits.** `rts` and `bsr` between a `WaitSet` and the
  `WaitClear` after it are time in which the counter does not advance, worth
  about half a sample. Every figure measured across a call boundary reads that
  much low, which is why the pulse width comes out at 0.92 lines rather than 1.00
  and why the interrupt-path position sits 0.3 lines below the poll-path one.

### Result block

Words, at `400000`. Words rather than bytes because a byte write on this bus is a
read-modify-write and a word write is one transaction; there is no shortage of
work RAM, so there is no reason to pack.

| Address | Field | Expected | Derivation |
|---|---|---|---|
| `400000` | `R_MAGIC` | `$5A5A` | completion |
| `400002` | `R_PHASE` | `11` | |
| `400004` | `R_TRAP` | `$0000` | no stray exception was taken |
| `400006` | `R_TRAPV` | `$0000` | the vector offset if one was |
| `400008` | `R_SSP` (long) | `$00401F00` | vector 0, fetched through the bus by `cpu.reset` |
| `40000C` | `R_CKSUM` | sum of the committed image | 4096 big-endian words, added with 16-bit wraparound |
| `40000E` | `R_VBCOUNT` | `16` | `VB_TARGET` |
| `400010` | `R_T1_BLANK` | 22 lines | 262 total less `VBLANK_SCANLINE` 240 |
| `400012` | `R_T1_ACTIVE` | 240 lines | *defines* iterations-per-line |
| `400014` | `R_T1_BLANK2` | within one sample of `R_T1_BLANK` | the same interval, a frame later |
| `400016` | `R_T2_COUNT` | `1` | IRQ4 acked on its first entry drops the level |
| `400018` | `R_T2_HELD` | `2` | with the ack deferred, RTE re-enters a still-asserted level |
| `40001A` | `R_T2_VB` | `1` | IRQ4 is raised at scanline 240, the first blanked line |
| `40001C` | `R_T3_POLL_A` | 86 lines | 22 blanked plus the timer's line 64 |
| `40001E` | `R_T3_END_A` | `R_T3_POLL_A` + 1 line | the pulse is one scanline wide |
| `400020` | `R_T3_POLL_B` | 182 lines | 22 plus line 160, and 96 lines past `POLL_A` |
| `400022` | `R_T4_CNT` | more than 1 | no ack, and a handler far shorter than a line |
| `400024` | `R_T4_FIRST` | `R_T3_POLL_A` | the autovector and the status bit are one signal |
| `400026` | `R_T5_POLL` | `R_T3_POLL_B` | the list is read live, so a mid-frame edit lands in the same frame |
| `400028` | `R_TIMEOUT` | `$0000` | no wait gave up and IRQ3 did not storm |

`R_CKSUM` is the assertion that the *whole* image arrived at the right address:
the CPU reads all 8 KB back through the real bus, not the debug bus, and the
harness computes the same sum over the committed `.bin` file in Rust. A load at
the wrong offset, a short image, or a poke that silently dropped `ReadOnly`
writes all move it.

`R_VBCOUNT` is the watchdog assertion. 16 is deliberately double the 8-frame
timeout: the program cannot reach 16 vblank edges unless every strobe landed,
because a reboot clears the result block and starts the count over. **The failure
mode was demonstrated rather than assumed**: see *Measurements* below.

`R_TRAP` and `R_TRAPV` are what separate "the loader never worked" from "the
program ran and then fell over". Those want completely different next steps, and
without the stray handler both present as a zero result block.

## The harness

`machines/tests/roadrunner_video_timing_test.rs`, ROM-less, one machine. It
builds with `create_bare`, pokes the image at `000000` through `BusDebug::write`,
resets, runs frames until the magic appears or `MAX_FRAMES` is reached, and reads
the result block back through the debug bus.

One test per property rather than one test collecting assertions, as on Williams,
and every test calls `assert_completed` first so a wedge fails on the magic byte
rather than on a handful of assertions about zeroes.

### The drift guard, and the trap it needs

The committed binary is the one artifact no reviewer can check by reading, so a
test re-assembles the source and byte-compares. **It must not be allowed to pass
by doing nothing.** The Williams guard reported green for its entire life because
no assembler existed on `PATH` anywhere, including inside the dev shell. The dev
shell now exports `PHOSPHOR_ASM=1` (`flake.nix:74`); with it set, a missing
assembler is a failure rather than a skip. CI has no dev shell, sets nothing, and
skips with a printed note.

That failure mode is demonstrated once here rather than inherited on faith: see
*Measurements*.

## Build and toolchain

`asl` and `p2bin`, already in `flake.nix` for Williams; `asl` targets 68000 with
no new dev-shell dependency.

```
asl -q -o roadrunner_video.p roadrunner_video.asm
p2bin roadrunner_video.p roadrunner_video.bin -r 0x0000-0x1FFF -l 0xA5
```

`p2bin`'s `-r` fixes the image at exactly 8 KB and `-l` fills the gaps, which is
what makes the load a flat copy and the checksum a fixed quantity.

**The fill byte is `$A5`, and that is not cosmetic.** It was `0x00` first, and
with a zero fill the image checksum was blind over the 85% of the image that is
padding: a ROM-less board's program ROM is already zero, so a truncated load
summed to exactly the same value as a complete one. Demonstrated by poking only
the first 4 KB, which passed. With `$A5` the same mutation fails. `$A5A5` is
also line-A, so a program counter that wanders into the padding vectors to the
stray handler instead of grinding through 3.5 KB of `ORI.B #0,D0`.

## Measurements

All on a ROM-less `roadrunner` built with `create_bare`.

### The loader (uzbt)

| Field | Predicted | Measured |
|---|---|---|
| `R_SSP` | `$00401F00` | `$00401F00` |
| `R_CKSUM` | sum of the image | equal to the harness's sum over the committed file |
| `R_VBCOUNT` | `16` | `16` |
| frames for the watchdog ride | about 16 | exactly 16 |

The program takes its first vblank edge in the frame it boots in (entry and the
8 KB checksum together cost roughly 160 scanlines, well short of scanline 240)
and publishes phase 4 on the sixteenth, so the ride covers exactly `VB_TARGET`
frames. That is asserted alongside the count, because the count alone would also
be satisfied by an edge detector that retriggered several times inside one blank.

**Nothing here needed correcting after the fact.** The loader carried to
`AddressSpace32` and the 68000 exactly as the static reading of the code said it
would, which is the one prediction in this document that mattered.

### The video signals (m0bu)

Iterations-per-line came out 6.508. Everything else is stated in lines, which is
that count divided out.

| Field | Derived | Measured | |
|---|---|---|---|
| `R_T1_BLANK` | 22 lines | 21.82 | 142 iterations |
| `R_T1_BLANK2` | within a sample of the above | 21.66 | 141 iterations |
| `R_T2_COUNT` | 1 | 1 | ack on the first entry |
| `R_T2_HELD` | 2 | 2 | ack deferred by one entry |
| `R_T2_VB` | 1 | 1 | the handler ran inside the blank |
| `R_T3_POLL_A` | 86 lines | 85.74 | timer at line 64 |
| pulse width | 1 line | 0.92 | `END_A - POLL_A` |
| `R_T3_POLL_B` | 182 lines | 181.92 | timer at line 160 |
| the move | 96 lines | 96.18 | `POLL_B - POLL_A` |
| `R_T4_FIRST` | `= POLL_A` | 85.43 | 0.31 lines apart |
| `R_T4_CNT` | more than 1 | 3 | |
| `R_T5_POLL` | `= POLL_B` | 181.61 | 0.31 lines apart |

Every one of those landed on its derivation the first time it ran. What did not
land the first time was the *structure* of two of the measurements, and both
corrections are recorded here rather than smoothed over.

**The poll path and the interrupt path cannot share a frame.** The first version
measured both in one frame, on the theory that the handler could snapshot the
polling loop's own counter and the two would agree to an iteration. It hung.
IRQ3 is a level the board holds for one scanline and nothing acknowledges it, so
`rte` lowers the mask back into a still-asserted interrupt and the handler is
re-entered before the interrupted instruction retires: for the whole of that
scanline the polling loop gets **zero** iterations, and the one pulse it exists
to observe passes entirely inside the interrupt storm. The two paths now get a
frame each. Since the timer entry is static, the position is the same in both,
and agreeing across two frames is the same check the single frame was meant to
be.

**A count-based timeout cannot bound an interrupt storm.** The wedge above was
the argument for bounding every wait, so a regression would report "gave up in
phase N" instead of spinning until the watchdog rebooted the machine and the
harness blamed whatever phase the restarted run reached. That works for a signal
that never arrives. It does nothing for a signal that never leaves: the limit
lives in the polling loop, and during a storm the polling loop does not execute.
The handler had to bound itself, which it now does at 64 entries.

### Every guard made to fail once

Rule: a check that cannot fail is not a check. Each of these was broken on
purpose and watched to fail before being trusted.

1. **The watchdog assertion.** Deleting the `PetDog` call from the vblank loop
   and rebuilding: `R_VBCOUNT` reads `8`, `R_PHASE` reads `3`, `R_MAGIC` is
   absent, and the run burns every frame it is given. That is the reboot, and it
   is the failure mode the issue predicted: the counters restart rather than
   stall, because entry clears the result block.
2. **The drift guard.** Zeroing one byte of the committed binary at `$000406`:
   the guard reports `first difference at $000406: built 0x1F, committed 0x00`
   with the rebuild commands.
3. **The `PHOSPHOR_ASM` trap.** Running the guard with `PATH=/nonexistent` and
   `PHOSPHOR_ASM=1` fails on the missing assembler; the same run with
   `PHOSPHOR_ASM` unset skips with a printed note. Both branches were executed.
4. **`VBLANK_SCANLINE` moved from 240 to 220.** Three tests fail: the blank reads
   45.77 lines against 22, and IRQ3 lands 115.57 lines past the vblank edge
   against 86. The mid-frame timer test fails with it.
5. **`timer_irq_at_scanline` returning true unconditionally.** Reported as
   `IRQ3 fired more times in phase 5 than a one-scanline pulse can`, phase 5
   being the first stage that lowers the mask.
6. **IRQ3 latched to the end of the frame instead of one line.** The same report,
   at phase 8, which is the stage that takes the interrupt.

Two guards did **not** survive their first mutation and were fixed rather than
kept. The image checksum was blind over its own padding, which the fill byte
above covers. And mutations 5 and 6 originally produced a reboot loop and a
misleading phase number, which is what the bounded waits and the handler cap
above were added for; before them, the message named phase 3 for a defect in
phase 8.

## The picture (78lx)

### The board has to be given graphics before it can draw

A board built with no ROM set has no tile or font graphics at all.
`PlayfieldGfx::empty` leaves a single blank placeholder bank, so every playfield
and motion-object pixel decodes to pen 0 and the compositor has nothing to draw;
the alpha cache is 512 zeroed tiles for the same reason. The signal assertions
did not care, and the picture assertions cannot work without them.

So the harness builds its own font and tile set and installs them through
`load_alpha` and `load_gfx`, the same entry points the cartridge loader uses.
Tile N is a solid block of pen N for N in 1 to 4; the font is eight 8x8 glyphs
written as string art. Both are *defined* rather than captured, which is what
keeps the expected picture derivable. It costs the registry's `create_bare`: the
machine is constructed directly so the graphics can be installed before the
program runs. Still no arcade ROMs, still CI-safe.

The bit layouts are restated in the harness from `ALPHA_LAYOUT`
(`atari_system1.rs:86-93`) and `tile_layout` (`:130-142`). That is a deliberate
coupling. Get either side wrong and the picture assertions fail rather than
quietly drawing the wrong thing.

### What it draws

All three layers, so the capture exercises the compositor rather than one corner
of it:

- a playfield of solid pen 1 across all 64 x 64 cells, red;
- "ROAD RUNNER" through the alpha layer at cell (2, 8), its pen 0 left
  transparent so the background shows through the glyphs;
- one motion object at (160, 80), four tiles tall, stepping codes 1 to 4 so each
  8-row band is a different pen and a different palette entry.

`the_program_draws_a_picture_through_all_three_layers` pins one thing from each
layer's own path: the playfield's PROM lookup and palette bank, the sprite's link
walk and per-row code stepping, and the alpha layer's glyph decode and
transparent pen 0. Without it, the two assertions below could agree about a black
screen.

### The two assertions that are supposed to fail

Both mid-frame writes are placed by the motion-object timer interrupt at
scanline 120, never by counting cycles.

**T6, the palette split.** The playfield is uniformly pen 1 and pen 1 is red. At
line 120 pen 1 becomes green. Hardware keeps the rows already scanned out red and
turns the rest green, one transition. This board reads the palette once at the
frame boundary, so all 240 rows come out green.

**T7, the beam has already passed.** At line 120 two playfield cells become pen 2,
which is green: one covering screen rows 48-55, drawn long before, and one
covering 200-207, not yet reached. Hardware changes only the lower one in that
frame and both by the next. This board changes both at once.

### How they are held: a ratchet, not an ignore

Each test asserts the **defect is still present** and its failure message states
what the correct answer is, that the fix is to write that expectation, and that
the test must not be deleted. The suite is green today,
`raster-sampling-fidelity.md` W3 turns it red the day it lands, and the only way
back to green is the correct expectation. This is the shape
`harness/tests/audio/expectations.toml` already uses for the same reason: the
list can only shrink and cannot quietly absorb a regression.

`#[ignore]` was rejected because an ignored test is invisible and
`phosphor-emulator-gn5w` is what happens when nobody comes back for one. Asserting
the current behaviour under only a comment was rejected because nothing then
fires when it stops being wrong.

T6 orders its assertions so the diagnosis is right in both directions: no green
at all means the write never landed, which is a broken fixture rather than a
rendering-model question, and only then does the ratchet speak.

### Both were shown to discriminate

Required, and done by mutation rather than argument. `render_frame` was
temporarily given a two-band composite: rows above line 121 rendered from a
palette and playfield snapshot taken at scanline 0, rows below from the state at
the frame boundary. That is the minimum change that produces the per-beam answer
for these two writes, since both happen on one known line.

Under it, T6 reports 121 rows still red against 119 green, and T7 reports its
upper row red. Both are the correct hardware answers, both fire the ratchet, and
both print the message telling the reader to write the real expectation. The six
signal assertions were unaffected, which is the right pattern for a
rendering-only mutation. Reverting restored green.

Anyone revisiting whether these two tests earn their keep should redo that
mutation rather than trust this paragraph.

### One thing the sequencing cost

Phase 12 is published at the vblank edge, which is still inside the frame the
harness is running and will capture when `run_frame` returns. The first version
restored the palette immediately afterwards, so red was back before T6's frame
was ever rendered and the test read a screen with neither colour where it wanted
green. The restore now rides out that frame first. This is the same class of
mistake as the Williams phase 9/10 collision, and the same fix.

## The rest of the compositor (7mee)

The picture above walks one path through each layer and leaves most of the merge
untouched. This step draws the rest, in bands that do not overlap so one capture
covers all of them, plus one more capture with the playfield scrolled. None of it
is about *when* the board draws, so none of it is affected by the whole-frame
model and none of it is held as a defect.

**Mirroring needs an asymmetric tile.** A solid block is its own mirror image, so
nothing built from tiles 1 to 4 can say anything about flip. Tile 5 is pen 1
across its left four columns and pen 2 across its right four, and swaps halves
when mirrored. Asserted on a motion object (word 0 bit 15) and on a playfield
cell (bit 15).

**The priority merge, all three paths.** A high-priority sprite does not draw its
own colour: the pixel resolves to `0x300 + (playfield pen << 4) + sprite pen`, so
a pen-2 sprite over the pen-1 background is entry `0x312`, which the program
paints a colour neither layer's own palette contains. Beside it the one pen the
merge excludes, where a high-priority sprite pen of 1 draws nothing at all. And
below, two identical low-priority sprites differing only in what is behind them:
`840000` bit 2 puts the playfield in front for colour-0 pen 2, so one loses and
the other draws.

**Scroll.** Both registers to 8. The playfield moves up and left by 8 and the
alpha and motion-object layers do not move at all, which is the half that catches
a scroll applied in the wrong place.

**The alpha layer's force-opaque bit and colour field.** A cell whose glyph is
code 0 has every pen 0 and would draw nothing; bit 13 forces it opaque anyway and
the colour field selects `colour * 4 + pen`.

### Three flips this hardware does not have

`Characters can flip too` was the question, and on this board they cannot. All
three facts come from the hardware description rather than from our code:

| Layer | horizontal | vertical | where |
|---|---|---|---|
| alpha | **none** | **none** | its tile info carries no flip flag at all; the only flag is force-opaque, which is a layer bit (`0x10`) clear of the flip bits (`0x01`, `0x02`) |
| playfield | cell bit 15 | **none** | the flags argument is `(data >> 15) & 1`, which is `TILE_FLIPX`; nothing supplies `TILE_FLIPY` |
| motion objects | word 0 bit 15 | **none** | the object descriptor gives a horizontal-flip mask of `0x8000` and a vertical-flip mask of **zero** |

Our board matches all three, and the absences are now pinned, because an absence
is exactly what a later refactor adds by accident while generalising a tilemap.
Tile 6 is pen 1 over pen 2, vertically asymmetric, so an upside-down copy would
be obvious; the same sprite is drawn twice, once with every bit words 0 and 2 do
not decode set, and the two must be identical. The alpha layer gets the same
treatment with an asymmetric glyph and both spare cell bits.

**Those comparisons carry a blank check, and it is not decoration.** Two
identical *empty* blocks compare equal, so without first proving the reference
block drew something the assertion would be a null check comparing two silences
and could never fail. That defect has appeared in every subsystem of this
repository; it is not going to appear here.

### Each one made to fail

| Mutation | What failed |
|---|---|
| motion-object horizontal flip disabled | the mirrored sprite reads as unmirrored |
| a vertical flip added on word 0 bit 14 | the two spare-bit sprites differ at offset (0, 0) |
| an X flip added to the alpha layer on bit 15 | the two spare-bit cells differ at offset (1, 0) |
| the merge's pen-1 exception removed | a high-priority pen-1 sprite paints over the playfield |
| the `840000` mask ignored | the sprite over pen 2 draws instead of losing |
| `xscroll` ignored | the playfield does not move |
| the force-opaque bit ignored | the blank glyph draws nothing |

## The second opinion: the same binary under MAME

Every expected value in this suite is derived from our own board file. That makes
it a regression guard immediately and a correctness guard only once each figure
has been checked against something that is not us. `tools/mame_roadrunner_conformance.lua`
is that something, and `mame_agrees_about_every_signal_the_rom_measures` is the
comparison.

The ROM was built for this. It never waits on a cycle count, and every position
it reports is divided by an iterations-per-line figure it measures in the same
run. That was a design claim carried through three issues without ever being
tested; this is the test of it.

### How the image gets in

The same trick the Rust harness uses, by a different door. Our harness pokes the
program-ROM region through `BusDebug`; the Lua script writes the image into
MAME's `maincpu` memory region and soft-resets the machine so the 68010
re-fetches its vectors out of it. A soft reset does not reload ROMs, so the patch
survives. `region:write_u8` takes a *logical* offset and handles the host swizzle
itself, which matters because the region is 16-bit big-endian; getting it wrong
would byte-swap the image, and the ROM's own checksum phase is what would catch
it.

**An autoboot script is re-run on every reset, including its own.** Without a
guard, the script patches, resets, is re-executed, patches, resets, and the
machine never reaches its second frame. The Lua state survives a soft reset, so a
global is enough to tell the second execution to stand down.

### The result

| Quantity | Ours | MAME | Derived |
|---|---|---|---|
| iterations across the 240 active lines | 1562 | 1608 | *(no expectation, and none possible)* |
| VBLANK dwell | 21.82 lines | 21.79 | 22 |
| ... measured again a frame later | 21.66 | 21.79 | same |
| IRQ4 entries, ack immediate / deferred | 1 / 2 | 1 / 2 | 1 / 2 |
| IRQ3 at line 64 | 85.74 lines | 85.82 | 86 |
| IRQ3 pulse width | 0.92 lines | 0.90 | 1 |
| IRQ3 at line 160 | 181.92 lines | 181.94 | 182 |
| the move between them | 96.18 lines | 96.12 | 96 |
| the level-3 autovector | 85.43 lines | 85.52 | = the poll path |
| the mid-frame timer | 181.61 lines | 181.49 | 182 |
| stack pointer, checksum, vblank count | identical | identical | |

**The raw counts differ by 3% and every derived figure agrees to within a tenth
of a scanline.** That is exactly the difference the calibration exists to cancel,
and it cancels.

### Why the raw counts differ, which turns out to be one instruction

Not a vague "different cores round differently". The gap is 2 cycles per loop
iteration and it has a single cause.

The shared poll loop is six instructions. On the 68000's documented tables:

| | cycles |
|---|---|
| `subq.w #1,(xxx).l` | 8 + 12 (abs.l word EA) = 20 |
| `beq.s` **not taken** | **8** |
| `addq.w #1,(xxx).l` | 20 |
| `move.w (a0),d0` | 8 |
| `and.w d1,d0` | 4 |
| `beq.s` taken | 10 |
| | **70** |

240 active lines is 240 x 456 = 109,440 cycles, and 109,440 / 70 = 1563. We
measure 1562.

MAME charges 68 for the same six instructions, and 109,440 / 68 = 1609 against a
measured 1608. The two cycles are the **not-taken byte branch**: MAME's Musashi
core sets `m_cyc_bcc_notake_b` to -2 for the 68000 and to **-4** for the 68010
against a base of 10, so a not-taken `beq.s` costs 8 on a 68000 and 6 on a
68010 (`m68kcpu.cpp:2118`, `:2174`). The loop contains exactly one.

**So MAME is the faster one here, and ours is the 68000.** Our core runs this
board as `M68kVariant::M68010` but charges 68000 cycle counts throughout; its
README says so in as many words, the variant gate covering only the exception
frame and `MOVE from SR` privilege. This is the first time that documented gap
has been measured rather than noted, and it is filed as
`phosphor-emulator-zi4z`. It is not a conformance-ROM defect and nothing here
depends on it, which is the whole point of dividing by a rate measured in the
same run.

Counts and identities are compared exactly; positions are compared in scanlines,
never in iterations. `R_T4_CNT` is not compared at all: it counts how many times
a handler re-enters inside one scanline, which is a function of how long that
handler takes and is not a property of the board. Both cores happen to report 3.

### What it found, which is the point of having it

**The ROM's sound-CPU assumption was wrong, and only a warm machine could show
it.** The program held the sound CPU in reset by writing 0 to `860001`, on the
reasoning that a running sound CPU latches responses and those drive IRQ6, which
outranks everything being measured. Under MAME the first run stopped at phase 5
with `R_TRAP` set and `R_TRAPV` reading `$78`: vector 30, the level-6 autovector.

The cause is that the reset line is driven from bit 7 of that latch, and MAME
acts on it only when bit 7 *changes*. `m_bankselect` starts at 0 and
`machine_reset` writes 0, so the change never happens, the sound CPU is never
held, it runs the real sound ROM from power-on and latches a response. Our own
write of 0 was likewise a no-change, so it never acknowledged anything either.

The fix is in the ROM, not in either emulator: it now writes `$80` and then `$00`
to force the edge whatever the latch held, and reads `FC0001` to drain a response
already waiting. A conformance ROM should not depend on power-on trivia. **This
is the stray-exception handler earning its place**: without it the failure would
have been a wedge at an unrelated phase instead of "vector 30, phase 5".

There is a real divergence underneath it, recorded rather than acted on. Our
board drives the sound reset from the *level* of bit 7 and holds the sound CPU
from construction; MAME drives it from the *edge* and therefore lets it run at
power-on. A level-driven reset line is the more plausible reading of the
hardware, which would make our side right, but that is a schematic question and
the schematic is not to hand. Where the two disagree, the schematic decides, not
MAME. Nothing was changed on MAME's say-so.

### The picture, which is a separate and unfinished exercise

The CI-safe suite installs synthetic graphics so it can run with no arcade ROMs,
while MAME runs the real ones, so the same writes draw different pictures and
comparing them directly is meaningless. Substituting *our* graphics into MAME is
not possible: `gfx_element` decodes lazily and caches, and MAME's Lua exposes no
way to invalidate a decoded tile.

So it is done the other way round, in
`harness/tests/roadrunner_mame_picture_test.rs`: the real Road Runner graphics
are loaded into **our** board, the same image is poked over the program ROM, and
every pixel of all six captured frames is compared. It is ROM-gated and gated on
`PHOSPHOR_MAME_PICTURE`, separately from `PHOSPHOR_MAME`, **because it does not
pass yet**. Tracked as `phosphor-emulator-j5wp`.

It has already earned its keep by finding one thing and localising two more.

**The ROM assumed a cold machine, for the second time.** It wrote the nine
palette entries and ten sprite-list entries it used and left the other 1015 and
2038 holding whatever was there. On our board that is zeroes; under MAME the real
game had been running first, so unwritten palette entries came out in the attract
mode's colours and leftover motion objects drew down the left edge. The ROM now
clears all of palette RAM and all of the motion-object list at entry, which took
the difference from about 900 pixels a frame to 64. Exactly the same class of
assumption as the sound latch above, found the same way.

What remains is two things, and they are different in kind:

1. **A capture-alignment race, which is a fixture bug.** The ROM publishes each
   phase at the vblank edge and MAME updates its screen on that same scanline, so
   which side of a mid-frame write MAME's dump lands on is a coin toss. Measured
   rather than assumed, with `PHOSPHOR_PICTURE_ALIGN=1`: our phase-11 frame
   best-matches MAME's phase *13*, and our phase-12 frame matches MAME's phase
   11. The fix is in the ROM, publishing a few lines into vblank rather than on
   its first.
2. **A residual of exactly 64 pixels**, one 8x8 cell at (0, 120), present in
   every phase and independent of the alignment. Small, constant and
   suspiciously cell-shaped. That is the part worth chasing, and it cannot be
   chased until (1) stops drowning it out.

**The playfield colour path was suspected and cleared.** Our index is
`0x100 + (0x20 + (palcolor << (bpp - 3))) * 8 + pen`; MAME's is a gfx colorbase
of 256 plus `color * granularity` with granularity 8, the same colour expression
and the same `bpp - 3` shift (`atarisy1_v.cpp:650-651`, the 256 being the
`gfx_element` constructor's `color_base` at `:634`). They agree exactly, and the
structure of the two pictures matches in every frame: same glyphs, same sprite
geometry, same positions. Only colours differ, and only where the two causes
above explain it.

### Running it

Not a CI gate: MAME is not in the dev shell and the arcade ROMs are not
redistributable. Gated on `PHOSPHOR_MAME` exactly as the drift guard is gated on
`PHOSPHOR_ASM`, and for the same reason: with the variable set, a missing `mame`
is a failure rather than a skip.

```bash
PHOSPHOR_MAME=1 PHOSPHOR_ROMS=~/ws/mame-runtime/roms \
  cargo test -p phosphor-machines --test roadrunner_video_timing_test mame_agrees
```

Both branches of that gate were exercised, and the comparison itself was made to
fail by moving `VBLANK_SCANLINE` to 232: it reports "the VBLANK dwell: 30.83
scanlines here against 21.79 under MAME", with the raw counts printed beside it
and a note that those are expected to differ.

## What this does not cover yet

The two picture assertions are `78lx` and land red until
`raster-sampling-fidelity.md` W3 converts this board off whole-frame rendering.
The picture half of the read-twice asymmetry belongs with them: the interrupt
half is measured here, and it is the half that needs no framebuffer.

Nothing about the sound board, the ADC, IRQ2, the EEPROM, or the slapstic. Sprite
and tile decode, palette derivation and orientation belong to the golden frames.

The sound CPU is held in reset by an explicit write of 0 to `860001` at entry, so
IRQ6 cannot assert; the ADC is never started, so IRQ2 cannot either. Both are
left pointing at the stray-exception handler rather than given a stub, so if
either does assert it is reported as a finding instead of swallowed.

## Risks

1. **The ROM encodes our model, not the hardware.** Every expected value here is
   derived from our source, which makes the suite a regression guard immediately
   and a correctness guard only once each figure is checked against a schematic.
   For this step that is almost vacuous, since the skeleton asserts loader
   mechanics rather than hardware behaviour, but it stops being vacuous at
   `m0bu`.
2. **A wedged program reads as a pass** unless the magic word is checked first.
   Guarded by `assert_completed`, which every test calls before anything else.
3. **A guard that cannot fail is worse than no guard.** Two here: the drift guard
   (skips when no assembler is present) and the watchdog assertion (would pass
   trivially if the program finished inside 8 frames). Both are made to fail on
   purpose and the result is recorded under *Measurements*.
4. **The image is poked, so nothing checks it against the ROM the board would
   normally load.** `load_program` also installs a slapstic ROM from
   `0x80000-0x88000` of the cartridge image; the conformance image has no such
   half and the window is left holding zeros. That is fine only because the
   program never reads it.

## Sequencing

Tracked as `phosphor-emulator-roadrunner-video-conformance-wfop`.

1. **Done** (`uzbt`) - design doc, skeleton ROM, harness, drift guard. No video
   assertion.
2. **Done** (`m0bu`) - the VBLANK level, IRQ4 and its ack, the placeable IRQ3 on
   both the poll and the interrupt path, and the interrupt half of the read-twice
   asymmetry. Twelve assertions, no arcade ROMs.
3. **Done** (`78lx`) - synthetic graphics, a picture through all three layers,
   and the two picture assertions held as a ratchet against W3. Fifteen
   assertions, no arcade ROMs.
4. **Done** (`7mee`) - the compositor paths the first picture missed: mirroring,
   the priority merge, scroll, the alpha force-opaque bit, and the three flips
   this hardware does not have. Twenty-one assertions in all.

## References

- `machines/src/atari_system1.rs` - memory map `:553-605`, `begin_scanline`
  `:1128`, `read_f60000` `:764`, `timer_irq_at_scanline` `:797`,
  `advance_watchdog` `:1195`, bus decode `:1271`, `:1300`
- `machines/src/roadrunner.rs` - `run_frame` `:765`, `reset` `:780`
- `core/src/core/address_space32.rs:466` - `debug_poke`, the loading mechanism
- `core/src/cpu/m68000/mod.rs:443` - `reset`, the vector fetch
- `core/src/cpu/m68000/README.md` - the 68010 format $0 exception frame
- [`williams-video-conformance.md`](williams-video-conformance.md) - the pattern
  this follows
- [`conformance-rom-board-survey.md`](conformance-rom-board-survey.md) - why this
  board
