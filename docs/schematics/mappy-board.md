# Mappy (Namco 1983, Bally Midway): CPU and video boards

Schematic transcription of the Mappy board set: two 68A09E CPUs, a Namco
15XX wavetable chip with a 99XX output custom, two 58XX control customs,
and a tile/sprite video board with a 36x60 scrolling playfield. The
primary source is the Bally Midway Mappy Parts and Operating Manual
(Apr 1983), https://arcarc.xmission.com/PDF_Arcade_Bally_Midway/Mappy_Parts_and_Operating_Manual_(Apr_1983).pdf
(42 pages; schematics begin on PDF page 32). MAME's `namco/mappy.cpp`
(`mappy`, `mappy_v.cpp`) is the behavioral cross-check where the drawing
goes silent, which on this board is everywhere a custom or the mixer PAL
sits in the signal path. Designations repeat per board: CPU 5B is DIP
switch bank A while video 5B is the palette PROM.

## 1. Page index

PDF page numbers, with what was read off each. READ marks pages whose
content is transcribed below; SCAN marks pages skimmed for values only.

| PDF page | Content | Status |
|---|---|---|
| 1-9 (1-1..1-9) | Installation, controls, DIP switch A option table (difficulty, attract sounds, rack advance, freeze) | READ |
| 10 (1-10) | DIP switch B option table (coin A, bonus, lives) | READ |
| 11-31 | Parts lists, harness, power, coin door, monitor | SCAN |
| 32-33 (8-5) | CPU board schematic | READ |
| 34-35 (3-4) | CPU board parts list (component values) | READ |
| 36-37 (8-7) | Video board schematic | READ |
| 38-39 (3-6) | Video board parts list (component values) | READ |

## 2. Clock tree

X1 is an 18.432 MHz crystal with C3 100PF and R7/R10 330 forming the
oscillator around two LS368 sections at 5A. A 74LS109 at 3B divides by 3
for the 6.144 MHz dot clock. The 07XX custom takes the dot clock and
produces 1H and 2H; both 68A09Es run on 2H at 1.536 MHz. The sound CPU's
E comes from a 74LS74 at 2A re-timing the CPU clock (clock/clear nets
partially traced), so it leads the main CPU by a quarter cycle, the same
arrangement as Super Pac-Man. The 15XX voice update is the master clock
divided by 768, 24 kHz.

Consequence for an emulator: both CPUs at 1.536 MHz, pixel clock 6.144
MHz, PSG domain at master/768. Same tree as Super Pac-Man.

## 3. Video timing

384 dots per line is 96 CPU cycles; 264 lines per frame is 25344 cycles
(60.61 Hz). The visible window is 288x224 starting at dot 0, line 0.
VBLANK, and both CPUs' IRQs, start on line 224. The tube mounts rotated
90 degrees clockwise; cocktail cabinets add a 180-degree flip. All of
this matches Super Pac-Man exactly (same 07XX family behavior, same
line/frame counts).

## 4. Interrupts, reset, watchdog

### Main CPU (MC68A09E), sheet 8-5

IRQ comes from the VBLANK edge through an enable (LS259 Q1, INTON).
MAME asserts the line while the mask is set and drops it when the mask
clears; the phosphor board holds a pending flag with the same rule. The
exact flop (a 74LS74 set by the edge, cleared by INTON low) was not
traced to part level.

### Sound CPU (MC68A09E), sheet 8-5

Same VBLANK IRQ through LS259 Q0. The sound CPU sits in reset until the
main CPU raises LS259 Q5 (SUB RESET, inverted into the reset line); the
reset sequence runs before its next cycle.

### Reset circuit and watchdog, sheet 8-5

Power-on reset feeds both CPUs and the LS259 clear. The watchdog
resets on any write to 0x8000 and fires after 8 missed VBLANKS. Like
Super Pac-Man, the emulator does not model the watchdog: the program
kicks it and nothing observable follows.

## 5. Address decode

Decoding is shared between an LS138/LS139 cluster and two PALs, SPC-5
and SPC-6, which merge the main/sound strobes (LTWR and friends). The
PAL equations are not on the drawing; the map below is the drawing's
decode overlaid with MAME's memory maps, and every region in it is
exercised by the program.

### Main CPU

