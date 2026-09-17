# The Namco 54XX explosion channel

What the Galaga-family board does between the 54XX's output pins and the summing
amplifier that also takes the WSG. Read for `phosphor-emulator-uxi9` (Galaga)
and `-pd5e` (Xevious). Nothing models any of it today: commands sent to the 54XX
through 06XX chip-select 3 are discarded, and explosions are silent.

**The two boards carry the same circuit, component for component.** That is the
finding, and it is a reading rather than a family resemblance: the values below
were read off each sheet separately and then compared. It matters because the
opposite is the norm on this hardware. Galaga and Dig Dug share this board and
do **not** share a volume law for the WSG's own DAC, which is one sheet away
from these filters.

## Provenance

| | |
|---|---|
| Drawing | `GALAGA CPU PC`, Midway Mfg. Co., part A084-91414-A000 |
| Read from | `arcade-museum.com/manuals-videogames/G/galaga3.pdf`, PDF p23 (whole sheet), p24 (right half) |
| Drawing | `Xevious CPU PCB Schematic Diagram`, Atari SP-230 1st printing, (c) Atari Inc. 1983 |
| Read from | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Xevious/Xevious_SP-230_1st_Printing.pdf`, PDF p10 (sheet 5B) |
| Transcribed | 2026-09-17 |

The Xevious package is the one to read. It is a clean 300 dpi drawing where the
Galaga pages are 150 dpi, and the Galaga sheet was used to confirm values rather
than to establish them. Every value below resolves on Xevious; all of them are
legible on Galaga once you know what you are looking at, which is not the same
thing and is why the reading was done in that order.

## Three channels, each a DAC into a band-pass

The 54XX presents three groups of four output pins. Each group drives a
binary-weighted resistor ladder into one multiple-feedback band-pass section of
an LM324, and the three section outputs meet at the summing amplifier.

| | channel 1 | channel 2 | channel 3 |
|---|---|---|---|
| 54XX pins (Xevious) | 20, 19, 18, 17 | 11, 10, 9, 8 | 7, 6, 5, 4 |
| ladder | 4.7k, 10k, 22k, 47k | 4.7k, 10k, 22k, 47k | 4.7k, 10k, 22k, 47k |
| series into the filter | 100k | 47k | 150k |
| shunt to the reference | 22k | 10k | 22k |
| feedback | 220k | 150k | 470k |
| filter caps | 0.001 uF, 0.001 uF | 0.01 uF, 0.01 uF | 0.01 uF, 0.01 uF |
| output leg | 33k | 33k | 10k |

The six filter capacitors are marked `MYLAR` on the Xevious sheet, `x 6 PL`.

Reference designators, which are all that differ between the two boards:

| | Galaga | Xevious |
|---|---|---|
| ladder, channel 1 | (unnumbered bank) | R104, R105, R106, R107 |
| ladder, channel 2 | (unnumbered bank) | R108, R109, R110, R111 |
| ladder, channel 3 | (unnumbered bank) | R112, R113, R114, R115 |
| series 100k / 47k / 150k | R24, R33, R42 | R117, R128, R132 |
| shunt 22k / 10k / 22k | R23, R34, R41 | R127, R131, R135 |
| feedback 220k / 150k / 470k | R22, R38, R40 | R118, R129, R133 |
| caps | C30, C31 / C28, C29 / C26, C27 | C26, C27 / C29, C30 / C31, C32 |
| output legs 33k, 33k, 10k | R21, R36, R37 | R124, R130, R134 |
| summing feedback 3.3k | R20 | R125 |
| bias 3.3k / 2.2k / 10 uF | R58, R59, C29 | R138, R137, C33 |

## The summing junction belongs to neither row alone

The three output legs land on the **same** LM324 inverting node the WSG's own
DAC reaches through its 10k, which is `5P` on Galaga and `8A` on Xevious, with
3.3k of feedback. So relative to that feedback the four sources enter at:

| source | leg | weight |
|---|---|---|
| WSG DAC | 10k | 0.33 |
| explosion channel 1 | 33k | 0.10 |
| explosion channel 2 | 33k | 0.10 |
| explosion channel 3 | 10k | 0.33 |

Nothing downstream of that node can be attributed to one row or the other, which
is why `namco-galaga-output` and `namco-54xx-explosion` cannot be validated
independently of each other.

The op-amp runs single-supply, and the reference its non-inverting inputs sit at
comes from 3.3k over 2.2k off +5 V with a 10 uF cap: about 2.0 V.

## What it establishes

- The explosion channel is three band-passes, not one, and their centers differ
  by an order of magnitude: the 0.001 uF section is roughly ten times the
  frequency of the two 0.01 uF sections.
- The two boards' networks are identical, so one model serves both and
  `phosphor-emulator-pd5e` is wiring rather than a second circuit.
- Channel 3 is twice as loud into the mix as channels 1 and 2, its leg being
  10k against their 33k.
- The Galaga transcription's open question, whether its third section's 10k
  output joins the summing node, is answered: it does. All three legs reach the
  same vertical bus into the amplifier, which is unambiguous on the Xevious
  sheet and legible on the Galaga one.

## What it does NOT establish

- **What the 54XX actually outputs.** These are the values the analog side
  imposes on whatever the MB8844 puts on its pins; the MCU is not modeled yet
  and no capture has been compared. Every frequency implied above is arithmetic
  on component values.
- **Whether the ladder pins are the bit order assumed.** The four resistors per
  channel are 4.7k, 10k, 22k and 47k in pin order, and it is taken that the
  4.7k leg is the most significant. That is the same ordering the WSG's own
  ladder has on this board, but it was not traced through the MCU's port
  assignment.
- **Bosco.** It carries a 54XX too and no Bosco drawing was read. It is not in
  the catalog row and this file makes no claim about it.
- **The op-amp's rails in practice.** Single supply and a 2.0 V reference are
  read; whether any section clips in normal play is not established.
