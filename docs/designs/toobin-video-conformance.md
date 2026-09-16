# Design: Toobin' Video Conformance ROM

A synthetic 68010 program, poked into a ROM-less Toobin' and run, that measures
the board it is running on and writes its verdict into work RAM.

Status: **the loader, the signals and the layer-priority sweep are in the tree
and passing.** The sweep documents the compositor's behavior across all 96
combinations of the PAL's live inputs and found nothing wrong, which was the
expected outcome, and the object sampling lead is measured at zero rows against
a same-instant playfield control. Both jg18.2 questions now have numbers. The
last section says what is left and what any of it can be worth without a PAL
dump.

## Why this board

`phosphor-emulator-jg18.2` has two open questions about Toobin's video, and both
are explicitly not settleable by reading the schematic harder or by guessing:

- **The layer-priority PAL.** Sheet 15 puts the decision in a 16L8A at 7E whose
  inputs are known and whose equations are not on the sheet, because a PAL is a
  programmed part. The ROM set carries no PAL dump, so MAME's behavior is
  reverse-engineered rather than read off silicon. Our compositor uses three of
  the PAL's nine inputs and ignores `LBPIX3` entirely.
- **The object sampling lead.** Sheet 13 establishes that the two line buffers
  trade every scanline, so what the beam shows on a line was scanned during the
  line before it. Whether our placement already absorbs that one-line delay
  could not be pinned down at the resolution the sheet was read at, and
  `machines/CLAUDE.md` warns that adding the delay a second time moves every
  object pixel the wrong way.

A golden frame cannot tell a right answer from a wrong one for either. That is
what a conformance ROM is for.

## What this board adds to the programme

`phosphor-emulator-conformance-rom-programme-hl4t.2` wants a shared harness
contract derived from more than one example. Two boards were not enough to fix
what generalizes, and this is the third:

| Axis | Williams | Road Runner | Toobin' |
|---|---|---|---|
| Main CPU | M6809 | M68010 | M68010 |
| Beam position | 4-line counter register | VBLANK level + programmable IRQ | VBLANK level + HBLANK level + programmable IRQ |
| Interrupt ack | PIA read | write to a latch, or none | write to a latch, always |
| Watchdog | counts, nothing acts | reboots after 8 frames | reboots after 8 frames |
| Scratch | undisplayed video RAM | 8 KB work RAM | 16 KB work RAM |

The interesting row is the second. **Toobin' is the first board in the programme
with a beam signal that moves faster than the loop reading it**, and that turned
out to be the whole difficulty of the exercise. See *T2* below.

## Mechanism: no new plumbing

Exactly the Road Runner mechanism, which is the first thing this board confirms
generalizes. `AddressSpace32::debug_poke` writes straight to the backing store
with no `AccessKind` check and `ReadOnly` regions are allocated backing, so a
test can patch program ROM on a machine built with no ROM set at all;
`M68000::reset` fetches the supervisor stack pointer from `000000` and the
program counter from `000004` **through the bus**, so both come from the poked
image.

| Property | Where |
|---|---|
| Program ROM at `000000-07FFFF` is a backed `ReadOnly` region | `toobin.rs:626-632` |
| `debug_poke` ignores `AccessKind`, `ReadOnly` regions take backing | `address_space32.rs` |
| `M68000::reset` fetches vectors 0 and 1 through the bus | `m68000/mod.rs` |
| `ToobinBoard::reset` does not clear map backing | `toobin.rs` |

### One address wrinkle, and it is load-bearing for the MAME half

The board decodes only A23, A22 and A18-A16 above A15, so `ToobinBoard::mask_addr`
folds every access with `& 0x00C7FFFF`. The CPU's `FFC000` therefore lands in the
work-RAM region whose base *in masked space* is `C7C000`.

**The ROM uses the address the real game uses and the harness peeks the region
base.** That is not an inconsistency to tidy: the program has to run under MAME
too, whose map decodes `FFC000` and knows nothing about our masked space.
`the_roms_work_ram_address_and_the_debug_buss_are_the_same_place` asserts the
fold rather than leaving it as a comment.

