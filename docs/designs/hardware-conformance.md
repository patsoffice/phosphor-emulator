# Design: Running the conformance ROMs on real hardware

> **Status: proposed.** Tracks `phosphor-emulator-9sr9`.
> Nothing here is built. The ROMs exist and run under our emulator and under
> MAME; this is about the third reader.

## Context

Two conformance ROMs exist: `machines/tests/roms/williams_video.asm` and
`roadrunner_video.asm`, described in
[`williams-video-conformance.md`](williams-video-conformance.md) and
[`roadrunner-video-conformance.md`](roadrunner-video-conformance.md). Each runs
under our emulator with no arcade ROMs, and each is cross-checked against MAME.

Every expectation in them is derived from our own board files or, increasingly,
from a schematic. That makes them a regression guard immediately and a
correctness guard only against another *model*. MAME is a second model, not an
authority: this year it has been wrong twice where we were right (the
motion-object special mask it declares and never installs,
`phosphor-emulator-h52k`; the video counter it saturates where the schematic
says it rolls over) and right once where we were wrong (the slow blit).

Real hardware is the only reader that is not a model.

## What is already true, and why this is close

The ROMs were written to need no host, and that was not for this reason, but it
is what makes this cheap:

- **They synchronise on hardware state, never on a cycle count.** Williams polls
  the video counter at `$CB00`; Road Runner polls the VBLANK level and the
  motion-object timer interrupt. A board with different cycle timings still
  lands on the same beam positions.
- **They strobe the watchdog.** Williams writes `$39` to `$CBFF`, which is what
  MAME's `williams_m.cpp` requires and therefore what the hardware requires.
  Road Runner strobes `880001` from its first instruction because that board's
  watchdog bites at 8 frames.
- **They clear their result block at entry**, so a wedged or rebooting program
  reads as a wedge rather than as a pass.
- **They assume nothing about a cold machine**, which two rounds of MAME work
  forced: the sound-latch edge, palette RAM, the motion-object list, and the
  PIA's stale interrupt flags are all initialised rather than inherited.

What they do **not** have is a way to say anything without a debugger attached.
Our harness reads the result block through `BusDebug`; the Lua scripts read it
through MAME's memory interface. On a real board there is neither.

So the work is an output channel and an operator loop. It is not new tests.

## Design

### The screen is the output channel

The verdict is 22 bytes on Williams and about 21 words on Road Runner. Render it
as hex on the display, expected beside actual, and let the operator photograph
the screen.

**Show measured numbers, not `PASS`.** A verdict that renders identically
whether or not the test ran is the failure mode this repository keeps finding,
and it would be worse here than anywhere: nobody is going to re-run a cabinet
test because a green screen looked suspicious. The raw figures are also the
interesting part, because the reason to make the trip is the handful of
questions where our derivation is uncertain, and those want a number.

Per board:

- **Williams** is a bitmap board with no character generator, so the ROM carries
  its own font: sixteen hex glyphs, 8x8, about 128 bytes, blitted per digit. The
  ROM already drives the blitter for its own test patterns, so this is a table
  and a small routine.
- **Road Runner** has an alpha layer and, on real hardware, the cartridge's real
  font ROM. Our CI-safe suite installs a synthetic font precisely because a bare
  board has none, but a hardware run has the genuine article, so text costs a
  string and a loop.

### The operator loop

Poll the board's inputs for a button and use it to page through results and to
re-run a test. Williams reads player inputs on PIA 0 at `$C804`; Road Runner has
its own input port. Both are already documented in the board files.

A test that can be re-run on demand matters more than it sounds: several of the
open questions are about a value that should be *stable*, and watching it stay
put across twenty re-runs at a cabinet is evidence a single capture cannot give.

### What must not change

**The result block stays byte-identical**, and the display code sits behind an
assembly-time flag the way `ROMBASE` already switches Williams' link address.
One source, one drift guard, and the CI-safe suites and MAME cross-checks are
untouched. A hardware build that diverged from the tested build would be
answering questions about a program nothing else runs.

## What a hardware run is for

The point is not to re-run assertions that already pass in two emulators. It is
to settle the things neither can. Ranked by whether the trip can actually answer
them:

| Question | How hardware answers it |
|---|---|
| **Does the Atari System 1 motion-object renderer draw a `$FFFF` entry?** (`phosphor-emulator-k3i6`) | Put a timer entry at X 0 and **look at the screen**. An 8x8 block is there or it is not. No instrumentation at all: the screen is the instrument. This is the cheapest and most decisive item on the list. |
| **Does the Williams video counter alias or saturate above line 255?** | The dwell ratio comes back about 2 or about 1. Derived from the schematic in [`../schematics/williams-video-counter.md`](../schematics/williams-video-counter.md); hardware is the arbiter. |
| **Is Road Runner's sound reset driven by the level of `860001` bit 7 or by its edge?** | Hold with a level write and see whether the sound CPU is actually held. Recorded as a real divergence from MAME and never settled. |
| **Is the Williams scanline count 260?** | Counter wraps against a known number of E clocks. See the dot-clock derivation. |

And one that must be struck from the list rather than carried along:

- **The Williams scanline-256 CB1 guard.** The design doc already establishes
  it is not CPU-observable. No conformance ROM can see it, on hardware or
  anywhere else, and pretending a cabinet trip could settle it would waste the
  trip.

## Costs and risks

- **Burning EPROMs.** Williams is 12 KB across three 2732s or one 27128; Road
  Runner's image replaces the motherboard BIOS rather than the cartridge.
  Ordinary work for anyone with a programmer, and the largest practical barrier
  is simply owning the board.
- **The Atari System 1 slapstic.** The conformance image sits in the motherboard
  BIOS region and the program never touches `080000-087FFF`, so it should not
  engage the slapstic at all. "Should" is doing work in that sentence and it
  wants checking before anyone burns anything.
- **The display path is under test when the display is the output.** For the
  timing assertions that is fine, because they are measured into RAM before
  anything is drawn. For the picture questions it is not a problem but a
  feature: the screen is exactly the thing being asked about.
- **A CRT is a hostile readout.** Misconvergence, overscan and a phone camera
  all argue for large glyphs, hex rather than decimal, and no reliance on
  colour to carry meaning.
- **One person, one board, no CI.** This will never be automated and should not
  pretend to be. The output is a photograph and a note in an issue.

## What this does not cover

Anything needing a logic analyser. If a question needs to observe a signal the
CPU cannot read, a conformance ROM is the wrong instrument no matter what
machine it runs on; the CB1 guard above is the worked example.

It also does not cover audio, input hardware, or anything about the cabinet.

## Sequencing

Filed under `phosphor-emulator-9sr9`:

1. `9sr9.1` confirm the Road Runner image is slapstic-safe, because it gates the
   item with the highest value.
2. `9sr9.2` Williams on-screen results and button paging, verified under our
   emulator and MAME by screenshot before anyone burns a ROM.
3. `9sr9.3` Road Runner the same, using the cartridge font.

The order is deliberate: (1) is research and cheap, and (2) is the board we have
schematics for and the simplest display path.
