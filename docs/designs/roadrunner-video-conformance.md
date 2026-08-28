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
> This step (`uzbt`) is the skeleton: it proves the Williams loading mechanism
> carries to a 68000 board on `AddressSpace32`, and nothing about video. The
> CPU-observable video assertions are `m0bu`; the two picture assertions are
> `78lx`, and they land red on purpose.
>
> Every figure in *Result block* below has been **measured** on a ROM-less
> roadrunner unless marked otherwise. Where a measurement corrected this
> document, it says so rather than being quietly rewritten.

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
| 5 | complete, `$5A5A` written |

### Result block

Words, at `400000`. Words rather than bytes because a byte write on this bus is a
read-modify-write and a word write is one transaction; there is no shortage of
work RAM, so there is no reason to pack.

| Address | Field | Expected | Derivation |
|---|---|---|---|
| `400000` | `R_MAGIC` | `$5A5A` | completion |
| `400002` | `R_PHASE` | `5` | |
| `400004` | `R_TRAP` | `$0000` | no stray exception was taken |
| `400006` | `R_TRAPV` | `$0000` | the vector offset if one was |
| `400008` | `R_SSP` (long) | `$00401F00` | vector 0, fetched through the bus by `cpu.reset` |
| `40000C` | `R_CKSUM` | sum of the committed image | 4096 big-endian words, added with 16-bit wraparound |
| `40000E` | `R_VBCOUNT` | `16` | `VB_TARGET` |

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

| Field | Predicted | Measured |
|---|---|---|
| `R_MAGIC` | `$5A5A` | `$5A5A` |
| `R_PHASE` | `5` | `5` |
| `R_TRAP` | `$0000` | `$0000` |
| `R_SSP` | `$00401F00` | `$00401F00` |
| `R_CKSUM` | sum of the image | `$472D`, equal to the harness's sum over the committed file |
| `R_VBCOUNT` | `16` | `16` |
| frames to completion | about 16 | exactly 16 |

The program takes its first vblank edge in the frame it boots in (entry and the
8 KB checksum together cost roughly 160 scanlines, well short of scanline 240)
and finishes on the sixteenth, so the run is exactly `VB_TARGET` frames. That is
asserted alongside the count, because the count alone would also be satisfied by
an edge detector that retriggered several times inside one blank.

**Nothing here needed correcting after the fact.** The loader carried to
`AddressSpace32` and the 68000 exactly as the static reading of the code said it
would, which is the one prediction in this document that mattered.

### The three guards, each made to fail once

Rule: a check that cannot fail is not a check. Each of these was broken on
purpose and watched to fail before being trusted.

1. **The watchdog assertion.** Deleting the `PetDog` call from the vblank loop
   and rebuilding: `R_VBCOUNT` reads `8`, `R_PHASE` reads `3`, `R_MAGIC` is
   absent, and the run burns all 32 frames. That is the reboot, and it is the
   failure mode the issue predicted: the counters restart rather than stall,
   because entry clears the result block.
2. **The drift guard.** Zeroing one byte of the committed binary at `$000406`:
   the guard reports `first difference at $000406: built 0x1F, committed 0x00`
   with the rebuild commands.
3. **The `PHOSPHOR_ASM` trap.** Running the guard with `PATH=/nonexistent` and
   `PHOSPHOR_ASM=1` fails on the missing assembler; the same run with
   `PHOSPHOR_ASM` unset skips with a printed note. Both branches were executed.

A fourth guard did **not** survive its first mutation and was fixed rather than
kept: see the fill byte above.

## What this step does not cover

No video assertion of any kind. VBLANK's IRQ4, the placeable IRQ3 and the
`2E0000` poll path are `m0bu`; the two picture assertions are `78lx` and land red
until `raster-sampling-fidelity.md` W3 converts this board off whole-frame
rendering.

Nothing about the sound board, the ADC, IRQ2, the EEPROM, or the slapstic. Sprite
and tile decode, palette derivation and orientation belong to the golden frames.

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

1. `uzbt` (this step) - design doc, skeleton ROM, harness, drift guard. No video
   assertion.
2. `m0bu` - VBLANK, IRQ4, and the placeable IRQ3, in the same ROM.
3. `78lx` - the two picture assertions, landed red as W3's acceptance test.

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