| Range | Use |
|---|---|
| 0x0000-0x0FFF | Video RAM (2H codes, 2J attrs, 2K x 8 each) |
| 0x1000-0x27FF | Work RAM with sprite registers at +0x780/+0xF80/+0x1780 |
| 0x3800-0x3FFF | Scroll register, write-only, value is address bits (offset >> 3) |
| 0x4000-0x43FF | 15XX shared RAM with the sound CPU (regs in the first 64 bytes) |
| 0x4800-0x480F | 58XX #0 (controls) |
| 0x4810-0x481F | 58XX #1 (DIPs) |
| 0x5000-0x500F | LS259 latch, A0 is data, A1-A3 select |
| 0x8000 | Watchdog reset (write) |
| 0xA000-0xFFFF | Program ROM, 3x8K (mpx_3.1d, mp1_2.1c, mpx_1.1b) |

### Sound CPU

| Range | Use |
|---|---|
| 0x0000-0x03FF | 15XX shared RAM (same backing as main 0x4000) |
| 0x2000-0x200F | LS259 latch (same strobe, SPC-5 merges both CPUs) |
| 0xE000-0xFFFF | Sound ROM, 8K fully populated (mp1_4.1k, no mirror) |

Note the sound ROM: Super Pac-Man carries a 4K 2732 in a socket wired
for 8K and mirrors. Mappy's mp1_4.1k is 8192 bytes, so 0xE000-0xFFFF
reads straight through.

### 74LS259 addressable latch at 2M

A1-A3 select the output, A0 is the data bit; the data byte is ignored.
Either CPU strobes it.

| Q | Name | Effect |
|---|---|---|
| 0 | SUB INT ON | Sound CPU VBLANK IRQ enable (low clears pending) |
| 1 | MAIN INT ON | Main CPU VBLANK IRQ enable (low clears pending) |
| 2 | FLIP | Cocktail flip into the video board |
| 3 | SOUND ON | 15XX sound enable (the voice latch clear) |
| 4 | 58XX RESET | Inverted: high releases both 58XX chips |
| 5 | SUB RESET | Inverted: rising edge resets the sound CPU |
| 6-7 | - | Unused |

## 6. Shared sound RAM and arbitration

The 1K at main 0x4000 / sound 0x0000 is one RAM with three ports' worth
of access: main CPU, sound CPU, and the 15XX reading its voice registers
out of the first 64 bytes. LS257/LS158 muxes slice the address lines per
phase (the inverting 158 on the high bits, as on Super Pac-Man). Neither
CPU ever waits; the phases are the same family's as Super Pac-Man
(15XX / sound / main across 1H and 2H) but were not re-derived here.

## 7. Video

### 7.1 Customs and their roles

| Custom | Site | Role, as far as the drawing shows |
|---|---|---|
| 07XX | 1D | Timing: H/V counters, sync, blanking out of the dot clock |
| 17XX | 2F | Playfield timing/position helper (POSIV-adjacent, inputs NOT FOUND) |
| 04XX | 2K | Sprite control: VSET/HSET/OBJEN/HSIZE addressing into sprite RAM |
| 12XX | 3L | Sprite ROM addressing (A30-A35, MATCH) |
| 11XX | - | Tile shifter family; exact site and pins NOT FOUND on this sheet |
| 07XX | CPU | CPU-board clock divider (same part, second instance) |

POSIV (the 2B LS32 output) and the Y0/Y1 RAM selects were chased through
several crops without landing on both inputs; they stay NOT FOUND. The
sprite position offsets (-40 X, the 256-Y/+1/-32 Y stack) live inside the
04XX/12XX and are MAME-anchored, like Super Pac-Man's.

### 7.2 RAM, ROM, and lookup parts

| Part | Site | Content |
|---|---|---|
| Tile codes | 2H, N58725P 2K x 8 | 0x0000-0x07FF of VRAM |
| Tile attrs | 2J, N58725P 2K x 8 | 0x0800-0x0FFF of VRAM |
| Sprite RAM | 2L/2M, 2K x 8 | Work RAM banks, sprite regs at top |
| Tile ROM | 3B, 2732 MPI-5 (mp1_5.3b) | 256 8x8 2bpp chars, bus INVERTED |
| Sprite ROMs | 3M/3N, 2764 (mp1_6.3m/mp1_7.3n) | Even/odd bytes of 128 16x16 4bpp sprites |
| Char LUT | 4C, 7052 MPI-6 (mp1-6.4c) | 256x4 tile color lookup |
| Sprite LUT | 5K (mp1-7.5k) | 256x4 sprite color lookup |
| Palette PROM | 5B, 7051 MPI-5 (mp1-5.5b) | 32x8 palette, A0-A4 from the PAL |
| Mixer PAL | 5D, MPI-4 | Tile/sprite/priority equations NOT FOUND (undumped) |
| Line buffers | 3E/4E, 2148 | Sprite scanline staging |
| Mux | 5E, LS298 | Buffer bank select |
| DAC | R7-R14 | 1K/470/220 red and green, 470/220 blue, to J2 |