## The ROM

`machines/tests/roms/toobin_video.asm`, assembled with `asl` and `p2bin` from
the Nix dev shell into a flat 8 KB image at `000000`. The `$A5` fill byte is
inherited from Road Runner for both its reasons: a zero fill would make the
checksum blind over the padding, and `$A5A5` decodes as line-A so a runaway
program counter vectors to the stray handler.

| Phase | Meaning |
|---|---|
| 1 | entry: stack set up, result block cleared, reset SSP recorded |
| 2 | the CPU has checksummed the whole 8 KB image through the real bus |
| 3 | the first vblank edge has been seen |
| 4 | `VB_TARGET` vblank edges seen, with the watchdog strobed at each |
| 5 | T1, the VBLANK level and the calibration everything else divides by |
| 6 | T2, the HBLANK level and its share of a line |
| 7 | T3, the scanline interrupt at line 100 |
| 8 | T4, the same at line 300 |
| 9 | the layer-priority sweep is painted and a frame composited with it |
| 10 | the object sampling lead probe is armed and repeating once a frame |
| 11 | complete, `$5A5A` written |

Every wait polls hardware state, never a cycle count, and every position is
counted in iterations of one shared poll loop whose rate T1 measures in the same
run. Nothing is compared against a constant that was measured once and written
down.

### Two things this board makes the program do that Road Runner did not

**The scanline interrupt has to be acked before it is armed, and after the wait
rather than before it.** `interrupt_scan` comes out of reset at 0, so the latch
is set at scanline 0 of every frame from power-on; a program that lowered its
mask without clearing it would take an IRQ1 it never asked for. Worse, arming
the target line and *then* waiting a frame for the origin means the beam crosses
that line during the wait, so the latch is already standing when the mask comes
down. The handler then runs before the polling loop completes one iteration and
records position zero. Every scanline position read exactly `0.00` lines, which
looks like the interrupt never arriving rather than like it arriving too early.

**The sound board has to be quieted and its levels handled rather than left
stray.** A bare board's sound 6502 has no ROM at all, so what it executes is
whatever an empty map returns, and it must not be able to raise level 2 into the
middle of a measurement. The program strobes the sound reset and drains the
response latch at entry; levels 2 and 3 get handlers that count and drain rather
than trap, because this ROM should *report* what a ROM-less sound board does
rather than assert it. `R_SND` carries the count out and is asserted zero.

## Measurements

All on a ROM-less `toobin`, 22 frames to completion.

| Field | Derived | Measured |
|---|---|---|
| `R_SSP` | `$00FFDF00` | `$00FFDF00` |
| `R_CKSUM` | sum of the committed image | equal to the harness's sum |
| `R_VBCOUNT` | 16 | 16 |
| `R_SND` | 0 | 0 |
| vertical blank | 32 lines | 31.70 |
| ... a frame later | the same | identical, 169 iterations both times |
| horizontal blank's share of a line | 0.2000 | 0.2082 |
| scanline interrupt, line 100 | 132.00 lines past the vblank edge | 131.31 |
| scanline interrupt, line 300 | 332.00 | 331.47 |
| the move between them | 200 | 200.16 |

The calibration is 5.331 iterations per scanline, so the vertical figures have a
resolution of about a fifth of a line; nothing asserts against that number and
it is recorded only so a reader can see what the tolerances mean.

### T2, which is the part worth reading

The horizontal blank is 64 of a line's 320 CPU cycles. **It took three loops to
measure it, and the first two both produced plausible wrong answers.**

- **Built from `WaitSet`/`WaitClear` like every other phase**, the `bsr` and
  `rts` around each wait cost more time than the thing being measured. It read
  3.0 iterations per line where T1 read 5.3, and the missing 2.3 was call
  overhead elapsing without the counter advancing.
