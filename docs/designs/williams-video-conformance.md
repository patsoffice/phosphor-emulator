# Design: Williams Video Timing Conformance ROM

> **Status: landed**, as `phosphor-emulator-williams-video-conformance-itvk`
> under `phosphor-emulator-conformance-rom-programme-hl4t`. Successor in spirit
> to [`frame-regression.md`](frame-regression.md), and a prerequisite instrument
> for [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md) W3.
>
> The ROM is at `machines/tests/roms/williams_video.asm`, assembled to
> `williams_video.bin` beside it, and the harness is
> `machines/tests/williams_video_timing_test.rs`. Every figure in *Result block*
> below has been **measured** on joust and robotron unless marked otherwise;
> where a measurement corrected the design, this document says so rather than
> being quietly rewritten.
>
> Two things this document predicted and got right, and one it got wrong:
> the slow-blit bug is real and measured (`itvk.4`); T7 discriminates, and was
> shown to fail under a deliberately wrong render model. What it missed is that
> phase 9 and phase 10 were published in the same frame, so capture C could
> never be taken (`itvk.2`, fixed).

## Context

Nothing in this repository tests *when* a machine draws. `boot_check_test.rs`
asserts a machine lights a pixel. `golden_frame_test.rs` pins a hash of a frame
produced by an attract loop — and cannot distinguish "we draw the picture
differently" from "the machine is in a different state", which is why its one
recapture to date (`b22534b`, three vector machines) produced diffs that were an
animation phase and a different attract level rather than a rendering change.
The ~211 render-adjacent unit tests all exercise a renderer with state handed to
it directly; none of them let the CPU and the beam run against each other.

For Williams that gap is exactly where the bugs were. Two commits, two days
apart, were both needed to make Joust draw correctly:

- `3eb7ce1` (2026-02-14) — *"Fix video timing: VA11/count240 scanline signals to
  ROM PIA CB1/CA1, 264-line frame"*
- `8df2e35` (2026-02-16) — *"Move Joust video rendering to per-scanline during
  run_frame()"*

The coupling is still explicit in the code. `WilliamsBoard::begin_scanline`
(`machines/src/williams.rs:648`) renders line N **before** the CPU runs line N's
cycles, then drives the ROM PIA's CB1 from VA11 and CA1 from count240. The
blitter writes VRAM in response to a PIA interrupt that fires at a beam-relative
line, and the renderer samples VRAM at a beam-relative line. Two phase-locked
moving parts, and no test in the tree observes either of them.

### Why not a state-injection fixture

The obvious cheap idea — dump video state from MAME, inject it, render, compare
— does not work here. `WilliamsBoard::render_frame` (`williams.rs:793`) is
`buffer.copy_from_slice(&self.scanline_buffer)`. The picture was built line by
line during `run_frame`; the renderer's domain *is* the finished framebuffer, so
injecting it and comparing proves nothing.

(That technique remains correct for the nine render-once machines in
`raster-sampling-fidelity.md` — including Marble Madness, whose mid-frame
behaviour is already reified as `mo_shadow` plus
`mo_bank_changes: Vec<(u16, u8)>`. It is a separate piece of work.)

### Why not a MAME frame comparison

Cycle divergence. Where our M6809 has been measured against modern MAME (ESB
boot, 454,189 instructions, `oyxg` notes) the agreement is exact per instruction
but carries a **constant offset** from our reset sequence. Where the two cores
disagree — indexed `[n]` extended indirect, +5 cycles per the MC6809E datasheet
against the reference core's +8 — the datasheet wins per `CLAUDE.md`, so **we**
are the correct side. A timing oracle built on MAME would have scored that
backwards. Anything indexed by cycles, and anything that runs long enough for an
RNG to move, cannot be compared.

### What a conformance ROM gives that nothing else does

1. **The expected output is derivable**, from the schematic and the datasheet,
   rather than borrowed from another emulator.
2. **It self-synchronises to the beam**, not to a cycle count, by polling the
   video counter at `$CB00`. A constant cycle offset between two
   implementations cancels; a delay loop would not survive it.
3. **Most of what it asserts is CPU-observable**, so the verdict is a byte in
   RAM — no PNG, no fixture, no image diff, no MAME.