Buffered through LS245s (1J/1L/1M/1N), decoded by the LS138 at 1F.
Address latches 3C/3D (LS174) hold the tile ROM address from the
scan/counter bus with FLIP XORed in at 3K (LS86).

### 7.3 Playfield

36 columns by 60 rows of 8x8 tiles over a 2K code/attr VRAM. Columns
0-1 and 34-35 are fixed side strips addressed out of 0x780-0x7FF with a
+2 row fold; columns 2-33 scroll vertically by the scroll register,
which loads address bits (write offset >> 3). Attribute bit 6 is the
priority flag: high tiles redraw over the sprites. Attribute bits 0-5
are the color. The mapper is MAME's `mappy_tilemap_scan` exactly,
including the +2/0x0F fold that Motos and Tower of Druaga need.

### 7.4 Sprites

64 slots, two bytes apart in three banks at work RAM +0x780 (code,
color), +0xF80 (Y, X), +0x1780 (attributes). Attribute 0 carries flip
X/Y, double width/height, and the tile LSBs; attribute 1 carries the
disable bit and X bit 8. Position math and the one-line line-buffer
delay match Super Pac-Man (`docs/schematics/sprite-list-scan.md`
covers the delay). Later slots draw over earlier ones.

Sprite pixels are 4 bits wide. A sprite pixel is transparent when its
LUT nibble is 15 (MAME's transpen mask with transcolor 15: the argument
is a color value, not a pen index). Mappy's low colors map pen 15 to
nibble 15, which is why matching pen 15's nibble looked right here and
broke on Dig Dug II, whose LUT mostly maps it to 0. Chars have no
transparency: the playfield is opaque.

### 7.5 GFX decode orders

MAME numbers plane 0 as the MSB (`planebit = 1 << (planes - 1)` in
`gfx_element::decode`); phosphor's `decode_gfx` numbers plane 0 as the
LSB. The phosphor layouts below are MAME's bit offsets in reverse, which
decodes bit-identical pixels:

- Chars: MAME planes {0,4} become `plane_offsets &[4, 0]`, x
  {64,65,66,67,0,1,2,3}, y STEP8(0,8), increment 128. The tile bus is
  inverted (MAME ROMREGION_INVERT on the tiles region), so the loader
  complements each byte before decoding.
- Sprites: MAME planes {0,4,8,12} become `&[12, 8, 4, 0]`, x
  {0-3,128-131,256-259,384-387}, y {0-56 step 8, 512-568 step 8},
  increment 1024, over the de-interleaved 16K (even bytes from 3M, odd
  from 3N, MAME ROM_LOAD16_BYTE).

Which ROM data bit the hardware calls pixel bit 0 is defined inside the
11XX/04XX/12XX customs and the undumped MPI-4 PAL: the drawing shows the
nets entering and leaving those parts but not the bit order within.
That trace is NOT FOUND (several crops, no full chain), so the order is
MAME-anchored and the golden frame is the check. If a capture ever shows
scrambled sprite colors, this trace is the first suspect.

### 7.6 Palette DAC

R9 220 / R8 470 / R7 1K red, R12 220 / R11 470 / R10 1K green, R13 220 /
R14 470 blue, PROM bits 0-2 red, 3-5 green, 6-7 blue, bit 0 toward the
1K tap. No on-board load resistor is visible on any color node between
the ladders and J2, so the node math has no load term (MAME models none
either): each gun's full drive reaches 255, blue included. R6 1K and R15
100 belong to COMP SYNC, not the colors.

