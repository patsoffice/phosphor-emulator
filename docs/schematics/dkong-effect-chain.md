# Donkey Kong's effect chain

What sits between an effect's oscillator and the summing bus on Donkey Kong's
CPU board, and it is the shape of that path rather than the whole analog section
that this file is for. The model is `machines/src/dkong_sound.rs`.

Read for `phosphor-emulator-dkong-sound-excerpt-yfbx`, which exists because that
file asserts specific component values throughout and nothing recorded where any
of them came from. The load-bearing claim it makes is a topology claim, that the
effects are **diode-mixed with their source rather than multiplied by it**, and a
drawing is the only thing that can settle a topology claim. A spectral fit is
evidence for it and not proof.

## Provenance

| | |
|---|---|
| Drawing | `TKG4-14-CPU`, sheet 3, Nintendo Co. Ltd., dated 5,56,12,1 |
| Read from | `arcade-museum.com/manuals-videogames/D/dk-tkg4u.pdf`, PDF pp29-30 |
| Transcribed | 2026-09-18 |

The scan quirks are already written up in `machines/src/tkg04.rs`'s header and
are repeated here only where they cost a pass: this is a 300 dpi 1-bit scan with
no text layer, so nothing in it is searchable, and the sheet is **cut across two
PDF pages, left half then right half**, so a sheet number and a PDF page never
agree. The whole analog section is on the right half, p30, in grid columns 11 to
16.

## Two channels, identical but for one resistor

The board carries the chain twice, and reading both is what makes the shape
trustworthy: a single reading of one channel could be a misread, two channels
that come out the same cannot both be misread the same way.

| stage | upper channel | lower channel |
|---|---|---|
| source coupling | `C18` 1 uF | `C21` 1 uF |
| base resistor | `R31` 10k | `R9` 10k |
| base clamp to ground | `D9` 1SS53 | `D4` 1SS53 |
| switching transistor | `Q5` 2SC1815 | `Q2` 2SC1815 |
| collector load | `R30` 100k | `R8` 100k |
| collector series | `R29` 10k | `R7` 10k |
| **mix diode, from this channel** | `D6` 1SS53 | `D1` 1SS53 |
| **mix diode(s), from the envelope** | `D7` 1SS53 | `D2`, `D3` 1SS53 |
| node capacitor | `C17` 4.7 uF | `C20` 3.3 uF |
| emitter follower | `Q4` 2SC1815 | `Q1` 2SC1815 |
| emitter load to ground | `R28` 4.7k | `R6` 4.7k |
| first series | `R27` **150** | `R5` **750** |
| first shunt | `C16` 1 uF | `C19` 1 uF |
| second series | `R26` 2k | `R4` 2k |
| second shunt | `R25` 5.1k | `R3` 5.1k |
| into the summing bus | `R24` 47k | `R2` 47k |

**The one asymmetry is `R27` 150 against `R5` 750.** Every other value matches
across the two channels, which is what makes that one worth pointing at rather
than filing as a misread: it was checked at full scale on both.

## The chain

```mermaid
flowchart LR
    SRC["source<br/>(oscillator)"] --> C18["C18 1u"]
    C18 --> R31["R31 10k"]
    R31 --> Q5["Q5 2SC1815<br/>switch"]
    D9["D9 1SS53<br/>base clamp"] -.-> Q5
    Q5 --> R29["R29 10k"]
    R29 --> D6["D6 1SS53"]
    ENV["envelope"] --> D7["D7 1SS53"]
    D6 --> NODE(["mix node<br/>C17 4.7u"])
    D7 --> NODE
    NODE --> Q4["Q4 2SC1815<br/>emitter follower"]
    Q4 --> DIV["R27 150 / C16 1u<br/>R26 2k / R25 5.1k"]
    DIV --> R24["R24 47k"]
    R24 --> BUS(["summing bus"])
```

The lower channel is the same graph with `C21 R9 Q2 D4 R7 D1 D2 D3 C20 Q1 R5 C19
R4 R3 R2`.

## Nets

Only the nets the chain needs. `ref.pin` where the drawing gives a pin.

