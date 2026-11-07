# Survey: what can each board's CPU see of the beam?

> **Status: first pass, from MAME drivers only.** Answers
> `phosphor-emulator-conformance-rom-programme-hl4t.1`. Companion to
> [`williams-video-conformance.md`](williams-video-conformance.md), which is the
> worked example this survey is trying to reproduce elsewhere.
>
> **Method caveat, stated up front.** The issue asks for the schematic *and* a
> reference driver. This pass had only the driver: MAME 0.287 under
> `~/ws/mame/src/mame`. A driver tells you what MAME models, which is a proxy
> for the schematic and not the thing itself, and at least one entry below
> (`foodf`) carries a MAME comment admitting its own timing is unresolved.
> Nothing here should be treated as settled until the schematic confirms it, and
> the FULL verdicts matter most because they are the ones that will get built on.

## Why this question

The Williams conformance ROM is immune to a constant cycle offset between
implementations because it never waits by counting cycles. It polls the video
counter at `$CB00`, which reads `scanline & $FC`, so every wait is keyed to where
the beam actually is. Reproducing that on a second board requires the second
board to expose something equivalent, and nothing says it does.

Three verdicts, as the issue defines them:

- **FULL**: a Williams-style self-synchronizing ROM is possible: the program can
  place an event at a chosen scanline without counting cycles.
- **PARTIAL**: beam-locked events exist at a fixed coarse grid, or a readable
  level changes inside the frame, but neither gives arbitrary placement. Useful,
  with the resolution stated.
- **FRAME**: only frame-level synchronization. A ROM can test what happens
  across a frame boundary and cannot place an event inside one.
- **NONE**: nothing beam-locked reaches the CPU at all.

## The table

| Board (games) | CPU | Readable beam state | Beam-locked interrupts | Verdict |
|---|---|---|---|---|
| **atari_system1** (marble, roadrunner) | MC68010 | VBLANK level at `$F60000` bit 4; **IRQ3 state at `$2E0000` bit 7**, asserted for exactly one scanline | IRQ4 at vblank; **IRQ3 at a scanline the program chooses** | **FULL** |
| **gridlee** | M6809 | VBLANK level at `$9700` (IN2) bit 7 | IRQ at scanlines 0, 64, 128, 192, 256; FIRQ once per frame at a fixed line (~92) | **PARTIAL**, 64 lines |
| **foodf** | MC68000 | none: `SYSTEM` at `$948000` is coin/start/service only | IRQ1 on 32V, at scanlines 32/96/160/224; IRQ2 at vblank | **PARTIAL**, 64 lines |
| **mcr2** (shollow) | Z80 | none | CTC trg2 at scanlines 0 **and** 240; trg3 at 0 | **FRAME**, two edges |
| **btime** (burgertime) | DECO CPU-7 (6502) | VBLANK level in a DIP port, bit 7 | main CPU NMI; audio NMI every 8 scanlines, but on the sound CPU's own bus | **FRAME** |
| **namco_galaga** (galaga, digdug, xevious) | 3 × Z80, shared bus | none: inputs arrive through the 06XX/51XX chain | main and sub IRQ at vblank; **sound Z80 NMI at scanlines 64 and 192** | **PARTIAL**, 128 lines |
| **gottlieb** (qbert) | I8088 | none: ports are DSW and IN1–IN4 | NMI at vblank (line 240) | **FRAME** |
| **mrdo** | Z80 | none: ports are P1, P2, DSW1, DSW2 | IRQ at vblank | **FRAME**, weakest of the set |

## The two findings that matter

### atari_system1 is FULL, and by a better mechanism than Williams

Williams gives you a counter to poll at four-scanline resolution. Atari System 1
gives you something stronger: a **programmable** beam-locked interrupt at
one-scanline resolution.

The motion-object list carries timer entries: an entry whose second word is
`$FFFF` is not a sprite, it is a request to raise IRQ3 when the beam reaches that
entry's Y position (`atarisy1_v.cpp:411`, `atarisy1r_state::update_timers`, and
`machines/src/atari_system1.rs:683` on our side). `int3_callback` asserts IRQ3
and schedules `int3off_callback` one scan period later, so the line is high for
exactly one scanline (`atarisy1_v.cpp:362-384`). The CPU can read that state back
at `$2E0000` bit 7 (`int3state_r`, `:394`).

So a conformance ROM on this board can say *"interrupt me at scanline 137"*,
write a video register in the handler, and assert what the picture does, with no
cycle counting anywhere and with a poll-able level as a backstop if interrupts
are inconvenient.

Two things make this the obvious second board:

1. It is the board `raster-sampling-fidelity.md` most wants an instrument for.
   Its W3 names atari_system1 as the *known-latched reference* against which the
   other six are to be judged, and our implementation's `mo_shadow` /
   `mo_bank_changes` model is an assumption nothing currently tests.
2. Our MC68000 is not cycle-accurate; that is an open epic
   (`cycle-accurate-m68000-5y6e`). A beam-synchronized ROM is exactly the test
   design that does not care, which makes this a demonstration of the technique's
   central claim rather than a compromise.

Costs to weigh before committing: it is a 68010 rather than an 8-bit CPU, the
program has to build a motion-object list before it can use the timer at all, and
scratch storage needs finding: `$400000-$401fff` and `$900000-$9fffff` are plain
RAM in MAME's map, which looks ample, but that has not been checked against what
our board actually backs.

### FRAME is worth more than it sounds, because of what W3 asks

Four of the eight boards are FRAME-only, and the instinct is to write them off.
That is wrong for the specific question already on the tracker.