The lookup mapping is the MPI-4 PAL's and is inferred from the program's
picture: chars take their lookup nibble as stored plus 0x10 (upper 16
entries, NO complement: this differs from Super Pac-Man's SPV-5), and
sprites take theirs as stored (lower 16).

## 8. Sound output chain

### 8.1 Waveform path (digital)

The 15XX reads waveforms from mp1-3.3m (256 bytes, low nibble wired),
eight voices of 4-bit samples with 4-bit volumes, updated at 24 kHz.
SOUND ON (LS259 Q3) is the voice latch clear: disabled reports (0, 0).

### 8.2 The 99XX: the schematic stops here

On the WSG boards the voice DAC is discrete (latch, ladders, 4066
switches) and every leg is on the drawing. On Mappy all of it is inside
the 99XX custom: eight digital voices in, one analog EXDATA out, law
unknown. MAME's `namco_15xx` device emits a finished mixed stream for
the same reason. The emulator assumes a linear sum of sample x volume
and says so in the code; the 99XX transfer function is NOT FOUND and
that assumption is the documented stand-in, not a schematic value.

### 8.3 Volume, coupling, power amp

EXDATA crosses R30 10K into the VR1 1K volume pot to ground, the wiper
drives C29 2.2MF 25V into LA4460 pin 2 (5N, +12V on pin 10). C29 against
roughly 10K of source puts the high-pass corner in the single Hertz, so
it is modeled as one pole near 7 Hz (VR1 assumed at maximum; the wiper
position is a trimmer setting, not a schematic value). The LA4460 runs
bridged (outputs pins 7 and 9) with a Zobel on each leg (C31/R45 and
C32/R46, 0.033MF/4.7) and supply filtering C30 47MF/63V plus C28 .01MF.
Amp and Zobels are flat across the audio band.

## 9. Inputs: the 58XX pair

Two 58XX customs (16XX family with Mappy's command numbering), modeled
in `core/src/device/namco58.rs`. 58XX mode 3 is 56XX mode 4 with the
credit nibbles swapped (0/1 vs 2/3), mode 4 is 56XX mode 9, and mode 5
is an LFSR boot check, not the 56XX sum check. See the module docs for
the full command set.

| Chip | Port A | Port B | Port C | Port D |
|---|---|---|---|---|
| #0 (controls) | Coins (coin1, coin2, -, service) | P1 (right, left on bits 1,3; bits 0,2 unused) | P2 (same, cocktail) | Buttons (P1, P2, start1, start2) |
| #1 (DIPs) | DSW2 through the mux, low/high nibble by out A bit 0 | DSW1 bits 0-3 | DSW1 bits 4-7 | DSW0 (cabinet, service mode) |

The mux is the LS257/LS158 pair on the CPU board (superpac-family
arrangement). Sticks are 2-way: only left and right reach the chip.
The program's boot check expects fixed vectors out of mode 5 (LFSR seed
0x22 with the MAME warmup/XOR order): table one at $F855
(`3 6 5 f a c e`), table two at $F85C (`8 4 6 e d 9 d`).

## 10. DIP switches

Pin levels throughout: the 58XX inverts what it reads, so a switch that
is ON reads 0. The manual's option pages call the banks DIP A (drawing
SW2 at CPU 5B) and DIP B (drawing SW3 at CPU 5E); MAME calls them SW1
and SW2. Factory setting is every switch OFF (0xFF/0xFF/0x0F).

### DSW1 (SW2, manual page 1-9)

The manual documents a 2-bit difficulty and marks SW3-5 MUST BE OFF,
but the program reads all three low bits plus coin B on bits 3-4, so
the table below is the program's superset with the manual's subset
noted. Manual ranks A-D are MAME ranks A-D exactly (SW3 OFF half).

| Bits | Setting | Values (pin levels) |
|---|---|---|
| 0-2 | Difficulty Rank A-H | A 0x07, B 0x06, C 0x05, D 0x04, E 0x03, F 0x02, G 0x01, H 0x00 |
| 3-4 | Coin B | 1C/1C 0x18, 2C/1C 0x00, 1C/5C 0x10, 1C/7C 0x08 |
| 5 | Demo sounds | On 0x20 (OFF switch), Off 0x00 |
| 6 | Rack test | Off 0x40, On 0x00 |
| 7 | Freeze | Off 0x80, On 0x00 |

Coin B's odd ratios (1C/5C, 1C/7C) are confirmed in the program's coinage
table at $D4D6: pairs (1,1),(1,5),(1,7),(2,1), read by the routine at
$FDD5 indexed from the $1362 DIP mirror. The manual's MUST BE OFF hid
real settings, not dead bits.

### DSW2 (SW3, manual page 1-10)

