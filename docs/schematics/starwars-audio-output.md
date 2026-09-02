# Star Wars's audio output

What Atari's Star Wars Sound PCB does between its four POKEYs, its speech chip
and the cabinet. Read for `phosphor-emulator-discrete-sound-fidelity-l5r3.10`,
the project-wide audit. The model is `machines/src/starwars.rs`.

This is the largest gap the sweep has found. The board has **an analog delay
line** and **a deliberate stereo matrix**, and the model has neither; and the
five sources it mixes arrive at the summing amplifier through five different
resistors, where the model uses two constants.

It is also the first board in this sweep where the model has explicit per-source
gain constants that can be held directly against the board's resistor ratios.

## Provenance

| | |
|---|---|
| Drawing | `STAR WARS Sound PCB`, Atari SP-225 sheet 16A, 2nd printing, (c) Atari Inc. 1983 |
| Drawing | `STAR WARS Sound PCB`, SP-225 sheet 16B, same printing |
| Read from | `arcade-museum.com/manuals-videogames/S/StarWars.pdf`, PDF pages 142 and 143, a 433 dpi scan |
| Transcribed | 2026-09-01 |

The schematics run from PDF page 114 to 147 and these two sheets carry all of the
audio. They are the cleanest drawings in this sweep: typeset block titles, one
function per box, and every value legible at moderate magnification.

Sheet 16A holds the generators, the buffers and the summing amplifier. Sheet 16B
holds the filter, the delay and the two output amplifiers.

## What the model does today

`StarWarsBoard::end_frame_audio` sums the four POKEYs with a single
`POKEY_GAIN = 0.20`, adds the TMS5220 with `SPEECH_GAIN = 0.50`, runs one
one-pole DC block at about 35 Hz, and scales to `i16`. Both constants are
commented as route gains taken from the reference emulator.

## The chain

![starwars audio output](starwars-audio-output.svg)

[`starwars-audio-output.json`](starwars-audio-output.json).

## The four POKEYs are not weighted equally

Each POKEY's `OUT` on pin 37 goes into a TL084 section wired as a
**transimpedance amplifier**: the chip's output current lands on a virtual
ground with a 1k feedback resistor and a 1000 pF cap across it, so
`V = -I * 1000` with a pole at 159 kHz. All four buffers are identical, at R20,
R22, R24 and R26. The speech chip instead gets a unity follower, AC-coupled
through C41 0.1 uF onto R28 100k.

The five buffered signals then reach one inverting summing amplifier with R30
12k of feedback, each through its own resistor and its own 0.1 uF:

| source | leg | gain `-R30/R` | leg high-pass |
|---|---|---|---|
| POKEY 0, `CO0` | R21 47k, C32 | **-0.2553** | 33.9 Hz |
| POKEY 1, `CO1` | R23 47k, C34 | **-0.2553** | 33.9 Hz |
| POKEY 2, `CO2` | R25 82k, C38 | **-0.1463** | 19.4 Hz |
| POKEY 3, `CO3` | R27 82k, C40 | **-0.1463** | 19.4 Hz |
| speech | R29 15k, C42 | **-0.8000** | 106.1 Hz |

Three things follow, and all three are things the model does not have.

- **Two POKEYs are 4.84 dB louder than the other two.** The model gives all four
  the same 0.20.
- **The model's 0.20 is the average of the board's four**, which are 0.2553,
  0.2553, 0.1463 and 0.1463, mean 0.2008. That is close enough that it is
  probably where the number came from rather than a coincidence, and it means
  the constant is right about the ensemble and wrong about every individual
  chip.
- **Speech sits higher against the effects on the board than in the model.**
  Board: `0.8 / 0.2008` = 3.98. Model: `0.50 / 0.20` = 2.5. The board's speech is
  **about 4.05 dB louder relative to the POKEYs** than the model's.

The per-leg capacitors also mean each source has **its own** high-pass rather
than one shared corner. The model's single 35 Hz block happens to match the two
47k legs almost exactly, is nearly twice too high for the 82k legs, and is
**three times too low for speech**, whose leg rolls off below 106 Hz. That last
one is in a range where it audibly thins a voice, and it looks deliberate.

## The analog delay, which is not modelled at all

Sheet 16B takes `SUM` into an active filter around a TL084 at 3C, built from R39
and R40 12k with C48, C49 and C50 all 0.0027 uF, and out through C51 0.47 uF as
the net **`AUD`**, with an `AUDIO T.P.` test point at TP8.

`AUD` then drives an **R5106 bucket-brigade delay line** at 3B, clocked at
**37.8 kHz** by a 556 in the `Delay Clock` block, with R44 100 and C53/C54 on its
supply, C55 0.1 uF and R45 470k on its output, and a further TL084 stage through
R46 and R47 12k with C56, C57 and C58 0.0027 uF smoothing the clock out of the
recovered signal.