4. **It is our own original code**, so the binary commits to the repository and
   the test runs in CI. Every video test in this tree is ROM-gated today, and CI
   has never once checked what a machine draws.
5. **One ROM covers three games.** Joust, Robotron and Sinistar share
   `WilliamsBoard`.

## Mechanism: no new plumbing

`AddressSpace16::debug_write` (`core/src/core/address_space16.rs:509`) writes
straight to the backing store with no `AccessKind` check, and `ReadOnly` regions
are allocated backing (`:391`). So a test can patch program ROM on a machine
built with **no ROM set at all**:

```rust
let entry = registry::find("joust").unwrap();
let mut m = (entry.create_bare)();                       // no ROMs — CI-safe
{
    let bus = m.debug_bus_mut().unwrap();
    for (i, b) in PROGRAM.iter().enumerate() {           // 12 KB image
        bus.write(0, 0xD000 + i as u32, *b);             // includes vectors at $FFF0-$FFFF
    }
}
m.reset();                                               // M6809 fetches $FFFE through the bus
```

Four properties of the bare board make this work, all verified in the tree:

| Property | Where |
|---|---|
| Program ROM at `$D000-$FFFF` is a backed `ReadOnly` region | `williams.rs:473` |
| `debug_write` ignores `AccessKind`, and `ReadOnly` regions are allocated backing | `address_space16.rs:509`, `:389` |
| `reset()` sets `clock = 0`, so frame 0 starts at scanline 0, and does **not** clear ROM or VRAM backing | `williams.rs:767-789` |
| The decoder PROMs are loaded and discarded, so a blank set costs nothing | `joust.rs:341` |

All four held. The program reaches its final phase and writes its magic byte on
both joust and robotron built with `create_bare` and no ROM set at all.

The M6809 fetches its reset vector through the bus (noted in `a6b5ac4` as the
reason Williams needed the generic `Cpu::reset`), so the patched vector is what
it takes.

The watchdog counter at `williams.rs:745` increments but never triggers a reset,
so the ROM does not have to pet it. It does anyway, so the identical binary
behaves on hardware and in MAME.

## Board facts the tests are derived from

All from `machines/src/williams.rs` and `core/src/device/{pia6820,williams_blitter}.rs`.

| Quantity | Value | Source |
|---|---|---|
| Main CPU | M6809 @ 1 MHz | `TIMING`, `:59` |
| Frame | 260 lines × 64 cycles = 16,640 cycles (60.096 Hz) | `TIMING` |
| Visible scanlines | 7–246 (`CROP_Y = 7`), 240 rows | `:602`, `:655` |
| Framebuffer | 292 × 240 RGB24, `Orientation::NORMAL` | `TIMING`, `joust.rs` has no `orientation` flag |
| VRAM | `$0000-$BFFF`, column-major: `addr = column*256 + scanline` | `:632` |
| Displayed columns | 3 … 148 (`FIRST_COL = CROP_X/2 = 3`, 146 columns) | `:628` |
| Pixel packing | 2 pixels/byte, high nibble = left | `:625-638` |
| Palette | `$C000-$C00F`, `BBGGGRRR` | `:607-618` |
| ROM PIA | `$C80C-$C80F` = PRA/DDRA, CRA, PRB/DDRB, CRB | `:995`, `:1089` |
| ROM PIA IRQ is the only one wired to the main CPU | `irq_a() \|\| irq_b()`, FIRQ unused | `:1137-1147` |
| CB1 ← VA11 | `(scanline & 0x20) != 0`, **not updated on scanline 256** | `:659-661` |
| CA1 ← count240 | `scanline >= 240` | `:664` |
| Video counter | `$CB00` returns `current_scanline() & 0xFC` | `:1000`, `:593` |
| Watchdog | `$CBFF`, cleared by writing `$39`; the counter increments and nothing acts on it | `:1119`, `:762` |
| Blitter | `$CA00-$CA07`; write to offset 0 triggers | `williams_blitter.rs:252` |
| Blitter rate | 1 cycle/byte fast, 2 slow (`CTRL_SLOW` = bit 2) | `williams_blitter.rs:291` |
| Blitter variant | Joust/Robotron = SC1, `size_xor = 4` | `williams.rs:425`, `sc1()` |
| Blit geometry | **width counts columns, height counts scanlines**: within a row the address steps by `dxadv`, and the row advance is the `+1` | `williams_blitter.rs:388-414` |
| Stride-256 row advance | `dstart = (dstart & 0xFF00) \| ((dstart+1) & 0xFF)` | `williams_blitter.rs:403` |
| CPU halted during blit | `step_cycle` runs the blitter *instead of* the CPU | `williams.rs:325-337` |

