# Survey: what can each board's CPU see of the beam?

> **Status: research complete**, as
> `phosphor-emulator-conformance-rom-programme-hl4t.1` under
> `phosphor-emulator-conformance-rom-programme-hl4t`. No code was written and
> nothing in the tree was changed by this survey. Four defects it found while
> reading are filed separately and named at the end.
>
> Companion to [`williams-video-conformance.md`](williams-video-conformance.md),
> which is the one working instrument and the thing every judgement below is
> measured against, and to
> [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md), which is what
> wants an instrument on these boards.

## The question

The Williams conformance ROM is immune to a constant cycle offset between two
implementations because it never waits by counting cycles. It polls the video
counter at `$CB00`, which returns `scanline & $FC`, so every wait is keyed to
where the beam actually is. Reproducing that on a second board requires the
second board to expose something equivalent. Nothing said it does.

Eight boards were surveyed: the seven named by
[`raster-sampling-fidelity.md`](raster-sampling-fidelity.md) W3, plus `gridlee`,
which is the only other board in the tree already rendering per scanline.

## Method

Per board, read in this order: the MAME driver's memory-map header comment and
its machine configuration (interrupt wiring, screen parameters, CTC/timer
wiring), then the driver source for anything the header does not settle, then
our board file for the map, the timing constants and the interrupt code. Nothing
below was inferred by probing our own emulator, and where the reference and our
tree disagree that is called out rather than reconciled.

Two facts about the loading mechanism were checked once and hold for all eight:

| Property | Where |
|---|---|
| `AddressSpace32::debug_poke` writes backing regardless of `AccessKind`, exactly as the 16-bit space does | `core/src/core/address_space32.rs:466`, `address_space16.rs:509` |
| `asl` assembles every CPU in the candidate set | verified by assembling a `cpu` directive for each: 6809, Z80, 6502, 68000, 68010, 8086, 6800 all accepted; `8088` is not a valid type name but `8086` is the same encoding |

So "poke an image into the program-ROM window of a bare machine, then reset"
generalises to every board here without new plumbing. That was never the hard
part.

## The verdict scale needed a fourth value

The issue asked for three verdicts: FULL, FRAME, NONE. The survey found that the
interesting boards do not fall into any of them, and forcing them would have
thrown away the distinction that actually decides where to spend effort. The
scale used below is:

- **FULL** - the CPU can name an arbitrary scanline and be told when the beam
  reaches it, by reading a counter or by arming a programmable interrupt. A
  Williams-style ROM ports directly.
- **FIXED** - the beam is only visible through interrupts hard-wired to
  particular scanlines. A program can place an event at those lines and nowhere
  else, without counting cycles. This is weaker than FULL and much stronger than
  FRAME, and it is where three of the eight boards landed.
- **FRAME** - only frame-level anchors: a vblank level or one interrupt per
  frame. A ROM can test what happens across a frame boundary and can test that a
  mid-frame effect exists at all, but any assertion about *which* scanline it
  landed on has to count cycles, and then the test is pinning CPU instruction
  timing as much as video timing.
- **NONE** - nothing beam-locked reaches the CPU.

No board scored NONE.

### Why FIXED is worth having, and the thing the Williams design did not need

The Williams ROM had to get its verdict into a byte of RAM, because seven of its
ten assertions are CPU-observable. The two picture assertions, T6 and T7, are
not: the harness reads the framebuffer.

That second readout path is more capable than it looks. **The framebuffer is
itself a measurement of beam position.** If a program writes the palette or VRAM
promptly on a beam-locked interrupt, the row at which the picture changes says
which scanline the interrupt fired on, to within a row or two, and the harness
reads that row without the program needing to know anything. The uncertainty is
the interrupt latency, which is a small constant: on Galaga a Z80 interrupt
acknowledge plus a store is well under 192 cycles, so under one scanline.

That is why FIXED is a usable verdict. A board with an interrupt at line 64 can
carry a T6-style palette split and a T7-style "the beam has already passed"
test, keyed to line 64 rather than to a line of the program's choosing, and the
assertion is insensitive to a constant cycle offset in exactly the way the
Williams design demanded. What FIXED cannot do is the T1/T2/T3 family: measuring
where a signal's edges are, which needs a readable position.

## The table