An analog delay is the whole reason this board sounds the way it does, and
nothing in the model corresponds to any part of it.

## Star Wars is stereo, and the matrix is deliberate

Two TL084 sections at 2B drive two connector pins, and they do not carry the same
thing.

- **Left**, pins 6, 5 and 7: the delayed signal through **R48 22k** and `AUD`
  through **R49 22k**, both into the inverting input, with **R50 47k** feedback
  and the non-inverting input grounded. Both arrive at `-47/22` = **-2.136**.
  Out through R51 2.2k to P2 pin 1, `H 7`, `LEFT AUDIO`, with TP9.
- **Right**, pins 9, 10 and 8: the delayed signal through **R52 22k** into the
  inverting input with **R55 47k** feedback, so **-2.136** again; but `AUD`
  arrives at the *non-inverting* input through **R53 22k** with **R54 47k** to
  ground, a divider of `47/69` = 0.681 into a non-inverting gain of
  `1 + 47/22` = 3.136, which is **+2.136**. Out through R56 2.2k to P2 pin 2,
  `F 6`, `RIGHT AUDIO`, with TP10.

So, writing `D` for delayed and `A` for `AUD`:

```
LEFT  = -(D + A)
RIGHT = -(D - A)
```

The delayed signal is common to both channels and the dry signal is in
antiphase between them, at gains matched to three figures. **The dry signal lands
entirely in the difference channel and the delayed signal entirely in the sum.**
That is a difference matrix, built on purpose out of one op-amp's two inputs, and
it is what makes the cabinet sound wide.

The model is mono and has no delay, so it can produce neither channel.

## What it establishes

- The four POKEYs are mixed at two different weights, 4.84 dB apart, where the
  model uses one constant for all four.
- The model's `POKEY_GAIN` of 0.20 is the mean of the board's four legs, so it
  is right about the total and wrong about the distribution.
- The board's speech-to-effects balance is about 4.05 dB hotter than the model's.
- Each of the five legs has its own high-pass, 33.9, 33.9, 19.4, 19.4 and
  106.1 Hz, where the model has one at 35 Hz.
- Each POKEY is loaded by a **transimpedance amplifier at a virtual ground**,
  which is a third distinct POKEY interface in this sweep after Missile
  Command's 10k with 0.1 uF and Tempest's 10k with 0.015 uF.
- **There is a bucket-brigade analog delay line clocked at 37.8 kHz**, with an
  active filter before it and another after it. None of it is modelled.
- **Star Wars is stereo by construction**, with the dry signal in the difference
  channel and the delayed signal in the sum, at matched gains of 2.136.

## What it does NOT establish

- **Which of the model's `pokey[0..3]` is the sheet's `C I/O 0`.** The board
  names its own chip selects `C I/O 0 P` through `C I/O 3 P` and the outputs
  `CO0` to `CO3`, so the sheet is self-consistent, but `quad_pokey_decode`'s
  index was not traced back to 5D, 4D, 3D and 2D. It has to be settled before
  the two weights can be assigned, and it is the same question BurgerTime left
  open.
- **The delay time.** The R5106's stage count was not read off the drawing and
  its datasheet was not consulted, so the 37.8 kHz clock alone does not give a
  delay in milliseconds.
- **The filter's exact response.** R39, R40 and the three 0.0027 uF capacitors
  are an active filter of a shape that was not worked out carefully enough to
  quote a corner; the values are recorded so someone can.
- **The Empire Strikes Back.** It is in the same catalog row and runs on
  `starwars.rs`, and no ESB drawing was read. It is a conversion on this PCB set,
  which makes sharing likely, but three times in this sweep a shared board family
  has not meant a shared output stage.
- **What is after P2.** `LEFT AUDIO` and `RIGHT AUDIO` leave the Sound PCB and
  the power amplifier is on neither of these sheets.
- **Any measurement.** Nothing here has been compared against a capture.

## Confidence

The best drawings in this sweep, at 433 dpi, typeset and blocked by function.
Every resistor and capacitor above was read without ambiguity at moderate
magnification and none of it needed a pixel-level check.

The one thing worth stating as read rather than assumed is the right channel's
non-inverting input, because it is the whole stereo finding: `AUD` reaches pin 10
through R53 22k with R54 47k to ground, and pin 10 is the `+` terminal. Had it
been the `-` terminal, the two channels would carry the same signal and the board
would be mono with two outputs. The pin numbers and the `+` and `-` marks were
checked together.

One correction to a first reading, recorded because it nearly went in: R17 10k
sits between +5 V and the TMS5220's pin 21, `ADD8/DATA`. It is a pull-up on a
data pin and has nothing to do with audio. The speech output's load is R18 1800
from pin 8 to ground.

This is a hand transcription and can be wrong. Nothing in it is checked by a
test; the section above it is what keeps that honest.