- **Inlined, but still counting into memory and checking a bound**, the loop ran
  at 4.375 iterations per line. 4.375 is 35/8, so the sampling phase repeated
  exactly every 8 lines instead of drifting, and the blank read exactly ONE
  sample on all 64 lines. The resulting share, 1/4.375 = 0.229, looks like a
  plausible answer for something that is really 0.200 and is not a measurement
  at all: **it is the loop period, and it would have read the same on a board
  whose blank was half as wide.**
- **Four instructions, counting in a register, with `dbne`/`dbeq` doing the wait
  and the bound together**, the loop samples about 13 times a line and 2.6 times
  inside the blank, which resolves it.

Then one more error on top, and it is the subtle one: `DBcc` does not decrement
on the iteration where its condition comes true, so using its register as the
sample count skips exactly the sample that detected each edge, two per line. The
blank read 15.6% against 20%, and every point of that deficit was the two
uncounted samples rather than anything the board did. Counting at the top of
each loop instead makes the detecting sample count, and the `+1` at the blank's
start and the `-1` at its end then cancel exactly, with no correction anywhere.

**The general lesson, which is why this is in the design doc rather than only in
a comment: a polling instrument reports its own period when the thing it is
measuring is near its resolution, and it does so without any sign of distress.**
Both wrong versions produced a stable number, repeatable across runs, in the
right order of magnitude. What exposed the second one was not a failing
assertion but noticing that 64 blanks over 64 lines had produced exactly 64
samples, which is the signature of a lock rather than of sampling. A tolerance
wide enough to accept 0.229 would have shipped an instrument that could not see
its subject, and the test now holds 1.5 points specifically so that it cannot.

The same failure shape appeared a third time, in T3: a bespoke polling loop
counted at a different rate from the calibration loop, because `tst.w` on an
absolute long address plus a far branch is not the same cost as `move.w (a0),d0`
and `and.w d1,d0` with a short one. Dividing one loop's count by another loop's
rate read every scanline position 6% low. The fix is structural rather than
numerical: the handler's own counter is what gets polled, through the same
`WaitSet` the calibration was measured with, so the count is on T1's scale by
construction.

## The layer-priority sweep

Phase 9 paints 96 test cells, twelve across and eight down as 16x16 blocks at
the screen origin, one per combination of the four live PAL inputs:

    cell = ((object * 4 + PFPRI) * 2 + PFPIX3) * 4 + ANPIX

`object` is 0 transparent, 1 opaque with pen bit 3 clear, 2 with it set. The
fifth input, the two object priority bits, is not swept: the game never drives
it, measured over 1507 list entries, so sweeping it would sweep our own code.

**`PFPRI` is swept over all four values even though Toobin' only ever uses 0 and
2.** That is the whole point of driving the board rather than watching it.

### Synthetic graphics, and what the encoder does not prove

A ROM-less board has no tiles, so every pixel decodes to pen 0. The harness
builds three solid-pen tile sets with an encoder that is **the exact inverse of
`decode_gfx`**, walking the same plane, x and y offsets and setting the bit where
the decoder reads it.

That guarantees pixel `(x, y)` of tile `N` decodes to the pen asked for, which is
the only property the sweep needs, because what is under test is the
*compositor*. It cannot catch an error in `decode_gfx` or in the three layout
constants, since encoding and decoding would cancel. Those are pinned by the
golden frame against the real ROM set. Writing the bytes out by hand would not
fix that and would add a second place for the layout to drift from.

### The readout is the color RAM address, not a color

The palette is loaded with an identity code rather than with colors: entry `i`
carries the low five bits of `i` in red and the next five in green, with bit 15
set so the intensity control cannot scale it. A rendered pixel therefore hands
back **which palette index the compositor chose**, and that is exactly what this
PAL decides: its outputs drive the multiplexers that pick one layer's color and
pen as the color RAM address. A test that only asked "is the object on top" would
read back less than the hardware decides. The layer is the range: playfield below
`0x100`, object `0x100-0x1FF`, alpha above.

The component scaling is `(c * 224) >> 5` with a 38 pedestal on everything but
zero, so the 32 steps land on 0 and then 45 to 255 in sevens. Injective, which
makes the inverse exact rather than a nearest match, and a test checks the round
trip for all 1024 indices before any of it reads a picture.