`raster-sampling-6kae.3` (W3) asks, per board: *does the sprite circuit read a
list captured at vblank, or read sprite RAM live during the frame?* Answering that
does **not** need scanline placement. It needs a program that writes the sprite
list during active display and observes whether the change appears on this frame
or the next, which is a frame-boundary test, and every FRAME board can host one.
`mcr2` can do better still, since CTC trg2 fires at both line 0 and line 240 and a
handler can tell them apart by alternation, giving a pair of timestamps that
bracket the visible area.

So the survey's practical output is not "one board qualifies and seven don't". It
is two tiers of test:

- **Placement tests** (Williams, atari_system1): where in the frame is state
  sampled, does a write split the picture at the beam, when exactly does an
  interrupt arrive.
- **Latch tests** (everything else): is this piece of video state read once per
  frame or continuously: the W3 question, answerable with a vblank edge alone.

## Per-board notes

**namco_galaga.** The main and sub Z80s see only a vblank IRQ. The *sound* Z80
takes an NMI at scanlines 64 and 192 (`galaga.cpp:786-799`, mirrored at
`machines/src/namco_galaga.rs:684`), two beam-locked intra-frame timestamps,
128 lines apart.

Those are usable, because all three Z80s share one memory map. MAME defines a
single `galaga_map` for all of them and comments the ROM window as *"the only
area different for each CPU"* (`galaga.cpp`, `galaga_map`, `map(0x0000, 0x3fff)`).
Video RAM at `$8000-$87FF` is common. So the sound Z80 can write the picture, and
a program running there has a 128-line grid plus the vblank edge at 224: three
beam-locked timestamps a frame. That is PARTIAL, not FRAME, and it covers three
registered machines.

One complication to carry into any implementation issue: on a bare board all
three CPUs execute zero-filled ROM into *shared* RAM, so the two that are not
running the test have to be parked. Galaga's LS259 misc latch at `$6820` holds
the sub and sound CPUs in reset, so the natural shape is a two-part image: a
short park-and-release stub in the main CPU's ROM window and the conformance
program in the sound CPU's. The Williams ROM did not have this problem because
its sound M6800 sits on a separate bus.

**btime.** The vblank line reaches the CPU as bit 7 of a DIP port, which MAME's
own comments note is "part of the vblank circuit" and is why SW2.8 reads the way
it does (`btime.cpp:139`, `:1258`, `:1314`). Readable, one edge per frame. The
8-scanline audio NMI (`audio_nmi_gen`, `:1030`) is beam-derived but lands on the
sound CPU, which has its own bus.

**foodf.** MAME's own comment on the 32V interrupt is worth quoting because it
bears directly on how much to trust the driver here: *"However, due to the
interrupt structure, it cannot fire at the same time as VBLANK. I have not solved
this"* (`foodf.cpp:327-329`). A conformance ROM would be measuring against a
reference that admits it is approximate. Schematic first for this one.

**gottlieb.** `GOTTLIEB_VIDEO_VBLANK = 240` and `screen_vblank()` drives the 8088's
NMI directly. Nothing in the base memory map reads back beam position
(`gottlieb.cpp`, `gottlieb_base_map`). Note the second-order problem: our I8088 is
instruction-level, not cycle-accurate (`cycle-accurate-i8088-nvrh`), so a
cycle-counted test here would be worthless, which is an argument for a
beam-locked one, if there were a beam to lock to. There is not, beyond vblank.

**mrdo.** The weakest board in the set. Four input ports, none of them beam
related, and a single vblank IRQ. It is the nearest thing to a NONE verdict, and
if the schematic confirms it, mrdo should be recorded as keeping its golden frame
and nothing else.

## Recommendation

1. **Second board: atari_system1.** FULL verdict, one-scanline placement, and it
   is the board the raster-sampling work already needs an instrument for. Build
   it as its own epic mirroring the Williams one.
2. **Third board, if there is one: namco_galaga.** PARTIAL at 128 lines, three
   registered machines for one ROM, and a Z80: a third CPU family, which is the
   real test of whether the harness contract generalizes. Behind atari_system1
   because of the park-the-other-CPUs complication and the coarser grid.
3. **Write `hl4t.2` (the shared harness contract) after atari_system1 lands**,
   not before, exactly as that issue says. A 68010 board with a programmable
   interrupt and an M6809 board with a pollable counter are different enough that
   generalizing from the pair will produce something honest; generalizing from
   Williams alone would produce a guess.
4. **Do not build placement-style ROMs for the FRAME boards.** Fold their needs
   into W3 as latch tests instead, which is work already scoped and already
   wanted.
5. **Confirm against schematics** before any of the above is built on. This pass
   is driver-derived, and the project's own standard is that the schematic and the
   datasheet win; `oyxg` is the precedent, where the reference core was wrong and
   we were right.

## References

- MAME 0.287: `atari/atarisy1.cpp` `:408`, `atari/atarisy1_v.cpp` `:362-397`,
  `:411`; `bally/gridlee.cpp` `:64`, `:101-139`; `bally/mcr_m.cpp` `:107`;
  `atari/foodf.cpp` `:322-341`; `dataeast/btime.cpp` `:1030`, `:1258`;
  `namco/galaga.cpp` `:786-799`; `gottlieb/gottlieb.cpp` `gottlieb_base_map`;
  `universal/mrdo.cpp` `main_map`
- Ours: `machines/src/atari_system1.rs:683` (`timer_irq_at_scanline`),
  `machines/src/namco_galaga.rs:684`, `machines/src/mcr2.rs:380`
- [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md) W3, for the board
  list and the latch-versus-live question this feeds