The blit-geometry row is the one fact this document originally left out, and it
is the one a reader needs to check the screen fill: 146 × 85 with
`DST_STRIDE_256` covers columns 3–148 of scanlines 0–84, not the transpose.

### Two derived facts worth stating up front

**The video counter aliases.** `current_scanline()` returns `u8`, so scanlines
256–259 read back as 0–3 and mask to 0. The counter therefore reads `$00` for
**eight** consecutive lines per frame (256–259 and 0–3) and four lines for every
other value. Whether that matches the hardware's vertical counter is a schematic
question; T1 measures what we do.

**The scanline-256 guard is not CPU-observable.** With `if scanline != 256`,
CB1 holds its line-224 value through line 256 and falls at line 257 instead.
Both lines read counter `$00`, and the PIA does not expose the CB1 *level* to
the CPU, so no program can distinguish the two. T3 records the edge set; the
one-line delay needs a schematic answer, not a test. This is recorded here so a
future reader knows it was considered rather than missed.

## The ROM

### Layout

| Region | Use |
|---|---|
| `$D000-$FFEF` | code |
| `$FFF0-$FFFF` | vectors (IRQ → handler, everything else → `RTI`) |
| `$0000-$001F` | direct-page variables (VRAM column 0, never displayed) |
| `$AFFF` | stack top (VRAM column `$AF` = 175, never displayed) |
| `$B000-$B01F` | result block (VRAM column `$B0` = 176, never displayed) |
| `$9800-$9FFF` | blitter timing scratch (columns `$98-$9F`) |
| `$A000-$A3FF` | blitter XOR-4 scratch |

Displayed VRAM is `$0300-$94FF` (columns 3–148). Everything the ROM uses for
its own storage is above that and below `$C000`, so no scratch write can
perturb the picture, and the banked-ROM overlay only covers `$0000-$8FFF`.

### Frame synchronisation

Two primitives, both keyed to the video counter and never to a cycle count:

- `WaitWrap` — spin until the counter reads ≥ `$C0`, then spin until it reads
  `< $10`. Returns during scanlines 256–259, i.e. in the tail of the frame just
  finished, after every visible line. A store issued here lands cleanly before
  the next frame's line 7.
- `WaitLine B` — spin until the counter equals `B`. The poll loop is ~10 cycles
  against a 256-cycle counter step, so it cannot skip a value.

### Phase protocol

The ROM writes a phase index to `$B001` as each phase completes, and `$5A` to
`$B000` at the very end. The harness runs one frame at a time and reads the
phase after each. Phase markers for the picture phases are written at
`WaitLine $F0` (line 240) of the frame they describe, so a phase change is
always observed after the `run_frame()` that produced the picture it labels.

`$B000` staying zero means the program wedged, and `$B001` says where. That is
the vacuous-pass guard: a zero-filled result block must never read as success.

| Phase | Contents |
|---|---|
| 1 | T1 — video counter survey |
| 2 | T2 — CA1 (count240) edge |
| 3 | T3R — CB1 (VA11) rising edges |
| 4 | T3F — CB1 falling edges |
| 5 | T4/T5 — blitter timing and the SC1 XOR-4 bug |
| 6 | screen filled solid with pen 1 |
| 7 | T6 — palette changed mid-frame → **capture A** |
| 8 | T7 — VRAM written above and below the beam → **capture B** |
| 9 | idle frame → **capture C** |
| 10 | complete, `$5A` written, one frame after phase 9 |

**Phase 9 has to be held for a whole frame.** The harness reads the phase byte
once per `run_frame()`, so a phase is only observable if it is the last one
written in its frame. The first version fell straight from phase 9 into phase 10
and the magic byte, all at line 240 of the idle frame, so capture C was never
taken and T7 failed with a missing capture rather than with anything about
pixels. `Done` now waits a full frame first. `WaitWrap` alone is not enough: it
returns during scanlines 256–259, which are still inside the same frame.

### Result block