| net | members |
|---|---|
| upper mix node | `D6` cathode, `D7` cathode, `C17`+, `Q4` base |
| lower mix node | `D1` cathode, `D2` cathode, `D3` cathode, `C20`+, `Q1` base |
| upper emitter | `Q4` emitter, `R28`, `R27` |
| lower emitter | `Q1` emitter, `R6`, `R5` |
| summing bus | `R24`, `R2`, and the other effect legs, to `R1` 47k and `VR2` 10k |
| NE556 half A | `R42` 47k, `R43` 27k to `8N.1`, `8N.2`; `C28` 0.033 uF on `8N.6` |
| NE556 half B | `R40` 47k, `R41` 27k to `8N.13`, `8N.12`; `C27` 0.047 uF on `8N.8` |
| DAC decay | `Q7` 2SC1815 emitter, `C32` 10 uF to ground, `R20` 10k in series |
| DAC collector load | `R37` 10k, from the supply to `Q7` collector |

## What it establishes

- **The effects are diode-mixed, not multiplied.** Two diodes meet at one node
  that feeds a transistor base: `D6` and `D7` on the upper channel, `D1` with
  `D2` and `D3` on the lower. Two diodes into a common node is a mix, and there
  is no multiplying element anywhere on the path. `dkong_sound.rs` lines 18-24
  assert exactly this and the drawing agrees, so that claim is no longer resting
  on a spectral fit.
- **The stage after the mix is an emitter follower into a resistive divider**,
  which is the rest of the shape the model assumes: `Q4`/`Q1` with their emitter
  load, then two series-shunt sections, then a 47k into the shared bus. The 47k
  is what sets each effect's weight in the mix.
- **The two 555 pairs are 47k / 27k with 0.033 uF and 0.047 uF**, `R42`/`R43`/`C28`
  and `R40`/`R41`/`C27` on the `NE556` at `8N`. These two were already confirmed
  by hand on 2026-08-30; they are repeated here so the file is self-contained.
- **The DAC's decay is `C32` 10 uF discharging through `R20` 10k**, which is the
  100 ms time constant `DAC_DECAY_S` uses. The code's "10 kOhm across 10 uF" is
  right in value. Note that **`R37`, also 10k, is `Q7`'s collector load and not
  the discharge path**; the issue text named `R37` for the decay, and the two
  being the same value is why that would never have shown up as a wrong number.

## What it does NOT establish

- **Which channel is which effect.** The two are described here as upper and
  lower because that is how they sit on the sheet. Tying them to "jump" and
  "stomp" needs the sound CPU's port decode traced to `C18` and `C21`, which was
  not done. The topology claim above holds either way, which is why this was left
  rather than guessed.
- **Why `R27` is 150 and `R5` is 750.** The asymmetry is read, not explained. It
  makes one channel's follower see a different load, so the two effects do not
  arrive at the bus at the same level, but what that ratio should sound like is
  not something this sheet says.
- **The chopper claim.** `dkong_sound.rs` lines 82-89 argue the oscillator chops
  the envelope rather than multiplying it. The diodes above are consistent with
  chopping and inconsistent with a multiply, but the specific one-sided-pulse
  reasoning in that comment is about the waveform, and a waveform is not
  something a schematic shows.
- **Anything about the 4049 oscillators, the `MB3614` sections or the `MB3712`.**
  They are on the same sheet and were not read. The full analog section is an
  NE556, two 4049 oscillators, seven 2SC1815, two MB3614 quads and an MB3712, and
  transcribing all of it is the "arcade motherboard" `README.md` warns against.
- **A rendered netlist.** The issue asked for a `dkong-effect-chain.json` beside
  this file for the NE556 and 4049 half. That half is not read yet, so there is
  nothing to render; the mermaid above covers the part that carries the claim.

## Confidence

A 300 dpi 1-bit scan, and legible at full scale without pixel-level guessing
once the right half is cropped. Every designator and value in the table was read
at magnification, and the two channels were read independently and compared
afterwards rather than one being assumed from the other.

Two readings were corrected during the pass and are worth recording because both
would have produced a plausible-looking wrong table. The upper divider's second
series resistor reads `R26` 2k, not a second `R25`; and the decay path is `R20`,
not `R37`, which sits on the other side of `Q7`.

This is a hand transcription and can be wrong. Nothing in it is checked by a
test; the section above it is what keeps that honest.
