# Congo Bongo's percussion is a circuit, not a sample set

What actually generates Congo Bongo's five percussion voices. Written because
the project had recorded the opposite, and the mistake had reached a plan.

Read for `phosphor-emulator-7z54`. The device that will be rebuilt from it is
`machines/src/congo_sound.rs`.

## Provenance

| | |
|---|---|
| Drawing | `Sound Board 834-5168 rev A`, sheets 1 and 2 of 2, Gremlin/SEGA, drawn 3-17-83 and 3-22-83 |
| Read from | `arcade-museum.com/manuals-videogames/C/congobongo5.PDF`, PDF pp15-18 |
| Transcribed | 2026-08-30, from a 150 dpi render; at pin level 2026-09-25 |
| Netlist | [`netlists/congo-sound.toml`](netlists/congo-sound.toml), the whole audio path |

Each sheet is spread across two PDF pages that overlap at the seam: sheet 1 is
pp15-16, sheet 2 is pp17-18. The percussion is on p18 and the gorilla on p17.

**Read again at pin level on 2026-09-25, and this note was wrong in four
places**, each corrected below where it stands: the drums are bridged-T
resonators, not multiple-feedback band-passes; each drum has its own bias
divider; the gorilla is an oscillator, not a noise burst; and SJ is a virtual
ground, which settles the balance question this note left open. The scan
itself is not what this note said either. Each page is a 1013 x 1318 8-bit
grayscale JPEG, 120 dpi on the letter page, and every page of the sound board
is **flipped top to bottom**, so anything with a direction has to be read after
flipping it. Grayscale holds up better than the number suggests.

This closes `congo_bongo` on the "no known schematic source at all" list in
`phosphor-emulator-aih3`.

## Why this file exists

The coverage catalog recorded this target as one whose *reference* is sample
playback, and the epic's phase list put it first on exactly that reasoning: that
comparing against recorded WAVs would exercise the claim that the tooling does
not care where a WAV came from.

That is wrong, and it is wrong in the direction that matters. **The board
synthesizes these voices in analog hardware.** The reference emulator plays
recorded samples because it never modelled the circuit, not because the circuit
is a sample player. Reading the drawing took twenty minutes and the belief had
been in the plan for weeks.

The general form is the one this project keeps relearning: *an emulator's
implementation choice is not evidence about the hardware.* A sample set is what
somebody did instead of reading this sheet.

## The four drum voices

Bass drum, low conga, high conga and rim are **the same circuit four times over**
with different component values. One PPI bit each, and the chain is:

```mermaid
flowchart LR
  PPI["i8255 PPI bit<br/>active low"] --> INV
  INV["U6 7416<br/>open collector<br/>10k pull-up to +12V"] --> C["coupling cap"]
  C --> D["1S2075 diode, cathode toward the cap<br/>+ a resistor to ground each side"]
  D --> C2["second cap"]
  C2 --> R["series resistor"]
  R --> BP["U13 3614 + input<br/>resistor to ground, bias divider<br/>bridged-T feedback on the - input"]
  BP --> OUT["coupling cap<br/>+ mixing resistor"]
  OUT --> BUS["SJ, the second summer's<br/>virtual ground on sheet 1"]
```

The front end is an envelope shaper: the inverter's falling edge is
differentiated by the coupling capacitor, the diode passes only that edge, and
the RC pair sets how fast it decays. The shaped pulse enters the op-amp's
**non-inverting** input. The feedback is a **bridged-T**: the "1M" (470k on the
bass) from the output back to the inverting input, the two equal capacitors in
series across it, and the "small resistor" from their junction to ground. That
network makes the stage **ring** at one frequency. A pulse into a ringing
resonator is how you build a drum out of parts, and it is the whole voice.

*Corrected 2026-09-25.* This section first called the op-amp stage a
multiple-feedback band-pass fed at the inverting input. The parts were right
and the filter was wrong, which is the difference between a block reading and
a pin-level one.

### What the resonators solve to

With the four U13 sections marked as ideal op-amps in the netlist, the solver
gives each voice's resonance from the parts alone
(`tools/netlist/tests/congo_ac_test.rs`):

| | bass drum | conga (L) | conga (H) | rim |
|---|---|---|---|---|
| resonance | **73.4 Hz** | **265.5 Hz** | **325.2 Hz** | **1079.9 Hz** |
| Q | 10.8 | 27.4 | 33.6 | 23.3 |
| ring time constant, Q/(pi f) | 47 ms | 33 ms | 33 ms | 6.9 ms |