Every row below was **measured** on joust and robotron and matches its
derivation, except `T4_SLOW`, which is the bug.

| Address | Field | Expected | Derivation |
|---|---|---|---|
| `$B000` | `MAGIC` | `$5A` | completion |
| `$B001` | `PHASE` | `10` | |
| `$B002` | `T1_TRANS` | `64` | counter changes once per 4 lines; 260/4 = 65 values, one repeated at the wrap ⇒ 64 transitions |
| `$B003` | `T1_WRAPS` | `1` | |
| `$B004` | `T1_MAX` | `$FC` | 252 = largest multiple of 4 below 256 |
| `$B005` | `T1_DWELL0` | ≈ 2 × `T1_DWELL4` | value 0 spans 8 lines (256–259, 0–3), every other value spans 4 |
| `$B006` | `T1_DWELL4` | — | reference dwell for one 4-line step |
| `$B007` | `T2_COUNT` | `1` | count240 rises once per frame |
| `$B008` | `T2_LINE` | `$F0` | 240 |
| `$B009` | `T3R_COUNT` | `4` | VA11 rises at 32, 96, 160, 224 |
| `$B00A-$B00D` | `T3R_LINES` | `$20 $60 $A0 $E0` | |
| `$B00E` | `T3F_COUNT` | `3` | in the window [line 16, line 240): falls at 64, 128, 192 |
| `$B00F-$B011` | `T3F_LINES` | `$40 $80 $C0` | |
| `$B012` | `T4_FAST` | `$10` | 8 × 128 = 1024 bytes × 1 cycle = 1024 cycles = 16 lines |
| `$B013` | `T4_SLOW` | `$20` | same blit with `CTRL_SLOW` = 2048 cycles = 32 lines. Measured `$10` before the fix, `$20` after. The bug, see below |
| `$B014` | `T5_A` | `$EE` | `$A000` written by the 4×4 blit |
| `$B015` | `T5_B` | `$00` | `$A100` **untouched**: SC1 XORs 4 into width/height, `4^4 = 0`, clamped to 1, so the blit is 1×1 |

### T4 in detail — the predicted first-run failure, which happened

`WilliamsBlitter::do_dma_cycle` returns the cycle count the byte cost (1 fast,
2 slow, `williams_blitter.rs:283`). The device unit test accumulates it
(`core/tests/williams_blitter_test.rs:62`). **The board discards it**:

```rust
// machines/src/williams.rs:333
blitter.do_dma_cycle(bus);
```

`step_cycle` runs exactly one DMA cycle per CPU cycle, so `CTRL_SLOW` has no
effect on wall time. **T4 failed on first run with `T4_SLOW == $10`** against a
derived `$20`, on both machines, exactly as written here before it was run.

This is a seam bug of the kind unit tests structurally cannot find — the device
is right, the board is right in isolation, and the join is wrong. It is the
single best argument for the whole exercise, and it was found by reading the
code while designing the test rather than by running it.

The fix charges the second cycle **inside the device**, so all blit timing stays
in `williams_blitter.rs`: `do_dma_cycle` consumes one clock, moving a byte on
the first of a slow byte's two and nothing on the second, with a `stall` flag
carrying the blit across. The alternative, a stall counter on the board driven
by the returned count, keeps the device untouched but puts blit timing in two
places.

The return value did **not** become vestigial as expected. It now means "clocks
consumed by this call", which is 1 while active, so
`core/tests/williams_blitter_test.rs` needed no change at all: its slow-mode
timing test still accumulates 2n and now does so for the right reason.

Which of a slow byte's two clocks moves it is not determined by anything
available. The CPU is halted throughout and cannot see the difference; only the
renderer could, and only within one scanline. The byte moves on the first clock.

**What it cost.** Robotron and Sinistar drew a different attract frame
afterwards and their golden frames were recaptured, reviewed by eye and by play.
Joust did not move at all, which says its attract loop issues no slow blit in
1800 frames while the other two do.

### T6 and T7 — the picture tests

These are the two that need the framebuffer, and the expected image is written
down rather than captured.

**T6, the palette split.** Displayed VRAM is filled solid with `$11` (pen 1 in
both nibbles). Palette entry 1 starts red (`$07` = `BBGGGRRR` with R = 7). At
counter `$78` (line 120) a single store puts green (`$38`) into `$C001`.