### Two vblank edges, not one

The cell loop takes about half a frame, and this board composites each row at its
own scanline boundary, so the CPU is drawing while the beam is reading.

The first attempt waited one vblank edge and produced a result that did not look
like a race at all: blocks 81, 82 and 83 came out blank, 84, 85 and 86 were
correct, and 87 through 95 were blank again. A partial draw leaves a blank
*suffix*; a hole in the middle is the two orders crossing, because the drawing
runs left to right through the cell index while the beam runs top to bottom
through the rows, and row 7 is composited sixteen scanlines after row 6. Cells
drawn before their own row was composited survived; the rest did not.

Two edges fix it. The first ends the raced frame; the second returns after a
frame composited from scanline 0 with every block already standing.

### What it found

Nothing wrong, which was the expected outcome and is worth stating as a result
rather than as an absence. All 96 cells match the shipped merge rule. The
behavior it documents, for the 24 cells where the alpha is transparent and the
decision is therefore visible:

| object | PFPIX3 | PFPRI 0 | 1 | 2 | 3 |
|---|---|---|---|---|---|
| transparent | 0 | playfield | playfield | playfield | playfield |
| transparent | 1 | playfield | playfield | playfield | playfield |
| opaque | 0 | object | object | object | object |
| opaque | 1 | object | playfield | playfield | playfield |

The other 72 cells are the alpha winning, in every one of them.

Three things that are now measured rather than assumed:

- **`LBPIX3` changes the pen and never the layer.** Every pair of cells differing
  only in the object pen's bit 3 selected the same layer. The shipped rule
  ignores that PAL input and the sweep says so out loud.
- **An opaque alpha pen wins everywhere**, over both object pens and all four
  playfield priorities. `ANPIX` does nothing here beyond its own transparency,
  which was the likelier of the two readings in `jg18.2` and is now the recorded
  one.
- **Priority 1 and 3 behave as 2 does**, which no measurement over the game could
  have said, because the game never produces them.

**This is a regression guard, not a correctness guard.** Every expectation is
derived from our own merge rule. It becomes a correctness guard only against an
oracle that is not us, and for this board there is no good one: the 16L8A at 7E
was never dumped in any of the three ROM sets, so MAME's priority is somebody's
reverse engineering of the same sheet we have rather than the part's contents.
What the sweep adds today is that the behavior is written down at a resolution
the game cannot reach, in a form a reader can check cell by cell.

## The harness

`machines/tests/toobin_video_timing_test.rs`, ROM-less, one machine, in CI.
Fourteen tests, one property each, every one calling `assert_completed` first so
a wedge fails on the magic word rather than on a handful of assertions about
zeroes. `R_TIMEOUT` and `R_TRAP` separate "a stage stalled" from "the program
took an exception" from "the loader never worked", which want different next
steps and would otherwise all present as a zero block.

The drift guard re-assembles the source and byte-compares, and fails rather than
skips when `PHOSPHOR_ASM` is set, which the dev shell exports. CI has no dev
shell and skips with a printed note.

## The object sampling lead

Phase 10 is the second jg18.2 question, and the design turns on one choice.

**It measures a latency, not a position.** Asking "is this object on the right
row" can only be answered against an oracle, and our own answer is the thing
under test, so a conformance ROM cannot ask it. Asking "how many rows after a
write to the list does the change appear" is answerable here without one, and it
is the same quantity: a path that scans a line ahead cannot show a change on the
very next line, because that line was already scanned.

One scanline interrupt at line 240 makes two writes as close together as a
handler can put them: one to the object list, changing a 128-row-tall object's
tile, and one to the playfield map, changing two cells of a painted column. The
playfield is the control. It has no line buffer, so the *difference* between the
two answers is the object path's lead, and a shared answer is the handler's own
latency turning up in both rather than anything about a line buffer. A single
probe could not tell those apart.