The device plays the congas at 160 and 250 Hz and the rim as low-passed noise,
so all three were tuned by ear to something the board does not do. These
figures take the 3614 as a conventional op-amp, which is unconfirmed, and they
are small-signal: how hard each shaped pulse drives its resonator, and whether
the 3614 clips on the way, are not in them.

### The values, per voice

| | bass drum | conga (L) | conga (H) | rim |
|---|---|---|---|---|
| PPI bit | PC0 | PC1 | PC2 | PC3 |
| U6 7416 section | 9 -> 8 | 5 -> 6 | 13 -> 12 | 11 -> 10 |
| pull-up | R21 10k | R31 10k | R41 10k | R51 10k |
| input cap | C20 68n | C26 68n | C32 68n | C38 10n |
| shaper resistors | R22 47k, R23 47k | R32 47k, R33 47k | R42 47k, R43 47k | R52 22k, R53 22k |
| diode | D1 1S2075 | D2 1S2075 | D3 1S2075 | D4 1S2075 |
| second cap | C21 1u | C27 33n | C33 33n | C39 3n3 |
| series resistor | R24 10k | R34 47k | R44 47k | R54 22k |
| U13 3614 section, + / - / out | 12 / 13 / 14 | 10 / 9 / 8 | 3 / 2 / 1 | 5 / 6 / 7 |
| feedback resistor | R28 470k | R38 1M | R48 1M | R58 1M |
| feedback caps | C23 100n, C24 100n | C30 33n, C31 33n | C35 33n, C36 33n | C41 6n8, C42 6n8 |
| tuning resistor | R29 1k | R39 330R | R49 220R | R59 470R |
| + input to ground | R25 47k | R35 47k | R45 47k | R55 22k |
| bias: +12 V, divider, decoupling, to + input | R27 10k, C22 47u, R26 22k | R37 10k, C28 47u, R36 22k | R47 10k, C34 47u, R46 22k | R57 4k7, C40 2u2, R56 10k |
| output cap | C25 1u | C29 1u | C37 1u | C44 1u |
| **mixing resistor** | **R30 240k** | **R40 390k** | **R50 390k** | **R62 200k** |

The rim has R60 2.2M, R61 2.2M and C43 47n around its section that the other
three do not, which is why it is the odd one: R60 and R61 run in series from
the inverting input to the output with C43 from their junction to ground, a
second T in parallel with the bridged-T, which makes its feedback a twin-T.

*The bias row is corrected 2026-09-25.* Each voice has its own divider from
+12 V into its + input, decoupled by an electrolytic; the row first had R37,
which is conga (L)'s, under the bass.

**The mixing resistors are the balance**, and they are the single most useful
row: 240k, 390k, 390k and 200k into a common node say the rim is loudest, the
bass next, and the two congas equal and quietest, with about 5.8 dB between the
extremes. Nothing about that is a matter of taste.

## The gorilla

A different circuit, on p17, and the only voice that is not one of the four
above. It runs from PB1 through two 4538B monostables (U19, both halves, with
R70 100k / C52 1u and R71 150k / C53 1u), 4001B gating at U18, diodes D6 and D7
(1S2075), and several 3614 sections at U16 and U17, into C59 10u and then U15,
marked `G501534`. Its output leaves through C61 1u and R94 51k to the same mix
bus.

A **HM5837** at U20 supplies noise to that side of the sheet, through R63 22k /
R64 22k and C47 470p into a 4001B at U18. That is a dedicated digital noise-
generator IC, and it is the board's only noise source.

*Read at pin level 2026-09-25, the gorilla is an oscillator, not a noise
burst.* U17 pins 2/3/1 integrate a control voltage, U16 pins 6/5/7 are a
Schmitt trigger with hysteresis through R87, and the Schmitt switches Q2
(2SC1684), whose collector through R85 reverses the integrator's ramp: a
relaxation oscillator. Its rate follows the control voltage from the summer at
U17 pins 13/12/14, which adds a pitch envelope (the NOR of the two monostables,
charging C55 through D6 and R73 and discharging through R72) to the noise,
third-order low-passed at U17 pins 10/9/8. The square wave is scaled by
U16 pins 2/3/1, low-passed at U16 pins 12/13/14, and then goes through U15.

