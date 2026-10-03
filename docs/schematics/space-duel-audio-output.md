# Space Duel's audio output

What Atari's Space Duel main PCB does between its two POKEYs and the cabinet.
Read for `phosphor-emulator-djlf` and corrected for `phosphor-emulator-a6so`.
The model is the shared board's `mix_audio` in
`machines/src/atari_color_vector_conversions.rs`, with Space Duel's part values
in `machines/src/spaceduel.rs`.

The finding in one line: **Space Duel's audio chain is Gravitar's, part for
part, under different reference designators.** B3 passes a 7.2 Hz coupling, a
gain of -10 with a 159 Hz pole and a gain of -1 with another, and reaches the
summing amplifier at 0.39. C/D3 passes a 723 Hz first-stage pole and a 185 Hz
coupling and reaches it at 1. Two unity inverters after the sum make the
antiphase output pair; neither filters.

## Correction

The first reading of this sheet (for `djlf`) got four things wrong, all found
when the Black Widow and Gravitar sheets were read beside it for `a6so`:

- **C29 is supply decoupling**, from the LM324's +15 V pin 4 to its -15 V pin
  11. It is not across R46, so B3's first stage has no 1.59 kHz pole.
- **C33 is supply decoupling** on the same pins of the last LM324 section. It
  is not across R51, so there is no 482 Hz pole after the mixer.
- **The summing amplifier is B5 pins 9, 10, 8, with R52 as its feedback**, not
  the last section (pins 2, 3, 1). R45 from the B3 chain runs to pin 9 beside
  R48. So B3's weight is R52/R45 = 3.9/10, not R51/R45 = 3.3/10, and the two
  sections after the sum are the output inverters.
- **R48 and R52 are 3.9k, not 39k.** At 300 dpi the decimal point is a few
  pixels; cropped side by side, both labels render as `3 9K` with the same gap,
  and the identical stage on Black Widow and Gravitar reads 3.9k with the point
  plain. So C/D3's coupling is 185 Hz, not 18.5 Hz.

Every one of these makes Space Duel's chain identical to Gravitar's, which is
corroboration in itself: the three boards are one design family.

## Provenance