| Board (machines) | CPU | Beam primitive | Resolution | Scratch not displayed | Verdict |
|---|---|---|---|---|---|
| `atari_system1` (**roadrunner**, marble) | M68000 @ 7.159 MHz (hardware is M68010) | motion-object timer entry fires IRQ3 at a chosen scanline, state readable at `$2E0000` bit 7; vblank level at `$F60000` bit 4 | 1 line | `$400000-$401FFF` (8 KB) | **FULL** on roadrunner, **FRAME** on marble (defect 3) |
| `mcr2` (shollow) | Z80 @ 2.496 MHz | Z80 CTC: readable down-counter, and a timer channel that interrupts N scanlines after a beam-locked trigger | 1 line | `$C000-$C7FF` (2 KB battery RAM) | **FULL** |
| `gridlee` (gridlee) | M6809 @ 1.25 MHz | IRQ at lines 64/128/192/256; FIRQ at ~92; vblank level at `$9700` bit 7 | 64 lines | `$0080-$07FF` (1920 B), `$9C00-$9CFF` NVRAM | **FIXED** |
| `namco_galaga` (galaga, digdug, xevious) | 3x Z80 @ 3.072 MHz, shared bus | sound-CPU NMI at lines 64 and 192; main/sub IRQ at line 224 | 128 lines | `$8800-$8B7F`, `$9000-$937F`, `$9800-$9B7F` | **FIXED** |
| `foodf` (foodf) | M68000 @ 6.048 MHz | IRQ1 at lines 0/64/128/192 (see defect 4); IRQ2 at line 224 | 64 lines | `$014000-$01BFFF` (32 KB) | **FIXED** |
| `btime` (burgertime) | M6502 (DECO CPU-7) @ 1.5 MHz | vblank level at `$4003` bit 7 | frame | `$0000-$07FF` (2 KB) | **FRAME** |
| `gottlieb` (qbert) | I8088 @ 5 MHz | NMI on vblank, line 240 | frame | `$0000-$2FFF` (12 KB incl. NVRAM) | **FRAME** |
| `mrdo` (mrdo) | Z80 @ 4.1 MHz | IRQ at line 224, once per frame, nothing readable | frame | `$E000-$EFFF` (4 KB) | **FRAME** |