Because `begin_scanline` renders line N before the CPU runs line N, the store
during line L is visible from line L+1 onward. The counter's 4-line granularity
puts the store in lines 120–123, so the boundary falls in scanlines 121–124,
i.e. screen rows 114–117.

Assertion:

- screen rows 0–113: every displayed pixel is red
- screen rows 118–239: every displayed pixel is green
- rows 114–117: transition band, not asserted
- exactly one transition: no red pixel appears below row 117

**T7, the beam has already passed.** This is the Joust bug class, stated
directly. Palette entry 1 is restored to red and entry 2 is green. At counter
`$78`, two stores put `$22` into VRAM at column 80:

| VRAM address | column | row | screen row |
|---|---|---|---|
| `$503C` | 80 | 60 | 53 |
| `$50C8` | 80 | 200 | 193 |

Column 80 maps to screen x = `(80 - 3) * 2` = 154 and 155, both nibbles pen 2.

Two-frame prediction:

- **Capture B** (the frame the writes happened in): screen row 193 at x = 154,
  155 is **green**; screen row 53 at the same x is still **red**, because
  scanline 60 was drawn long before the store.
- **Capture C** (the next frame, no writes): **both** rows are green.

Get the render order wrong in either direction and one of the two captures is
wrong. Render whole-frame at end of frame and capture B shows both rows green.
Render whole-frame at start of frame and capture B shows neither.

**T7 is the load-bearing test in this document.** Everything else guards a
signal; this one guards the model.

**Both predictions held, and the discrimination was demonstrated rather than
argued.** `begin_scanline` was temporarily given a second pass that re-renders
every visible line at scanline 259, which is the whole-frame-at-end-of-frame
model above. Under it T6 and T7 both failed and the six CPU-observable tests
still passed, which is the right pattern for a rendering-only mutation.
Reverting restored green. Anyone revisiting whether these two tests earn their
keep should redo that mutation rather than trust this paragraph.

### The screen fill

A CPU loop over 146 × 255 bytes would cost ~10 frames. The blitter does it in
37,230 cycles as a solid fill, split into three passes of 85 rows so the CPU is
never halted for longer than a frame (12,410 cycles = 194 scanlines each) and
the watchdog can be petted between them:

| Register | Value | Note |
|---|---|---|
| `$CA01` solid | `$11` | pen 1, both nibbles |
| `$CA02/03` src | `$0000` | read anyway even in solid mode (`do_dma_cycle` step 1); pointed at VRAM so it touches no I/O |
| `$CA04/05` dst | `$0300`, `$0355`, `$03AA` | column 3; rows 0, 85, 170 (stride-256 row advance is `(dstart+1) & 0xFF`) |
| `$CA06` width | `$96` | 146 ^ 4 (SC1) |
| `$CA07` height | `$51` | 85 ^ 4 (SC1) |
| `$CA00` ctrl | `$13` | SOLID \| DST_STRIDE_256 \| SRC_STRIDE_256, fast |

Rows 0–254 cover every visible scanline (7–246). Source stride 256 keeps the
dummy source reads inside `$0000-$91FF`, so the DMA never reads a PIA register
and cannot clear an interrupt flag as a side effect.

Every size register is written as a pre-XORed hex literal with its derivation
in a comment rather than as `size+4`. Addition and XOR agree only when bit 2 of
the size is clear — true for 146, 8 and 128, false for 85 — and that is exactly
the kind of trap a conformance ROM should not be carrying.

## The harness

`machines/tests/williams_video_timing_test.rs`, ROM-less, over Joust, Robotron
and Sinistar. Each machine is built with `create_bare`, the image its
program-ROM window takes is poked in through `BusDebug::write`, and `reset()`
picks up the patched vector.

```rust
let mut m = (entry.create_bare)();
for (i, b) in PROGRAM.iter().enumerate() {
    bus.write(0, LOAD_ADDR + i as u32, *b);      // ReadOnly takes it
}
m.reset();

for _ in 0..MAX_FRAMES {
    m.run_frame();
    let phase = peek(&*m, R_PHASE);
    if (7..=9).contains(&phase) { /* capture this frame, once */ }
    if peek(&*m, R_MAGIC) == MAGIC { break; }
}
```