| Bits | Setting | Values (pin levels) |
|---|---|---|
| 0-2 | Coin A | 1C/1C 0x07, 1C/2C 0x06, 1C/3C 0x05, 1C/6C 0x04, 2C/1C 0x03, 2C/3C 0x02, 3C/1C 0x01, 3C/2C 0x00 |
| 3-5 | Bonus | 20k&70k 0x38, 20k&60k 0x30, 20k&80k 0x28, 30k&100k 0x20, 20k 0x18, 20k,70k&every 0x10, 20k,80k&every 0x08, none 0x00; second column when lives = 5 (30k&80k, 30k&100k, 30k&120k, 30k, 40k, 30k,100k&every, 40k,120k&every, none) |
| 6-7 | Lives | 3 0xC0, 5 0x80, 1 0x40, 2 0x00 |

All three tables match MAME bit for bit. The lives header in the manual
reads NUMBER OF SPACE FIGHTERS, a leftover from another game's manual;
the rows are Mappy's. The bonus column switches on lives = 5 exactly as
MAME's PORT_CONDITION says.

Coin A mostly matches its program table at $D4C6 (pairs
(1,1),(1,2),(1,3),(1,6),(2,1),(10,3),(3,1),(3,2), read at $FDBF from the
$1361 mirror): seven of eight pairs agree with the manual. Index 5, the
manual's 2C/3C row, reads (10,3) from the ROM: one byte, bit 3 off from
0x02. The game follows the ROM either way (the emulator runs the same
bytes), the option label keeps the manual's 2C/3C, and this footnote is
where the discrepancy lives.

### DSW0

Bit 2 cabinet (upright 0x04, cocktail 0x00), bit 3 service mode (off
0x08, on 0x00), bits 0-1 unused.

## 11. Program notes

- Boot check tables at $F855 and $F85C (see section 9).
- DIP mirrors at $1360-$1365 (difficulty, coin A, coin B, sounds,
  lives, bonus) copied from the $1378-$137E 58XX readback with change
  detection; coinage re-looked-up on change (JSR $FDC2/$FDD9).
- Coinage readers at $FDBF (coin A, LDX #$D4C6) and $FDD5 (coin B, LDX
  #$D4D6): LSLB stride over byte pairs.
- Service menu strings at $FCB0 (ATS/COIN/CREDIT/RANK/RND/SOUND) and
  $FEB0 (MAPPY/SCROLL/TABLE/UPRIGHT/COIN/CREDIT/ON/OFF/RANK/SOUND/
  1UP/2UP).

## 12. Things an emulator is likely to miss

- The tile bus is inverted: complement the tile ROM before decoding.
- The sprite ROMs interleave even/odd (3M even, 3N odd).
- The char LUT has no complement (unlike Super Pac-Man's ^0x0F).
- The scroll value is the write ADDRESS shifted (offset >> 3), and it
  applies to columns 2-33 only.
- The sound ROM is a full 8K: no mirror.
- The palette has no load term: blue reaches 255.
- Flip is LS259 Q2 into the video board, not a memory-mapped flop.
- The 58XXs run once a frame after VBLANK (50 us in MAME's model).
- The sticks are 2-way; up and down bits are unconnected.

## 13. What the emulator does with this

`machines/src/namco_mappy.rs` follows `superpacman.rs`: two M6809s
beside a board holding the shared address space, MappyVideo (scanline
composited), Namco15xx, a Mappy audio stage (linear 99XX stand-in, C29
pole, flat amp), and two Namco58s. DIPs are pin levels with the
tables from section 10. Everything in section 14's list is
MAME-anchored or assumed in the code and labeled as such. Dig Dug II
shares the board through `MappyVariant::DigDug2`; see
`docs/schematics/digdug2-board.md`.

## 14. NOT FOUND (consolidated)

Mechanism inside customs, the undumped PAL, or traces that never
landed, each labeled at its use site in the code:

- POSIV gate inputs (2B LS32 pins 12/13); Y0/Y1 to tile RAM selects.
- 11XX site/pins; 04XX/12XX/17XX internal logic; sprite position
  offsets (MAME-anchored).
- MPI-4 (5D) equations: LUT mapping, transparency decode, priority
  (program-picture inferred).
- GFX plane/bit order trace (MAME-decoder anchored, golden verified).
- 99XX voice mixing law (assumed linear).
- Scroll counter load path (address-as-value, MAME-anchored).
- SPC-5/SPC-6 strobe equations.
- Sub CPU Q-flop (2A) exact nets; 15XX pin 2 feed.
- IRQ flop part-level trace.
