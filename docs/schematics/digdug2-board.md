# Dig Dug II (Namco 1985): Mappy hardware, no schematics

Dig Dug II runs on the Mappy board set: two 68A09Es, a Namco 15XX
wavetable chip with a 99XX output custom, the 07XX-timed scrolling
36x60 playfield, and the same LS259 latch and address decode. The
scrolling playfield gives it away, and MAME agrees: `digdug2` lives in
`src/mame/namco/mappy.cpp`, shares `mappy_common` (CPUs, clocks,
`mappy_main_map`, `superpac_sub_map`, video, palette), and differs only
in its ROMs, its input ports, and one MCU swap.

No schematics of this game are known, so there is no page index and no
part-level trace here. The source of truth is MAME's `digdug2`
machine config, input ports, and ROM definitions, read against the
Mappy manual transcription in `docs/schematics/mappy-board.md`. Every
hardware claim below that is not a ROM fact is MAME-anchored or
inherited from the Mappy board, and labeled as such.

## 1. What is shared with Mappy

Everything not named in section 2, by way of MAME's shared config:

- Clock tree, video timing, and the rotated tube: mappy-board sections
  2 and 3. Both CPUs at 1.536 MHz, 384x264 at 60.61 Hz, VBLANK IRQs
  through the LS259 enables.
- Interrupts, reset, and the sub-CPU hold: section 4. The sound CPU
  still sits in reset until the main CPU raises SUB RESET.
- Address decode: section 5, with the ROM window widened (section 2.1).
  Video RAM, work RAM with embedded sprite registers, the scroll
  register, shared sound RAM, the two MCU windows at 0x4800/0x4810,
  and the LS259 at 0x5000/0x2000 all sit where Mappy puts them.
- Video pipeline: section 7 entire. Same playfield mapper (including
  the +2/0x0F side-strip fold), same sprite register layout and
  position math, same GFX decode orders with the inverted tile bus,
  same palette DAC with no load term, same lookup mapping (chars
  +0x10, sprites as stored).
- Sound chain: section 8 entire. The 15XX voices, the linear 99XX
  stand-in, and the output network are unchanged; only the waveform
  PROM dump differs.
- The 58XX on MCU #0: mappy-board section 9, controls side. Coins,
  sticks, and buttons reach the same ports.

## 2. What differs

### 2.1 Program ROM fills 0x8000-0xFFFF

MAME's `mappy_main_map` already maps ROM from 0x8000 with the comment
"only a000-ffff in Mappy". Dig Dug II populates the whole window with
two 16K ROMs (the `digdug2` New Ver. set):

| ROM | Address | CRC32 |
|---|---|---|
| d23_3.1d | 0x8000-0xBFFF | cc155338 |
| d23_1.1b | 0xC000-0xFFFF | 40e46af8 |

The Old Ver. set (`digdug2o`: d21_3.1d, d21_1.1b, plus a different
sprite LUT MAME flags as a possible bad dump) is not supported; the
emulator pins the New Ver. dumps above.

### 2.2 Sound ROM is 8K at 0xE000

Same map as Mappy's (`superpac_sub_map`), different dump: d21_4.1k,
0x2000 bytes, CRC32 737443b1, fully populated with no mirror.

### 2.3 Tile ROM, same inversion

d21_5.3b, 0x1000 bytes, CRC32 afcb4509. MAME marks the region
ROMREGION_INVERT exactly as on Mappy, so the loader complements each
byte before decoding the 256 8x8 2bpp chars.

### 2.4 Sprite ROMs are doubled

Two 16K ROMs instead of Mappy's two 8K, still even/odd interleaved
(MAME ROM_LOAD16_BYTE), giving 256 16x16 4bpp codes:

| ROM | Half | CRC32 |
|---|---|---|
| d21_6.3m | even bytes | df1f4ad8 |
| d21_7.3n | odd bytes | ccadb3ea |

The decode layout is Mappy's `spritelayout_4bpp` unchanged.

### 2.5 PROMs

Same 0x0220 layout as Mappy (palette 0x20, char LUT 0x100, sprite LUT
0x100), different dumps:

| ROM | Use | CRC32 |
|---|---|---|
| d21-5.5b | palette | 9b169db5 |
| d21-6.4c | char lookup | 55a88695 |
| d21-7.5k | sprite lookup | 9c55feda |

And the 15XX waveform PROM: d21-3.3m, 0x100 bytes, CRC32 e0074ee2.

### 2.6 MCU #1 is a 56XX, not a 58XX

MAME's `digdug2(config)` fits NAMCO_58XX at `m_namcoio[0]` and
NAMCO_56XX at `m_namcoio[1]`. The 56XX is the older 16XX-family part
(modeled in `core/src/device/namco56.rs`); its DIP-side protocol
matches what the 58XX does in 58XX mode 3, and its boot check is the
56XX sum check rather than the 58XX LFSR. The port wiring on #1 is
unchanged (muxed DSW2 on A, DSW1 halves on B and C, mixed port D),
and the boot program reads its DIPs through the same mux protocol.