**One test per signal, rather than one test collecting assertions.** The design
originally called for collecting them the way `golden_frame_test` does, so that
"these three signals moved and these four did not" is the diagnosis. Ten
separate `#[test]` functions give the same property for free — the runner
reports each independently — without a bespoke collector, and each one carries
its derivation in its doc comment. Every test calls `assert_completed` first, so
a wedged program fails on the magic byte rather than on a dozen assertions about
zeroes.

### Sinistar, and the rotation that caught the harness out

Sinistar maps program ROM at `$E000-$FFFF` (8 KB) and adds SRAM at `$D000`
(`williams.rs:461-470`). Rather than fork the source, the origin is a build-time
symbol: `asl -D ROMBASE=0xE000` produces a second 8 KB image from the same
assembly, and everything the program touches other than its own code lives in
video RAM below `$C000`, so relocating the code is the whole difference. Its
blitter also carries the window clip, which `$C900` bit 2 gates and the program
clears at startup along with the ROM bank.

**The thing that actually broke was not the memory map.** Sinistar's cabinet
stands the monitor on its side, so `SinistarSystem::render_frame`
(`sinistar.rs:442`) rotates the board's landscape raster 270 degrees into a
240x292 portrait buffer. Every coordinate in this document is a *raster*
coordinate — scanline 7 is screen row 0, VRAM column 80 is x 154 — so reading the
rendered frame directly asked for row 0 and got row 239.

Both picture tests failed, and usefully they failed in *opposite* directions:
T6 saw green where it wanted red at the top of the frame, T7 saw red where it
wanted green near the bottom. A capture taken at the wrong frame would have
moved both the same way, which is what ruled that out; a probe confirmed all
three machines publish each phase on the identical frame. The harness now undoes
the rotation on read, so the assertions stay in raster coordinates and the
cabinet's orientation remains a separate concern with its own tests.

Not covered: the `$D000` SRAM as a blit destination that is not video RAM. It is
the one thing Sinistar could exercise that the other two cannot, and it needs a
new phase in the ROM rather than a relocation of the existing one.

## Build and toolchain

- **Assembler:** `asl` and `p2bin`, in `flake.nix`. **Not `lwasm`:** nixpkgs has
  no `lwtools` attribute at all, so it would have meant a custom derivation with
  an upstream tarball URL to maintain. `asl` is packaged and targets 6502, Z80,
  68000 and the rest, so a conformance ROM for a second board needs no new
  assembler.

  ```
  asl -q -o williams_video.p williams_video.asm
  p2bin williams_video.p williams_video.bin -r 0xD000-0xFFFF -l 0x00
  ```

  The port was verified rather than assumed: `asl` reproduces the original
  `lwasm` image **byte for byte**, sha256 `296f9527…053bcc7` from both. Two
  independent assemblers agreeing over the same source says more about the
  binary than either alone. The port needed only three changes — a `cpu 6809`
  directive, `setdp $00` becoming `assume dpr:$00`, and `zmb $FFF0-*` giving way
  to `org $FFF0` with p2bin's `-r`/`-l` doing the zero fill.
- **Committed artifacts:** `machines/tests/roms/williams_video.asm` (source) and
  `machines/tests/roms/williams_video.bin` (12 KB image), both in git.
- **Drift guard:** a test that re-assembles the source and byte-compares against
  the committed binary. It reports the first differing address and the rebuild
  commands. **It must not be allowed to pass by doing nothing:** the dev shell
  exports `PHOSPHOR_ASM=1`, and with that set a missing assembler is a failure
  rather than a skip. CI has no dev shell, sets nothing, and skips with a
  printed note. This matters because the guard's first incarnation reported
  green for its whole life, `lwasm` being on `PATH` nowhere.
- The binary is a full 12 KB `$D000-$FFFF` image, zero-padded, with vectors at
  the top, so `load_program` is a flat copy. `the_image_fills_the_program_rom_window`
  pins the length and the reset vector, so a bad `-r` window produces a failure
  rather than a short file nobody notices.

## Running the same binary in MAME

Not required, and deliberately not a gate. But the ROM is beam-synchronised and
its verdict lives in RAM, so the identical image can be substituted for Joust's
program ROMs (MAME warns on the CRC mismatch and runs) and the result block read
back with a Lua script using the idiom already in `tools/mame_digdug_trace.lua`:
`manager.machine.devices[":maincpu"].spaces["program"]:read_u8(0xB000 + n)`.