The writes are undone at every vertical blank, so the transition happens once a
frame forever and the harness can read any frame. Left one-shot, the picture
would carry the changed state from the second frame on and there would be no edge
to find.

| | measured |
|---|---|
| write made during row | 240 |
| playfield change first visible | row 241 |
| object change first visible | row 241 |
| **object path's lead over the playfield's** | **0 rows** |

Both reach the very next row, which is the earliest possible: the interrupt latch
is set at the start of its line and that line is composited immediately, before
the CPU runs, so the handler cannot reach the line it fired on.

**So this renderer applies no lead at all.** It reads the object list live at the
row it is drawing, exactly as it reads the playfield map. That is now a measured
fact rather than an inference from reading `draw_motion_objects_row`, and it is
the number a MAME run would be compared against.

**It does not say what the board does.** Sheet 13 establishes the two line
buffers are real and trade every scanline. Whether sheet 7's vertical match
constant already absorbs them is still open and still needs an oracle. What has
changed is that the question now has a shape a second implementation can answer
in one number, instead of a shape that required reading a schematic at a
resolution nobody had.

The control earned its place immediately. Its first run reported the playfield
changing at row 200 for a write at row 240, which is impossible and which was an
address bug: `LEAD_PFTOP` is a cell row and the paint loop had been given the
sweep's stride, which counts 16-pixel blocks and is therefore twice as long. The
band landed at cell row 50, off the bottom of a 48-row screen, and the column
read the cleared map. A probe without a control would have reported the object's
241 on its own and looked entirely convincing.

## What is left

The instrument works, the graphics are in, the priority sweep runs and the lead
is measured. One thing remains.

1. **The MAME second opinion.** Everything the sweep asserts is derived from our
   own merge rule, so it guards against regression and not against being wrong.
   `tools/mame_roadrunner_conformance.lua` is the pattern: write the image into
   MAME's `maincpu` region, soft-reset so the 68010 re-fetches its vectors, and
   guard against the autoboot script re-running on its own reset. The ROM was
   built for this, which is why it addresses `FFC000` and `FF8000` rather than
   the masked space our debug bus uses.

**Be honest about what that second opinion can be worth here.** There is no PAL
dump in any of the three ROM sets, so MAME's rule is a reverse engineering of the
same sheet we have. Agreement means two independent readings concur, which is
real evidence and is not verification; disagreement means one of us is wrong and
the picture says which. The only thing that would be ground truth is a dump of
the 7E PAL, and it does not exist.

**And the stakes are small, which was measured before any of this was built.**
Over 3000 frames of recorded play, mutating the merge rule and counting changed
pixels gives:

| candidate | pixels changed | share |
|---|---|---|
| control: `PFPRI >= 2` instead of `!= 0` | 0 | 0.00000% |
| `LBPIX3` set forces the object in front | 26,057 | 0.00442% |
| `LBPIX3` set keeps the object behind | 6,770,082 | 1.14781% |
| the object beats the alpha | 919,221 | 0.15585% |

The control coming out at exactly zero is what makes the rest trustworthy, and it
also closes an axis: since `PFPRI` only ever takes 0 and 2, any rule that
distinguishes priority 1 from 2 is unobservable on this game. The whole
playfield-priority mechanism touches 0.016% of pixels and `LBPIX3` 0.004%, about
nine pixels a frame. The third row is excluded by inspection rather than by
measurement: 1.15% would delete half of every sprite over priority playfield, and
the picture is not that.

So this is not a defect hiding in plain sight, and the sweep should be read as
documentation of behavior rather than as a bug hunt. One caveat worth keeping: a
small pixel count is not the same as an invisible one, and nobody has checked
whether those 26,057 pixels cluster on one object's outline or scatter.

## References

- `docs/designs/roadrunner-video-conformance.md`, the pattern this follows
- `docs/designs/williams-video-conformance.md`, the proof of concept
- `docs/designs/conformance-rom-board-survey.md`
- `phosphor-emulator-jg18.2`, the two questions
- `phosphor-emulator-conformance-rom-programme-hl4t`, the programme