### 2.7 MCU #1 port D mixes buttons with DIPs

On Mappy, port D of the DIP MCU carries DSW0 alone. On Dig Dug II,
MAME's DSW0 port carries live buttons beside the cabinet DIP:

| Bit | Use |
|---|---|
| 0 | P1 drill (button 2) |
| 1 | P2 drill (button 2, cocktail) |
| 2 | Cabinet DIP (upright 0x04, cocktail 0x00) |
| 3 | Service-mode button (distinct from the service coin on MCU #0) |

All four are active low. The drill buttons and the service-mode button
are inputs, not DIPs, so they default high and the cabinet bit keeps
its upright default.

### 2.8 The watchdog write is a no-op

MAME's `init_digdug2` replaces the 0x8000 watchdog write with
`nop_write`: "appears to not use the watchdog". The phosphor board
does not model the watchdog on any Mappy-family game, so there is
nothing further to do; the note is here so nobody goes looking for a
kick the program never sends.

## 3. Inputs

MAME's `NAMCO_56IN0`/`NAMCO_56IN1` macros on MCU #0, identical in shape
to Super Pac-Man's: 4-way sticks on ports B (P1) and C (P2), pump on
BUTTONS bits 0-1, starts on BUTTONS bits 2-3, coins and the service
coin on the COINS port. The second button (drill) is the one that
moved: it sits on MCU #1 port D (section 2.7), not on the BUTTONS
port, which is why the board carries a separate `in_buttons2` nibble.

## 4. DIP switches

Pin levels throughout, as on Mappy: the MCU inverts what it reads, so
a switch that is ON reads 0. MAME-sourced; no manual is known.

### DSW1 (SW1, the only option bank)

| Bits | Setting | Values (pin levels) |
|---|---|---|
| 0 | Service mode | Off 0x01, On 0x00 |
| 1 | Lives | 3 0x02, 5 0x00 |
| 2-3 | Coinage | 1C/1C 0x0C, 2C/1C 0x08, 1C/2C 0x04, 3C/1C 0x00 |
| 4-5 | Bonus life | 30k 80k and ... 0x30, 30k 100k and ... 0x20, 30k 120k and ... 0x10, 30k 150k and ... 0x00 |
| 6 | Level select | Off 0x40, On 0x00 |
| 7 | Freeze | Off 0x80, On 0x00 |

### DSW2 (SW2, entirely unused)

MAME marks all eight bits DIPUNUSED. The field stays at its 0xFF
default and the game exposes no bank for it.

### DSW0

Section 2.7's mix: drill buttons on bits 0-1, the cabinet DIP on bit
2, the service-mode button on bit 3.

## 5. The transparency lesson this game taught

Sprite pixels are 4 bits wide, and a sprite pixel is transparent when
its LUT nibble is 15. That rule lives in MAME's palette code
(`transpen_mask` in `src/emu/dipalette.cpp` compares indirect pens to
the transcolor), not in the driver, which is why it was first read as
"pen index 15 is transparent". Mappy's low colors map pen 15 to nibble
15, so the wrong rule looked right on Mappy and broke here, where the
LUT mostly maps pen 15 to 0 and the sprites arrived with black boxes.
The fix matches the LUT nibble against 0x0F; mappy-board section 7.4
states the corrected rule for both games.

## 6. Things an emulator is likely to miss

- The program ROM starts at 0x8000, not 0xA000; Mappy's window is the
  special case, not this one.
- MCU #1 is a 56XX. Fitting a second 58XX boots (the DIP protocol
  overlaps) but answers the wrong boot check.
- The drill buttons are on MCU #1 port D, not on the BUTTONS port with
  pump and the starts.
- DSW2 is unused: leave it 0xFF and expose no bank.
- The service-mode button is a live input on port D bit 3, distinct
  from both the service coin and the DSW1 service-mode DIP.
- Transparency is LUT nibble 15, not pen index 15 (section 5).
- The sprite ROMs are twice Mappy's: 256 codes, same layout.
- The tile bus is inverted, as on Mappy.

## 7. What the emulator does with this

`machines/src/digdug2.rs` holds only the ROMs, DIPs, controls, and
system glue; everything shared lives in `machines/src/namco_mappy.rs`
parameterized by `MappyVariant::DigDug2`. The variant selects the ROM
base (0x8000), fits a 56XX in the MCU #1 slot, and mixes port D per
section 2.7. DIPs are pin levels with the tables from section 4.

## 8. NOT FOUND (consolidated)

There are no schematics, so every board-level claim inherits the
Mappy manual trace or a MAME anchor; the Mappy list
(mappy-board section 14) applies in full. Items this game adds:

- The 56XX-in-#1-slot wiring: MAME machine-config anchored only.
- The port D button mix: MAME input-port anchored only.
- The Old Ver. sprite LUT (d2x-7.5k): possibly a bad dump per MAME;
  unsupported either way.
- Any board-revision differences between the Mappy and Dig Dug II
  PCB sets (silkscreen, socket population, analog values): unknown.
