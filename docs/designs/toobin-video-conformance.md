# Design: Toobin' Video Conformance ROM

A synthetic 68010 program, poked into a ROM-less Toobin' and run, that measures
the board it is running on and writes its verdict into work RAM.

Status: **the loader and the signals are in the tree and passing. The two
questions this was started for are not answered yet**, and the last section says
exactly what is left.

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
| 9 | complete, `$5A5A` written |

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

## What is left, which is everything this was started for

The instrument works. Neither jg18.2 question is answered yet, and both need the
same two things.

1. **Synthetic graphics.** A ROM-less board has no tiles at all, so every
   playfield, object and alpha pixel decodes to pen 0 and the compositor has
   nothing to draw. Road Runner's harness solved this by installing a synthetic
   font and tile set through the same entry points the real loader uses;
   `load_playfield_gfx`, `load_mo_gfx` and `load_alpha_gfx` are the equivalents
   here. Still no arcade ROMs, still CI-safe.
2. **The MAME second opinion.** Every expectation above is derived from the
   board's own geometry, which makes this a regression guard immediately and a
   correctness guard only where the derivation is independent of us. For the
   PAL there is no derivation available at all, so the comparison against
   something that is not us **is** the measurement.
   `tools/mame_roadrunner_conformance.lua` is the pattern: write the image into
   MAME's `maincpu` region, soft-reset so the 68010 re-fetches its vectors, and
   guard against the autoboot script re-running on its own reset.

Then the two questions, as picture phases:

- **The priority sweep.** The PAL's inputs are `LBPRI1:0`, `LBPIX3`, an
  object-transparent term, `ANPIX1:0`, `PFPIX3D` and `PFPRI1:0`. Toobin' never
  drives `LBPRI`, which is measured, so the live sweep is `PFPRI` (4) x `PFPIX3`
  (2) x `LBPIX3` (2) x object-transparent (2) x alpha pen (4): 128 cells, each
  recording the color index that comes out. Ours against MAME's, cell by cell.
  **Our compositor reads three of those inputs and ignores `LBPIX3` entirely**,
  which jg18.2 did not call out and which the sweep would settle either way.
- **The object sampling lead.** Place an object at a known Y and use the
  scanline interrupt, now known good to a fifth of a line, to change something
  the object's row depends on at a chosen line. Whether the change lands on that
  row or the one after it is the one-line delay, measured rather than assumed.

Both are tracked on jg18.2. Neither should be turned into a code change until
the MAME half exists, because for the PAL our own answer is the thing under
test.

## References

- `docs/designs/roadrunner-video-conformance.md`, the pattern this follows
- `docs/designs/williams-video-conformance.md`, the proof of concept
- `docs/designs/conformance-rom-board-survey.md`
- `phosphor-emulator-jg18.2`, the two questions
- `phosphor-emulator-conformance-rom-programme-hl4t`, the programme
