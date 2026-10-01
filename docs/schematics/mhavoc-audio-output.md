# Major Havoc's audio output

What Atari's Major Havoc main PCB does between its quad POKEY and the cabinet.
Read for `phosphor-emulator-yxda`, part of adding the machine
(`phosphor-emulator-r7pa`). The model is `machines/src/mhavoc.rs`.

The finding in one line: **the quad POKEY's four outputs reach the mixer as two
groups, not one.** OUT 1 to OUT 3 are tied onto one node and OUT 4 has a node of
its own, and the two nodes go through different filters, OUT 4's a bass voice
low-passed near 400 Hz and the other three's a treble path near 4 kHz, before a
mixer that weights OUT 4 at 2.2 times the others.

## Provenance

| | |
|---|---|
| Drawing | `MAJOR HAVOC Main PCB Schematic Diagram`, SP-252 sheet 10A, 2nd printing, (c) Atari Inc. 1983, blocks `Gamma (γ) Input/Output` and `Audio` |
| Drawing | `MAJOR HAVOC Main Wiring Diagram`, SP-252 sheet 1B, 2nd printing |
| Read from | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Major_Havoc/Major_Havoc_SP-252_2nd_Printing.pdf`, PDF pages 19 and 2, a 600 dpi scan |
| Transcribed | 2026-09-30 |

The package is 19 pages stored rotated; sheet 1A's contents are on page 1 and
sheet 10A, `Gamma Input/Output, Audio`, is the last page. Sheet 1B is page 2.

## The quad POKEY and its chip selects

The chip is 13Q, labeled `QUAD CUSTOM I/O`. Its four selects come from an LS139
at 11Q, enabled by `/QCI/O`, with B on A4 and A on A3: Y0 to `CS1`, Y1 to `CS2`,
Y2 to `CS3`, Y3 to `CS4`, in order. So the model's chip `n`, decoded from
address bits 4 and 3, is `CS(n+1)`, unlike Star Wars, whose LS139 runs the other
way.

**Read by name, not traced:** that the internal POKEY selected by `CSn` is the
one whose audio leaves on `OUTn`. The pins are labeled that way and the package's
internals are not on the sheet.

## Two nodes

| node | pins | first stage | coupling | T network | feedback |
|---|---|---|---|---|---|
| A | `OUT 4` (21) alone | 14R/S pins 9, 10, 8, R135 **1k** | C63 0.22 uF into R136 39k | R136 39k, C67 **0.01 uF** to ground, R137 18k | R138 39k ∥ C69 **0.01 uF** |
| B | `OUT 1` (23), `OUT 2` (29), `OUT 3` (27) tied | 14Q pins 2, 3, 1, R140 **1k** | C66 0.22 uF into R141 39k | R141 39k, C68 **0.001 uF** to ground, R142 18k | R143 39k ∥ C70 **0.001 uF** |

- **Both first stages are transimpedance amplifiers at a virtual ground.** Each
  pin goes straight to an LM324's inverting input, whose non-inverting input is
  `+5V AUD` (R158 100k from +5 V with C62 0.22 uF). No series resistor, so the
  pins are held at +5 V and each node's output is 1k times the current its
  chips' devices sink: linear in their conductance, and the three tied chips add
  exactly. C64 and C65, 0.001 uF on the two nodes, sit on points the op-amps hold
  still and do nothing in the band.
- **The couplings** are 0.22 uF into 39k, 18.5 Hz on both nodes.
- **The second stages** have gain `-R138/(R136 + R137)` = -39/57 = **-0.684** on
  both. Node A's T network puts a pole at `1/(2 pi C67 (R136 ∥ R137))` = **1.29
  kHz** and its feedback one at `1/(2 pi R138 C69)` = **408 Hz**. Node B's are
  ten times higher: **12.9 kHz** and **4.08 kHz**.
- **The speech input** `TIAUD`, from the TMS5220 that production boards do not
  fit, joins node B's second stage at its summing node.

## The mixer and the antiphase pair

Both second stages feed 14Q pins 9, 10, 8, with **R139 10k** from node A and
**R144 22k** from node B into **R145 10k** of feedback, the non-inverting input
on `+5AUD`. So node A arrives at -1 and node B at -0.455: **OUT 4 is weighted 2.2
times each of the others**, and both paths carry two inversions, so they add in
phase. The output leaves through **R148 2.2k** with **C92 0.01 uF** to ground as
`AUD1`, a pole at 7.2 kHz before the amplifier's own input loads it, and 14Q pins
13, 12, 14 invert it at unity (R146, R147 10k) through R149 2.2k and C93 0.01 uF
as `AUD2`.

## The cabinet

Sheet 1B: the Regulator/Audio II PCB (`SEE SHEET 2A`) drives `SPKR 2` (W) and
`SPKR 1` (BN) on P8, and **three speakers** hang across that pair: LS3 on its
own, and LS1 and LS2 in series (joined on OR) through P32/J32. Both returns are
unused, so this is a bridge like Missile Command's and Tempest's, the antiphase
pair heard in phase, and mono is right. No volume control is drawn in the speaker
path on this sheet.

## What it does NOT establish

- **That `CSn` pairs with `OUTn` inside the package**, as above.
- **The speakers' impedance and the Regulator/Audio II revision** on this
  board; sheet 2A was not read for this.
- **The LM324s' supply and headroom.** The first stages rest at `+5V AUD` and
  rise 1k times the current: up to 9.5 mA for one chip with every device on,
  so 9.5 V on node A and up to 28.5 V on node B's three chips, which no rail
  allows. How loud the game actually drives them, and so whether they clip,
  needs a measurement over play, and the rail was not read.
- **Any measurement.**