Link addresses, all backed `ReadOnly` regions that `debug_write` will take:
`mcr2` `$0000` 48 KB; `gridlee` `$A000` 24 KB; `namco_galaga` `$0000` 16 KB per
CPU, selected by bus master; `foodf` `$000000` 64 KB; `atari_system1`
`$000000` 512 KB; `btime` `$B000` 20 KB; `gottlieb` `$6000` 40 KB; `mrdo`
`$0000` 32 KB. Reset vectors land inside the window in every case, including
Gottlieb, where the 8088's `$FFFF0` aliases to `$FFF0` because A16-A19 are not
decoded (MAME's `map.global_mask(0xffff)`, `gottlieb.cpp:1150`).

## Per board

### atari_system1 - FULL on Road Runner, FRAME on Marble

System 1 has the finest primitive of any board surveyed, and which of our two
System 1 games can use it depends on the cartridge.

An entry in the motion-object list flagged with `$FFFF` in its second word is a
timer rather than a sprite: IRQ3 fires at the top of that entry's band, held for
exactly one scanline, and the state is readable at `$2E0000` bit 7
(`atarisy1_v.cpp:407-449`, `atarisy1.cpp:408`, mirrored at
`atari_system1.rs:761-784, 1107`). The band position is
`(256 - (word0 >> 5) - vsize * 8 - 1) & $1FF`, so the program picks the line. That
is a programmable scanline interrupt with a poll path as well as an interrupt
path, at one-line resolution, which is finer than the Williams counter's four.
On top of it sits a live vblank level at `$F60000` bit 4
(`atari_system1.rs:736-749`) for frame synchronisation.

**Road Runner has that hardware; Marble Madness does not.** MAME splits the
driver in two over exactly this and says why at `atarisy1.cpp:2686`:

> atarisy1r_state is used because hardware to generate interrupt 3 only exists on
> LSI Cart 2, 3, 4 & cockpit boards, it is missing on TTL, LSI

`atarisy1_state::update_timers` is an empty function (`atarisy1_v.cpp:407-409`),
so the timer is never scheduled for the games that use that class, and every
`marble` set uses it (`atarisy1.cpp:2671-2675`). Every `roadrunn` set uses
`atarisy1r_state` (`atarisy1.cpp:2687-2689`), which schedules it. Our
`roadrunner` machine is registered against the
`roadrunn` set (`roadrunner.rs:873-878`) and shares `AtariSystem1Board`
(`roadrunner.rs:27, 555-556`), so the primitive is genuinely available to us on
one of the two games.

Our board file implements the timer unconditionally, which is right for Road
Runner and wrong for Marble. Filed as defect 3.

Two practical notes for a ROM here. The main program ROM window is
`$000000-$07FFFF` and is plain; the slapstic-banked window is a separate
`$080000-$087FFF` whose state machine is driven by data accesses
(`atari_system1.rs:786-796`), so a conformance ROM that never touches
`$080000` never perturbs it. And the motion-object list is snapshotted at the
start of vblank for rendering (`snapshot_motion_objects`, `:1135`) while the
timer interrupt reads live sprite RAM (`:762`), so a timer entry takes effect on
the interrupt in the same frame it is written but only affects the picture from
the next one. That asymmetry is itself worth an assertion.

### mcr2 - FULL, and the surprise of the survey

Satan's Hollow has no video counter and no vblank bit. It has a Z80 CTC at ports
`$F0-$F3`, fully readable and writable by the program
(`machines/src/satans_hollow.rs:374,388`), and that turns out to be enough:

- CTC channel 2 is triggered by the board at scanlines 0 and 240, channel 3 at
  scanline 0 (`mcr_m.cpp:107-126`, mirrored at `mcr2.rs:406-418`). Those are the
  beam-locked anchors.
- A CTC channel in timer mode counts the CPU clock through a divide-by-16 or
  divide-by-256 prescaler and a 1-256 time constant. The MCR clock tree makes
  256 CPU cycles exactly one scanline: one 19.968 MHz oscillator, Z80 at /8 and
  the pixel clock at /4, 512 dots per line (`mcr2.rs:43-65`,
  `MAIN_OSC_MCR_I` at `mcr.h:32`, `Z80CTC(config, m_ctc, MAIN_OSC_MCR_I/8)` at
  `mcr.cpp:1810`). So prescale 256 with time constant N interrupts exactly N
  scanlines after the channel is started.
- The Zilog CTC lets the CPU read a channel's down counter at any time, and our
  device implements that (`core/src/device/z80ctc.rs:72-75`). Started from the
  line-0 interrupt with prescale 256, that read *is* a vertical line counter.

So mcr2 supports both Williams primitives: `WaitLine N` by polling the down
counter, and a programmable scanline interrupt by arming a timer channel. At one
line rather than four, it is finer than Williams.

Two cautions. The phase of the counter within a line is set by the latency
between the line-0 trigger and the program's write of the time constant, which
is a small constant offset of the same kind Williams tolerated. And the board's
declared frame rate is wrong (defect 1), which does not affect the
CTC-to-scanline ratio but does mean an expectation stated in frames is standing
on sand.

mcr2's palette is CPU-writable inside video RAM (`$EF80-$EFFF`,
`mcr2.rs:384-390`), which is what W1 of `raster-sampling-fidelity.md` is about.
A conformance ROM here is directly W1's acceptance test.

### gridlee - FIXED, and the cheapest instrument to build

Gridlee is the only candidate whose picture tests could pass **today**: it
renders per scanline and latches its palette bank per scanline
(`gridlee.rs:565-574`). Every other board on this list renders once per frame,
so on all of them a T6/T7-style assertion fails by construction until the raster
work is done.

- Vblank level at `$9700` bit 7 (`gridlee.rs:922-930`, `gridlee.cpp:64`), which
  gives two edges a frame and identifies which IRQ is which.
- IRQ at scanlines 64, 128, 192 and 256, wrapping back to 64
  (`gridlee.cpp:107-120`, `gridlee.rs:576-580`).
- FIRQ at scanline 92, which the MAME header records as `FIRQ generated by ???
  (but should be around scanline 92)`. **Do not build an expectation on 92.**
  It is a guess in the reference, and pinning it would be pinning the guess.
- The palette is 64 PROM banks selected by a CPU write to `$9200`, latched per
  scanline, so a bank change mid-frame is a T6-style split with no palette RAM
  needed.
- Video RAM `$0800-$7FFF` is exactly the 256x240 bitmap at two pixels per byte,
  so all of it is displayed. Scratch is main RAM above the 32 sprite entries
  (`$0080-$07FF`, `gridlee.rs:773-788`) plus 256 bytes of NVRAM. Sprite graphics
  come from a GFX ROM that a bare board leaves blank, so nothing draws from the
  sprite list anyway.
- M6809 at `$A000`, the same CPU as Williams, so the source idioms and the
  `asl` build port over unchanged.