Where the two disagree, the schematic decides, not MAME. `oyxg` is the standing
precedent.

## What this does not cover

Sprite and tile decode, palette derivation, orientation, and every question
about whether Joust's *artwork* is right. The ROM draws its own patterns. Those
belong to the golden frames and to the state-injection fixtures for the
render-once machines.

It also says nothing about the sound CPU, which on a bare board executes
zero-filled memory on its own bus and cannot reach main VRAM.

## Risks

1. **The ROM encodes our model, not the hardware.** Every expected value in the
   result table above is derived from *our* source. That makes the suite a
   regression guard immediately and a correctness guard only once each figure
   has been checked against the schematic. Each expectation therefore carries
   its derivation in the table, and the ones that are still just "what we do
   today" — the counter aliasing, the scanline-256 guard — are named as such.
2. **A wedged program reads as a pass** unless the magic byte is checked first.
   Guarded by `assert_completed`, which every test calls before anything else,
   and the guard has fired for real: the phase 9/10 collision surfaced as a
   missing capture and a named phase rather than as a suite of assertions about
   zero bytes.
3. **A guard that cannot fail is worse than no guard**, which this document
   demonstrated the hard way. The drift guard comparing source to binary
   reported green for its entire life because no assembler existed on `PATH`
   anywhere, including inside the dev shell. It now fails when `PHOSPHOR_ASM`
   says an assembler should be there, and that failure mode was tested by
   running it with an empty `PATH`. Any future conformance ROM inherits both the
   pattern and the trap.
4. **Counter granularity is 4 lines**, so no test can assert a single-line
   position. Every expectation above is stated to that resolution, and T6's
   transition band is four rows wide by construction.
5. **Sinistar's window clip** silently suppresses blits into
   `[clip_address, 0xC000)` when enabled. The ROM's scratch at `$9800-$A3FF`
   sits above Sinistar's `$7400` clip, so a stray enable would make T4 and T5
   fail confusingly. The ROM clears `$C900` explicitly at startup.

## Sequencing

Tracked as `phosphor-emulator-williams-video-conformance-itvk`.

1. **Done** (`itvk.1`) — `asl` and `p2bin` in `flake.nix`, source ported and
   verified byte-identical, drift guard made to fail three ways on purpose.
2. **Done** (`itvk.2`) — hold phase 9 for a frame so capture C is taken.
3. **Done** (`itvk.3`) — land the ROM, the image and the harness, with the slow
   assertion held back. Nine assertions, no arcade ROMs, running in CI.
4. **Done** (`itvk.4`) — the slow-blit fix, landed with the assertion that found
   it.
5. **Done** (`itvk.5`) — ROM-gated suites re-run; Robotron and Sinistar
   recaptured, Joust unmoved.
6. **Done** (`itvk.6`) — Sinistar, from a second link address built out of the
   same source. All three machines on `WilliamsBoard` are covered.

Only then a second board. `raster-sampling-fidelity.md` W3 lists seven:
`namco_galaga`, `btime`, `mrdo`, `foodf`, `gottlieb`, `mcr2`, `atari_system1`.
Williams is first because it is the only one that exposes beam position to the
CPU, which is what makes the ROM self-synchronising. Whether any of the others
can host one at all is `phosphor-emulator-conformance-rom-programme-hl4t.1`, and
a first look is not encouraging: `btime` exposes an in-vblank bit
(`btime.rs:559`), which is one edge per frame, and most of the rest expose
nothing of the beam.

## References

- `machines/src/williams.rs` — `begin_scanline` `:648`, `render_scanline` `:600`,
  `step_cycle` `:321`, map `:451`, video counter `:1000`, reset `:767`
- `core/src/device/williams_blitter.rs` — register map, `do_dma_cycle` `:283`
- `core/src/device/pia6820.rs` — CRA/CRB semantics, `set_ca1` / `set_cb1`
- `core/src/core/address_space16.rs:509` — `debug_write`, the loading mechanism
- `machines/src/registry.rs` — `MachineEntry::create_bare`
- Sean Riddle, <https://seanriddle.com/blitter.html> — SC1/SC2 behaviour
- MAME `src/mame/midway/williamsblitter.cpp`, `williams.cpp`