**U15 is a VCA in function**, which answers the question below as far as the
drawing can: its CY pin is driven by the second monostable's envelope (C54,
charged through D7 and R74, discharged through R75, buffered by U16 pins
10/9/8). That wire crosses the seam between p17 and p18 and is the least
certain junction on the sheet. The part number `G501534` is still not
identified, and neither is what its RD pin and C60 do.

## What it establishes

- **All five voices are analog circuits on the board.** There is no sample ROM
  and no sample player. The catalog's ordering rationale for this target was
  false.
- **Four of the five are one design with four sets of values**, so modelling one
  correctly gets four, and getting the topology wrong gets all four wrong
  together.
- **The voices are ringing band-pass filters excited by a shaped pulse.** The
  current model synthesizes them as damped sine oscillators and enveloped noise,
  which is a different thing that can be made to sound similar. A damped sine is
  what a ringing band-pass produces, so the model is not far off in behaviour;
  it is far off in that no constant in it corresponds to a part.
- **The relative levels are the four mixing resistors**, not a matter of ear.
- **The trigger polarity is confirmed**: each voice hangs off a 7416 inverter
  driven by its PPI bit, and the current model's mapping (gorilla PB1, bass PC0,
  conga low PC1, conga high PC2, rim PC3) matches the drawing's labels exactly.
- **The board's noise source is one HM5837**, shared, where the model gives the
  rim and the gorilla an LFSR each.

## What it does NOT establish

- **Any centre frequency or decay time.** The values are transcribed but not
  solved. A multiple-feedback band-pass's centre and Q come from the two
  capacitors, the feedback resistor, the series resistor and the resistor to
  ground together, and none of that arithmetic has been done here. That is the
  first work of the rebuild, not of this note. *Solved 2026-09-25, for the
  bridged-T each stage actually is; see "What the resonators solve to".*
- **What U15 `G501534` is.** It sits in the gorilla's path after a 10 uF
  coupling capacitor, with pins marked IN, OUT, VCC, GND, CY and RD. It was not
  identified, and until it is, the gorilla's chain is not understood.
- **The gorilla at component level.** Its two monostables' timings, its gating,
  and how the noise reaches it were read well enough to say what the parts are
  and not well enough to model them.
- **Where SJ goes.** The percussion mix bus is labelled `SJ SH1` on sheet 2 and
  `SJ SH2` on sheet 1, joining the node R20 20k drives from the PSG chain. The
  relative level of percussion against the two SN76489As therefore depends on
  the PSG chain's output impedance, which was not worked out. Sheet 1's PSG path
  is U5 and U4 76489A -> R13/R15 10R -> C12/C13 100n -> C14/C15 1u -> R16/R17
  51k -> U12 3614 with R18 100k -> R19 51k -> U12 3614 with R20 20k -> `SOU`.
  *Settled 2026-09-25, and the concern was wrong: SJ is U12 pin 13, the second
  summer's inverting input and so a virtual ground. Each voice reaches `SOU` at
  R20 over its own mixing resistor (bass 20/240, congas 20/390, rim 20/200,
  gorilla 20/51) and each PSG at (R18/R16)(R20/R19), 0.769, with no dependence
  on any output impedance. The PSG path above also misplaces two parts: each
  PSG output has a series RC to ground (C12 with R14 10R, C13 with R15 10R),
  and R13 ties PSG 2's IN pin to ground with no value printed.*
- **Whether any of this matches what the emulator currently plays.** Nothing was
  measured. The claim here is about the drawing only.
- **The op-amp part.** `3614` is written on every section; whether that is an
  LM3614, a Norton amplifier, or a house number was not chased, and its input
  structure decides whether the summing nodes above are virtual grounds.

## Confidence

A clean scan, read at 150 dpi, where every designator and value in the table
above was legible without magnification. The four-voice table is a
straightforward read of one well-drawn sheet and is the part to trust.

The gorilla section and the mix bus are read at block level rather than
component level, and are marked so above.

This is a hand transcription and can be wrong. Nothing in it is checked by a
test; the section above it is what keeps that honest.

*Since 2026-09-25 the pin-level netlist is the authority and this note is its
argument.* The netlist loads and lints clean, carries every designator on
sheet 2 with no gaps (R21-R94, C20-C62, D1-D8) and sheet 1's audio parts, and
`congo_ac_test.rs` holds the resonances and the balance at `SOU`. The
readings to doubt are the ones a 120 dpi grayscale scan makes hardest: the
junction dots, and above all the gate envelope's wire into U15 across the page
seam.