Note that the MAME header says the IRQ is "generated by 32L" while its code
fires every 64 lines. The two disagree and neither is a schematic. An
instrument's first job here would be to *measure* the cadence and the rows, by
having each IRQ handler mark a distinctive row, which is a thing nothing in the
tree does today.

### namco_galaga - FIXED, using a CPU that is not the main one

No CPU-readable beam state at all. The common memory map (`galaga.cpp:310-358`)
lists DIP switches, the 06XX custom I/O, work RAM, the tilemaps, the scroll
registers and, on Xevious, the background ROM readback. Nothing else.

What it does have is three Z80s on one bus. Addresses at and above `$4000` are
common to all three (`galaga.rs:941-943`), so the sound CPU can write video RAM,
and the sound CPU takes an NMI at scanlines 64 and 192
(`galaga.cpp:786-799`, `namco_galaga.rs:757-767`). Main and sub take a vblank
IRQ at line 224. Three beam-locked lines, on two different processors, and the
conformance ROM would supply all three images.

The palette is PROM-derived and not CPU-writable (`galaga.rs:483`), so there is
no palette-split test here. The mid-frame writable state is the tilemap, the
sprite registers, the starfield latches and, on Xevious, the scroll registers,
which is a T7-shaped test rather than a T6-shaped one.

### foodf - FIXED, on top of an unresolved reference

68000, four IRQ1s and one IRQ2 per frame, 32 KB of undisplayed work RAM, and a
write-only palette at `$950000`. Mechanically the easiest of the FIXED boards.

The problem is that the reference does not know where IRQ1 fires. MAME says so
in as many words (`foodf.cpp:326-329`):

> WARNING: the timing of this is not perfectly accurate; it should fire on 32V
> (i.e., on scanlines 32, 96, 160, and 224). However, due to the interrupt
> structure, it cannot fire at the same time as VBLANK. I have not solved this
> mystery yet

We copy MAME's fallback of 0/64/128/192 (`foodf.rs:778-781`). A conformance ROM
derived from our source would therefore pin a value the reference itself flags
as wrong. Filed as defect 4. Until that is settled, foodf is a board where the
instrument would be built on a known-unsound expectation, which is the exact trap
`williams-video-conformance.md` risk 1 describes.

### btime - FRAME, with an encryption tax

The main CPU takes no periodic interrupt at all. The driver header says so
(`btime.cpp:28`): "These games don't have VBLANK interrupts, but instead an IRQ
or NMI" on coin insertion, and the machine configuration confirms it, with no
`set_vblank_int` anywhere (`btime.cpp:2297-2307`).

The one beam-locked thing the main CPU can see is the live vblank bit injected
into DSW1 at `$4003` bit 7 (`btime.rs:859-860`). Two edges per frame.

The sound CPU sees more: its NMI is gated by scanline bit 3
(`btime.cpp:1028-1031`, `btime.rs:617-620`), which is a beam-locked event every
eight lines. It is on a separate address space with no path to video RAM, so it
cannot be used to place a picture event.

There is also a build cost specific to this board. Burger Time runs a DECO CPU-7,
which bit-permutes opcode fetches at addresses where `(addr & $0104) == $0104`
once the CPU has performed a write (`btime.rs:39-54`). A conformance ROM would
have to confine its code to addresses where that never holds, or pre-apply the
inverse permutation to exactly the bytes that will be fetched as opcodes, which
is a build step that has to know which bytes are opcodes. Between that and the
FRAME verdict, btime is the worst value on the list.

### gottlieb - FRAME

One NMI per frame at the start of vblank, line 240 (`gottlieb.cpp:2169`,
`qbert.rs:408-413`). No readable vblank bit and no other beam-locked input; the
input ports are DIPs, buttons and the trackball (`gottlieb.cpp:1161-1165`).

12 KB of undisplayed RAM and a CPU-writable 16-entry palette at `$5000-$501F`
make it mechanically easy, and it is the other half of W1. But with a single
anchor per frame, a palette-split test has to count cycles from the NMI, so the
row the split lands on is a function of our 8088 instruction timing as much as of
the video circuit. That is still a real test of "does a split happen at all, is
there exactly one, is it monotone", and it is not a test of where.

### mrdo - FRAME, and the least to work with

One vblank IRQ per frame at line 224 (`mrdo.cpp:226`, `mrdo.rs:108-110,594`).
Nothing readable: the map is ROM, video RAM, colour RAM, sprite RAM, two
write-only sound chips and the protection PAL readback at `$9803`
(`mrdo.cpp:101-105`). PROM palette, so no palette test. The mid-frame writable
state is the tilemaps and the scroll registers.