| | |
|---|---|
| Drawing | `Space Duel` PCB schematic, SP-181, 2nd printing, (c) Atari Inc. 1982: sheet 5B, blocks `Option Switch Input And Audio Output` and `Coin Door And Control Panel Output` (the first reading called this 6A); sheet 4A `Watchdog`; sheet 4B `Microprocessor` and `Address Decoder`; the `Troubleshooting with the CAT Box` memory map |
| Read from | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Space_Duel/Space_Duel_SP-181_2nd_Printing.pdf`, PDF pages 7, 8, 10 and 18; 300 dpi grayscale scans |
| Transcribed | 2026-10-01; corrected 2026-10-02 |

## The two POKEYs and their option switches

The chips are `B3 CUSTOM` and `C/D3 CUSTOM`. The CAT-box memory map puts
`Custom I/O 0 (Unfiltered)` at `1000-100F` and `Custom I/O 1 (Filtered)` at
`1400-140F`; sheet 5B selects C/D3 with `/I/O0` and B3 with `/I/O1`, so C/D3
(the lightly filtered chain) is at `1000` and B3 at `1400`.

Each chip's pots read one switch bank through a 10k pull-up pack: **D4 into
C/D3** and **B4 into B3**.

## The chains

Each `AUD` (pin 37) goes straight to an LM324 inverting input, a virtual
ground, so each first stage's output is its feedback resistance times the
current the chip's devices sink.

| part | Space Duel | Gravitar's equivalent | what it sets |
|---|---|---|---|
| B3 first stage | R46 1k | R47 1k | transimpedance |
| B3 coupling | C30 0.22 uF into R44 100k | C30, R45 | 7.2 Hz high-pass |
| gain -10 | R42 1M with C27 0.001 uF | R43, C27 | 159 Hz pole |
| gain -1 | R47 1M in, R43 1M with C28 0.001 uF | R48, R44, C28 | 159 Hz pole |
| B3 into the sum | R45 10k | R46 10k | weight 3.9/10 = 0.39 |
| C/D3 first stage | R50 1k with C32 0.22 uF | R51 with C34 | 723 Hz pole |
| C/D3 coupling | C31 0.22 uF into R48 3.9k | C33, R49 | 185 Hz high-pass, weight 1 |
| summing feedback | R52 3.9k | R52 | |
| output inverters | R54/R55 3.3k (AUD1), R53/R51 3.3k (AUD2) | R53/R54 and a buffer | antiphase pair, no filtering |
| decoupling | C29, C33 0.1 uF | C29, C35 | none in the band |

C34 and C133 (0.015 uF) sit on the virtual-ground nodes and do nothing in the
band. C/D3's first stage is biased at +5 V through R49 100k, decoupled by C37
and C42.

B3's chain inverts four times and C/D3's twice, so the two add in phase. In
band, B3 reaches the sum at 3.9 times C/D3, below its 159 Hz poles.

`AUD1` and `AUD2` leave on J19 pins 11 and 12, an antiphase pair.

## The rest of the board, for the shared model

Read for `a6so`, so that Space Duel could move onto the board Black Widow and
Gravitar use (`docs/schematics/bwidow-gravitar-board.md`):

- **The IRQ** is J4, an LS161 counting the 3 kHz clock, cleared by `/INTACK`,
  with L3 (LS00) NANDing QC and QD into `/IRQ` and `/IRQ` back into ET: the same
  circuit, pin for pin, as Black Widow's. Interrupts come twelve 3 kHz edges
  after each acknowledge.
- **The watchdog** is H4, an LS393 pair clocked by F4's 3 kHz output and
  cleared by `/WDCLR` or power-on reset, into K3: the same as Black Widow's,
  128 periods.
- **The output latch** is R9, an LS273, with `INVERT Y` and `INVERT X` on D7
  and D6, the start and select lamps, the coin lockout and the coin counters.
  No bank select: Space Duel has 1K of program RAM.
- **The input muxes** are N9 and L9, LS251s on AB2-AB0 at `0900-0907`, whose
  inverting W outputs drive DB7 and DB6. N9's D0-D7 are SHIELDS 1, SHIELDS 2,
  ROT LEFT 1, ROT LEFT 2, THRUST 1, THRUST 2, GAME SELECT and CABINET; L9's are
  FIRE 1, FIRE 2, ROT RIGHT 1, ROT RIGHT 2, START, OPTION 0, OPTION 1 and
  OPTION 2. The option and cabinet pins are the ones Black Widow's L9 and N9
  buffers carry them on.
- **M9 at `0800`** is Black Widow's coin-door buffer, except that D5 is a real
  `DIAG STEP` switch (with a test pad) rather than the signature-analysis test
  point.
- **Decode** is by LS42s (R2, P3, L4) and an LS139 (K6), not Black Widow's PROM
  pair.

## Where the reference driver diverges

It models the IRQ as a free-running 246 Hz clock and has no watchdog. Its
audio model hangs values on different refdes and gives C28 as 1 pF where the
sheet reads 0.001 uF.

## What it does NOT establish

- **The invert polarity from the drawing.** The CAT-box map labels both bits
  `1 = Invert`, and the program writes them set in an upright cabinet, so the
  sense flips somewhere between the latch and the yoke that was not traced.
  The model shows an upright picture for bits set.
- **Decode mirrors.** The LS42 decode was seen but not traced output by output;
  the model decodes the addresses the memory map lists.
- **The POKEYs' source impedance**, which would set the C34/C133 shunt poles.
- **LM324 finite gain-bandwidth, slew, and rail headroom.**
- **Everything past J19 11/12.** The speaker load and any volume control are
  off these sheets.
- **Any measurement.**
