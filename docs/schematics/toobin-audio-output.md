# Toobin's audio output

What Atari's Stand-Alone Audio PCB does between its YM2151, POKEY and speech
socket and the cabinet. Read for `phosphor-emulator-fuqb.1`. The model is
`machines/src/atari_jsa.rs`, shared by every JSA-I game, of which Toobin' is the
only one in the registry today.

Three results.

**The mix register's field layout is confirmed exactly**, bit for bit, against
what the model already decodes. **The output is genuinely stereo and the model
is mono**, which was already known, but the mechanism is more interesting than
"two speakers": the YM2151's own `CT1` and `CT2` output pins gate whether the
POKEY and speech reach the left channel, the right channel, or neither. And
**the fitted FM mix constant turns out to sit on the board's own resistor
ratio**, which nobody expected.

## Provenance

| | |
|---|---|
| Drawing | `Stand-Alone Audio PCB Assembly Schematic Diagram, Sheet 3`, Atari SP-320 sheet 21, 1st printing, (c) Atari Games Corporation 1988, drawing `045713-xx B` |
| Drawing | `… Sheet 4`, SP-320 sheet 22, same package, for the `/RDIO` port and coin counters |
| Drawing | `… Sheet 2`, SP-320 sheet 20, for the address decode |
| Read from | `www.arcade-museum.com/manuals-videogames/T/Toobin.pdf`, PDF pages 104-105, 106-107 and 102-103 |
| Transcribed | 2026-09-14 |

The scan has **no text layer at all**, and each schematic sheet is spread across
two PDF pages. The package starts at PDF page 62 with its contents on page 63;
sheet `n` is pages `62 + 2n` and `63 + 2n`. `poppler-utils` is in the dev shell
for this: `pdftoppm -r 300` with its `-x -y -W -H` crop flags reads one block of
a sheet where a whole sheet at 100 dpi does not.

## The mix register

`5F`, an LS273 octal latch clocked by `/MIX` and cleared by `/POR`, takes the
sound CPU's data bus and holds the whole of the board's volume control:

| Bit | Signal | Drives |
|---|---|---|
| D7, D6 | `SM2`, `SM1` | Speech volume, 2 bits |
| D5, D4 | `PM2`, `PM1` | POKEY volume, 2 bits |
| D3, D2, D1 | `YM2`, `YM1`, `YM0` | YM2151 volume, 3 bits |
| D0 | `LPF` | Low-pass filter enable |

That is exactly what `atari_jsa.rs` decodes, including the widths. Nothing to
change.

## The volume controls are resistor ladders, not multipliers

Each volume field gates binary-weighted resistors through CD4066 analog
switches, summing at an LM324 virtual ground. The weights:

| Source | Switch | Resistors | Conductance ratio |
|---|---|---|---|
| YM2151, per channel | `5C` (left), `5D` (right) | `R62`/`R69` 7.5k, `R64`/`R67` 15k, `R63`/`R68` 30k | 4 : 2 : 1 |
| POKEY | `5E` | `R70` 75k, `R71` 150k | 2 : 1 |
| Speech | `5E` | `R72` 75k, `R73` 150k | 2 : 1 |

All three ladders are binary-weighted with no offset leg, so the switched
conductance is proportional to the volume code and the model's
`code / max` is the right *shape*. This is not the Namco case where the
selected conductance was one arm of a divider and the law came out non-linear:
here the ladder sums into a virtual ground, so conductance is the gain.

## Signal path

The POKEY and the speech socket are summed **first**, into a single mono signal:

```
POKEY  --[75k/150k, PM2/PM1]--+
                              +--> 4A LM324, R38 47k feedback --> PS
Speech --[75k/150k, SM2/SM1]--+
```

`PS` is then injected into each channel's mixer through a 12k leg, and **each
leg is gated by one of the YM2151's control-output pins**: `CT1` for the left,
`CT2` for the right. The YM2151's two audio channels go into the same two
mixers through their own ladders.

```
YM left  --[7.5k/15k/30k, YM2..YM0]--+
                                     +--> 4B LM324, R43 12k fb --> R44 --> 4A LM324 --> LAUD
PS       --[12k, gated by CT1]-------+

YM right --[7.5k/15k/30k, YM2..YM0]--+
                                     +--> 4B LM324, R48 12k fb --> R47 --> 4A LM324 --> RAUD
PS       --[12k, gated by CT2]-------+
```

