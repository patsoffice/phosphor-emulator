# Space Duel's audio output

What Atari's Space Duel main PCB does between its two POKEYs and the cabinet.
Read for `phosphor-emulator-djlf`. The model will live in `machines/src/spacduel.rs`.

The finding in one line: **each POKEY drives its own transimpedance stage and
its own multi-pole filter chain, and the two chains meet at one mixer.** The
B3 POKEY passes a 1.6 kHz low-pass, a 7 Hz coupling, two 159 Hz poles at gains
of -10 and -1, and reaches the mixer at 0.33. The C/D3 POKEY passes a 723 Hz
low-pass, an 18.5 Hz coupling, two unity inverters, and reaches the mixer at 1.
The mixer itself low-passes at 482 Hz.

## Provenance

| | |
|---|---|
| Drawing | `Space Duel Main PCB Schematic Diagram`, SP-181 sheet 6A, 2nd printing, (c) Atari Inc. 1982, blocks `Option Switch Input And Audio Output` and `Coin Door And Control Panel Output` |
| Read from | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Space_Duel/Space_Duel_SP-181_2nd_Printing.pdf`, PDF page 10, a scanned drawing |
| Transcribed | 2026-10-01 |

Only sheet 6A was read. All refdes below are from it.

## The two POKEYs and their option switches

The chips are `B3 CUSTOM` and `C/D3 CUSTOM`, Atari POKEYs. Address lines AB3
through AB0 reach both; the selects were not traced (the CPU map in the
reference driver puts POKEY 1 at 0x1000 and POKEY 2 at 0x1400, which this sheet
neither confirms nor contradicts).

Each chip's pots read one DIP switch through a 10k 8-pin pull-up pack: **B4
into B3** (pins P0-P7) and **D4 into C/D3**. So DSW1 rides B3's ALLPOT and
DSW0 rides C/D3's, matching the reference driver.

## Two transimpedance first stages

Each `AUD` (pin 37) goes straight to an LM324 inverting input with no series
resistor, so each POKEY works into a virtual ground and its stage output is
the feedback resistance times the current its devices sink.

| POKEY | shunt | amp pins | feedback | corner | bias |
|---|---|---|---|---|---|
| B3 | C34 0.015 uF MYL | 9, 10, 8 | R46 **1k** with C29 **0.1 uF** | **1.59 kHz** | pin 9 reference not traced; rails are +-15 V |
| C/D3 | C133 0.015 uF MYL | 5, 6, 7 | R50 **1k** with C32 **0.22 uF** | **723 Hz** | pin 5 on +5 V through R49 100k, decoupled by C37 and C42 |

C34 and C133 sit on points the op-amps hold still and do nothing in the band.

## The B3 chain

C30 0.22 uF couples the first stage into R44 100k, a 7.2 Hz high-pass. Then
two inverting low-pass stages, both on +-15 V LM324s with grounded
non-inverting inputs:

- R42 **1M** with C27 **0.001 uF**: gain **-10**, pole at **159 Hz**.
- R47 **1M** in, R43 **1M** with C28 **0.001 uF**: gain **-1**, pole at **159 Hz**.

R45 **10k** carries the result to the mixer.

## The C/D3 chain

C31 0.22 uF couples the first stage into R48 39k, an 18.5 Hz high-pass. Then
two unity inverting stages, also grounded-input LM324s:

- R52 **39k** over R48 39k: gain **-1**.
- R54 **3.3k** in, R55 **3.3k** feedback: gain **-1**.

R53 **3.3k** carries the result to the mixer. The two inversions cancel and
neither stage filters, so the model omits them and keeps the sign they leave.

## The mixer and the outputs

One LM324 (pins 2, 3, 1; pin 3 grounded; +-15 V rails) sums both chains
through R51 **3.3k** with C33 **0.1 uF**, a **482 Hz** low-pass. Gains: the
C/D3 chain at **-1**, the B3 chain at **-0.33** (3.3k over 10k). Each chain
passes four inversions total, so the two add in phase.

The output appears as `AUD1`/`AUD2` test points and leaves on J19 pin 11
(`AUD`) and pin 12 (`AUC`). What hangs on J19 is off this sheet.

## The output latch (same sheet, lower block)

An R9 LS273 latches, from the coin-counter write:

- `INVERT X`, `INVERT Y` to the vector generator.
- `COIN LOCKOUT` through Q2 2N6044 (R117 1k, C116 0.1 uF).
- `START LED`, `SELECT LED` through R121/R122 220 ohm.
- `COIN CNTR-L` through Q4 and its network (not read in full).

## Where the reference driver diverges

MAME's `bwidow_a.cpp` models POKEY 1 straight into its mixer and hangs its
filter values on different refdes: mixer input R46 where the sheet has R44 at
100k in a filter and R45 at 10k feeding the mixer, C28 as 1 pF where the sheet
reads 0.001 uF, and a 3.9k mixer feedback where the sheet has R51 at 3.3k.
The sheet wins everywhere it was legible; see the not-established list for
the one value read with less than full confidence.

## What it does NOT establish

- **C33's value beyond 0.1 uF at moderate confidence.** The scan blurs the
  label and only the leading `1` is certain. It sets the mixer's 482 Hz pole.
- **C37 and C42.** Bias decoupling on the C/D3 stage's pin 5; no model effect.
- **Pin 9's DC reference on the B3 first stage.** The trace runs off the area
  read. The stage's output is AC coupled through C30 either way.
- **The POKEYs' source impedance**, which would set the C34/C133 shunt poles.
  In the ideal virtual-ground model both nodes sit still, so neither cap acts.
- **LM324 finite gain-bandwidth, slew, and rail headroom.** Ideal op-amps in
  the band; clipping is not modeled.
- **The selects into B3/C/D3** (pins 30-32: CS0, CS, R/W were seen but not the
  decode), and **Q4's coin-counter network** in full.
- **Everything past J19 11/12.** Sheet 2B (power supply) was not read, so the
  speaker load and any volume control are unknown.
- **Any measurement.**