4 KB of work RAM at `$E000`, a Z80 at `$0000`, and a board that our own file
describes as taking "a single VBLANK IRQ with no mid-frame raster"
(`mrdo.rs:639`). A conformance ROM here would assert frame length and the IRQ
line, and very little else.

## Where this leaves the programme

Two facts frame the choice of a second board, and they cut across each other.

**Only gridlee can pass a picture test today.** All seven boards named by W3 are
render-once machines (`raster-sampling-fidelity.md`, "Current state"). On every
one of them a T6/T7-style assertion fails by construction until the per-scanline
work lands. That is not a reason to skip them, because a failing conformance
assertion is a specification, and W1/W2/W3 currently have no acceptance test
other than "golden frames unchanged". But it does mean that a ROM on any of them
lands red and stays red until other work finishes, which is a different kind of
artifact from the Williams suite.

**`hl4t.2` wants a second example that is not a sibling of the first.** It exists
to derive a shared contract from two examples rather than guess one from a single
example. Gridlee and Williams are near siblings: both M6809, both bitmap video
RAM, both already per-scanline, both from the same lineage of designers. A
contract derived from those two would be a contract for one kind of board
wearing a plural.

Three candidates, and each is the best answer to a different question.

| | `roadrunner` (atari_system1) | `shollow` (mcr2) | `gridlee` |
|---|---|---|---|
| Sync primitive | MO timer entry, 1 line, plus a poll path | CTC timer channel, 1 line, plus a readable line counter | interrupts at 4 fixed lines |
| Distance from Williams | M68000, `AddressSpace32`, tilemap plus motion objects, autovector levels | Z80, IM2 vectored, tilemap with a dirty-tile cache, palette inside video RAM | M6809, bitmap video RAM, near-identical |
| Picture tests on landing | fail until W3 | fail until W1 | pass |
| Serves an open raster issue | W3 (`6kae.3`, latched vs live sprites) | W1 (`6kae.1`, per-scanline palette) | none |
| Known defect underneath | `xvz6`, IRQ3 wrongly enabled on the sibling game | `1kk2`, frame rate | none found |
| Extra cost | slapstic window to avoid; the MO list is the primitive's own data structure | the frame-rate defect should be settled first | little |

`roadrunner` is the widest structural jump and the only one that would exercise
the 32-bit address space, so it is the strongest test of what the shared contract
really is. `shollow` is the primitive that surprised the survey and the board
whose open raster issue is furthest along. `gridlee` is the only one that
produces a green suite on the day it lands, and the cheapest by a wide margin,
and the weakest generalisation.

This is a fork the survey should not settle on its own. It is recorded on the
epic for a decision.

## Defects found while reading

Four, all filed, none fixed here. Three of the four are in code that no test in
the tree exercises for timing, which is the whole argument for the programme.

1. **mcr2 declares 36.93 Hz.** `2_496_000 / (256 * 264) = 36.93`
   (`mcr2.rs:32-41`, `core/src/core/machine.rs:69`). MAME declares 30 Hz for the
   same board and never states a raw pixel timing (`mcr.cpp:1821`). Our figure
   matches neither 30 nor 60, so the line count, the dot count, or both are
   wrong. The schematic is the arbiter.
2. **mcr2 does not cascade CTC channel 0 into channel 1.** MAME wires
   `zc_callback<0>` to `trg1` (`mcr.cpp:1812`); our board never reads
   `zc_output` (`mcr2.rs:406-418`). The device supports it
   (`z80ctc.rs:215-217`); the board does not use it.
3. **atari_system1 generates IRQ3 for both its games, and only Road Runner's
   cartridge has the hardware.** See the atari_system1 section above. The fix is
   a per-cartridge gate, not the removal of the feature.
4. **foodf's IRQ1 scanlines are a documented guess.** See the foodf section
   above.

## What this survey did not do

No code was written, no expectation was checked against a schematic, and no
board was probed by running it. Every "resolution" figure above is what the
primitive can express, not a measurement of what our implementation does with it.

The scope was the seven boards W3 names plus `gridlee`. Every other raster board
in the registry was left alone, and so was every vector board, on the grounds
that the programme wants an instrument where the raster work is, not a complete
census. Adding a board to this table is a morning's reading against the pattern
above, and the four defects it turned up suggest that morning is not wasted.