Both channels take the **same** three YM volume bits; there is no independent
left/right volume. The stereo placement the program actually controls is which
side `PS` lands on, through `CT1`/`CT2`.

**That is a mute path.** With both `CT1` and `CT2` clear, the POKEY and speech
reach neither speaker regardless of their volume codes. `Ym2151` gained
`ct1()`/`ct2()` for this and `atari_jsa.rs` gates on them.

Measured on Toobin', **both pins are set on every one of 2400 frames**, coined
up and in attract alike. So the game never pans the POKEY and never mutes it,
and the gating is inert here. It is modeled for the rest of the JSA-I catalog,
and because a mute is a bad thing to find out about later.

**Where the stereo is actually lost is worth being exact about, because it is
not only the downmix in `atari_jsa.rs`.** `Ym2151::drain_audio` returns a single
stream: it sums all eight FM channels and never looks at their per-channel
left/right enable bits. The FM's own panning is therefore gone before the board
model sees a sample, and recovering any of this starts in the YM2151 core rather
than in the board.

## The gain ratio, and a fitted constant that happens to be right

At maximum volume codes, into each channel's 12k feedback:

- YM ladder: `1/7.5k + 1/15k + 1/30k` = 0.2333 mS, gain `12k x 0.2333mS` = **2.80**
- `PS` leg: `12k / 12k` = **1.00**, and `PS` itself is the POKEY at
  `47k x (1/75k + 1/150k)` = `47k x 0.02mS` = **0.94**

So the board's YM-to-POKEY ratio at full volume is about **3.0 : 1**, which is
the value `YM_MIX` in `atari_jsa.rs` was already fitted to by ear.

**Do not read that as a confirmation.** The two ratios are only comparable if
our YM2151 and POKEY cores normalize their outputs the same way relative to the
chips', and nobody has checked that. What it does mean is that the fitted
constant is not obviously wrong, and that a proper leveling of the two cores
would be a cheap way to turn a lucky number into a derived one.

## The low-pass filter

Per channel, around the output amplifier:

- `C53`/`C58` 0.0022 uF across the `R45`/`R46` 12k feedback: a fixed pole at
  `1 / (2*pi*12k*0.0022u)` = **6.0 kHz**.
- `C55`/`C57` 0.001 uF shunting the `R44`-`R45` junction to analog ground:
  with `R44` 12k, **13.3 kHz**.
- `C54`/`C56` 0.0027 uF switched in parallel with that shunt by `Q5`/`Q6`
  2N3904, through `R56`/`R59` 150k. With it, the shunt is 0.0037 uF and the
  corner moves to **3.6 kHz**.

The transistor's base is driven from two 1k resistors in a wired OR: `R58`/`R60`
from `LPF`, and `R57`/`R61` from **`YM0`**, the least significant bit of the YM
volume. So the extra capacitor is switched in either when the program asks for
the filter or whenever the YM volume code is odd. The second of those is not
obviously deliberate and is worth a second read before anyone models it.

None of this is modeled. `atari_jsa.rs` applies no filter at all, and the mix
register's `LPF` bit is latched and ignored.

## The `/RDIO` port, from sheet 22

`5J` LS240 buffers eight signals onto the sound CPU's data bus when `/RDIO`:

| Bit | Signal |
|---|---|
| D7 | Self-test switch |
| D6 | `/NMI` line state |
| D5 | `SFULL`, sound output buffer full |
| D4 | `/SPHRDY`, speech chip ready |
| D3 | `COIN4` |
| D2 | `COIN3` |
| D1 | `COIN2` |
| D0 | `COIN1` |

**Bit 3 is a fourth coin input, not a tied +5V** as the model's comment says.
The board carries four coin switches (`JCOIN-1` through `JCOIN-4`), each pulled
up by 1k to VCC with 0.1 uF to ground and closing to ground.

Two coin counters hang off `CCTR1`/`CCTR2`: 2N5306 darlingtons from +14V with
1N4001 flyback diodes. Those are the `/WRIO` bits the model latches and ignores,
and ignoring them is right, since nothing about a mechanical counter reaches the
speaker or the screen.

**Unresolved: the buffer's polarity.** `5J` reads as an LS240 in this scan, which
inverts, and the board uses both LS240 and LS244 elsewhere. The model follows the
non-inverting reading for the handshake bits, which is what makes the sound
program's polling work and what the game boots with. Somebody should read that
designator at higher resolution before changing any polarity on the strength of
this transcription.
