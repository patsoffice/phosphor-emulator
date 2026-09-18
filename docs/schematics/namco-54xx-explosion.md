# The Namco 54XX explosion channel

What the Galaga-family board does between the 54XX's output pins and the summing
amplifier that also takes the WSG, and which of the MB8844's ports arrives on
each of them. Read for `phosphor-emulator-uxi9` (Galaga) and `-pd5e` (Xevious).
The network is modeled in `machines/src/namco_wsg_output.rs` and the MCU that
drives it in `core/src/device/namco54.rs`.

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
| Datasheet | Fujitsu `MB8840/MB8840H SERIES` NMOS single-chip 4-bit microcomputer, TM336-A871, January 1987 |
| Transcribed | 2026-09-17 |
| Port mapping read | 2026-09-17 |

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
| MB8844 signal | R7, R6, R5, R4 | O7, O6, O5, O4 | O3, O2, O1, O0 |
| written by | `OUT` to R-Port #1 | `OUTO` with CF = 1 | `OUTO` with CF = 0 |
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

## Which MCU port feeds which ladder

The sheet draws the 54XX as a custom block with pin numbers and no port names,
so the pin groups above only become ports once the MB8844's package pinout is
read against them. That reading is Table 1 of the Fujitsu datasheet, whose pin
numbers for the 28-pin MB8842/MB8844 are:

| signal | pins | signal | pins |
|---|---|---|---|
| O3-O0 | 7-4 | R3-R0 | 16, 15, 13, 12 |
| O7-O4 | 11-8 | R7-R4 | 20-17 |
| K3-K0 | 27-24 | R10-R8 | 23-21 |

with pin 1 `EX`, pin 2 `X`, pin 3 `/RESET`, pin 14 Vss and pin 28 Vcc. Laid over
the sheet that accounts for every pin the block draws: the three ladder groups
are R7-R4, O7-O4 and O3-O0, the eight command lines are K3-K0 on 27-24 and
R3-R0 on 16, 15, 13, 12, the three lines entering from the top are `/RESET` on
3, `R10//IRQ` on 23 and the clock on 2, and pin 1 is `EX` tied to ground because
the part is driven by an external clock on `X` rather than a resonator. Pin 14
and pin 28 are absent from the block because they are Vss and Vcc.

**Neither drawing is clean, and they correct each other.** Every row above is
Table 1 except `R10-R8`, which is Fig. 1; take each from the other where they
disagree.

- Fig. 1's MB8842/44 package drawing labels **pin 11 `R7`**, which is a typo for
  `O7`: it already has R7 on pin 20, Table 1 gives O7-O4 as pins 11-8, the
  features page calls the O-Port an 8-bit port, and the 48-pin MB8846/48 diagram
  of the same die on the same page shows an O7. Taking Fig. 1 at its word leaves
  the chip with seven O pins and two R7s.
- Table 1's MB8842/44 column gives **`R10-R8` as "23-22"**, two pin numbers for
  three signals. Fig. 1 has the third: 23 `R10//IRQ`, 22 `R9//TC`, 21 `R8`,
  which is also the only assignment that leaves no pin over.

Nothing in the ladder mapping rests on that second one. It is here because it is
the reason to read both drawings rather than whichever was found first.

**Two ladders on one port is a documented mode, not an inference.** Table 1's
O-Port entry: by the `OUTO` instruction, the four bits in the accumulator go
without conversion to the lower nibble O3-O0 or the upper nibble O7-O4
"depending on whether the carry flag (CF) is `0` or `1`". So the O port is two
independently latched nibbles, each holding while the other is written, which is
what lets one instruction drive two ladders. Fig. 3's block diagram draws them
as separate `OH Port` and `OL Port` latches behind the output PLA.

So, in the order the model reports them:

| MCU port | series leg | filter center |
|---|---|---|
| O-Port lower nibble, `OUTO` with CF = 0 | 150k | 168 Hz |
| O-Port upper nibble, `OUTO` with CF = 1 | 47k | 452 Hz |
| R-Port #1 | 100k | 2.5 kHz |

The natural reading, that the pin groups run in ladder order, is wrong, and it
is worth saying why it is so easy to reach: the groups **do** run in order down
the package, R7-R4 then O7-O4 then O3-O0, but the ladder that group feeds runs
the other way, 100k then 47k then 150k. Getting it backwards puts the busy
O-port channels through the 2.5 kHz filter and leaves the near-idle R port
driving the 168 Hz one, and the explosion comes out thin and high with no body
while every register in the chip reads correctly.

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
- **Which MCU port feeds which ladder**, from the datasheet pinout laid over the
  sheet: R-Port #1 into the 100k leg, the O port's upper nibble into the 47k and
  its lower nibble into the 150k. This was the file's open question and it is
  now read rather than borrowed.
- **The ladder bit order**, which was the file's other open question. The 4.7k
  leg is the most significant, because on every one of the three groups it sits
  on the highest-numbered bit of the nibble (R7, O7, O3) and the datasheet has
  bit 0 as the LSB of each port. The 47k leg is on R4, O4 and O0. The four legs
  come to 1 : 2.13 : 4.68 : 10, which is the 1 : 2 : 4 : 8 of a binary-weighted
  ladder in preferred values.

## What it does NOT establish

- **That the frequencies above are what the board makes.** They are arithmetic
  on component values. What the modeled chip and network together produce has
  since been compared against a reference capture, and that measurement, not
  this file, is what says the model sounds right; see the
  `namco-54xx-explosion` row in `tools/sound-compare/targets.toml`.
- **The PLA's other 30 entries.** The output PLA is mask-programmed inside the
  die and is on no drawing. The dual 4-bit mode above is a documented standard
  mode and it is the one the board is wired for, both nibbles going to their own
  ladder; what the 54XX's PLA would do with a code the firmware never emits is
  not known and does not matter here.
- **Bosco.** It carries a 54XX too and no Bosco drawing was read. It is not in
  the catalog row and this file makes no claim about it.
- **The op-amp's rails in practice.** Single supply and a 2.0 V reference are
  read; whether any section clips in normal play is not established.
