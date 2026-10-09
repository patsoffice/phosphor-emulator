# Gyruss computer logic board (Centuri 010-4623)

Source: `arcade-museum.com/manuals-videogames/G/Gyruss__1983__Konami.pdf`
(Centuri manual), manual p27 (PDF p31),
"COMPUTER LOGIC BOARD - GYRUSS", part 010-4623, dated 5-15-83 as read on the
scan. Read 2026-10-07. Companion sheet is the sound/IO board (manual p28),
linked to this board by a 40P flat cable; see `gyruss-sound.md`.

Address decode and video sequencing live in Konami customs (501, 503, 083)
whose truth tables are not on the sheet; where the drawing goes dark the MAME
driver (`konami/gyruss.cpp`) is the fallback and is flagged as such below.

## Clocks

- 18.432 MHz crystal (bottom left, LS368 oscillator) into LS107 dual-JK
  dividers producing the 001/002/003 phases.
- Main Z80 at master/6 = 3.072 MHz, sub CPU at master/12 = 1.536 MHz, pixel
  clock at master/3 = 6.144 MHz (all MAME; the divider shape is consistent
  with /6 but was not traced gate by gate).
- Frame: 396 dots/line x 256 lines at the /3 pixel clock (MAME, PCB-measured
  H = 15.50 kHz, V = 60.56 Hz): 198 CPU cycles/line, 50688 cycles/frame,
  visible rows 16-239. Time Pilot takes its timing from this board.

## Main CPU and reset

- Z80A with address bus buffered through LS244s (AB0-AB14 on the sheet).
- Program ROM: 3x 2764 at 11J/12J/13J; the 14J socket is empty (space for a
  diagnostics ROM; the game jumps there at startup if its first byte is 0x55).
- RESET from a 555 + transistor network (19H area); a second 555 + LS293 is
  the watchdog, reset by a write to 0xC000 (MAME).
- NMI: vblank edge into the Z80, gated by LS259 Q0 (MAME `vblank_irq`).

## Main memory map (from MAME; decode is the 501 custom + LS138/LS139)

- 0x0000-0x7FFF program ROM (24 KB populated).
- 0x8000-0x83FF color RAM, 0x8400-0x87FF video RAM (1 KB each).
- 0x9000-0x9FFF work RAM.
- 0xA000-0xA7FF RAM shared with the sub CPU (seen at 0x6000-0x67FF there).
- 0xC000 read DSW2, write watchdog reset. 0xC080 read SYSTEM inputs, write
  sound-CPU IRQ trigger. 0xC0A0 P1, 0xC0C0 P2, 0xC0E0 DSW1, 0xC100 read DSW3
  and write sound latch. 0xC180-0xC187 write the LS259 main latch (3C).
- LS259 bits wired per MAME: Q0 master NMI mask, Q2/Q3 coin counters,
  Q5 flip screen. Q1/Q4/Q6/Q7 unwired.

## Sub CPU (Konami-1, 18F area)

- A Konami-1: a 6809 with scrambled opcodes (MAME `konami1` device). Its bus
  is the Q-bus on the sheet (QAB0-QAB12, QDB0-QDB7) with its own 2764 (19E,
  at 0xE000), 2114 RAMs, and the shared window onto main RAM.
- Sub map (MAME): 0x0000 scanline read, 0x2000 IRQ mask write, 0x4000-0x47FF
  RAM (sprite list at 0x4040-0x40FF), 0x6000-0x67FF shared RAM,
  0xE000-0xFFFF program ROM.
- IRQ: vblank, gated by the 0x2000 mask (MAME `vblank_irq`).
- The sub CPU cooks the sprite list: the main CPU writes queue entries into
  shared RAM and the sub CPU's program turns them into positioned sprite
  entries. No sprite-specific DMA; the 2149 line buffers (7A/7B) with LS163
  counters are the hardware side, abstracted by drawing from sprite RAM.

## Konami-1 opcode cipher (from MAME `konami1.cpp`, Olivier Galibert, BSD)

- Opcode fetches only (data reads unaffected): `val ^ xor`, where xor is
  selected by address bits 1 and 3: 0x0 -> 0x22, 0x2 -> 0x82, 0x8 -> 0x28,
  0xA -> 0x88.
- Gyruss sets no encryption boundary (`empty_init`), so the cipher applies
  at every address. Our M6809 applies it at the three opcode-fetch sites
  (fetch + the two $10/$11 second-byte reads); operands are never decoded.
- Confirmed against the dump: `gyrussk.9` opens with a near-monotonic
  descending byte pattern (b4 b2 b0 ae ...), which is cipher text, not code.

## Video

- 32x32 tilemap of 8x8 2bpp chars from one 2764 (tiles ROM). The char layout
  is the same as Time Pilot's: pixels 0-3 in byte `y`, 4-7 in byte `y + 8`,
  planes at bits 4/0, 16 bytes per char, 512 codes.
- Tile: code = video + ((color & 0x20) << 3), color = color & 0x0F,
  flip = color >> 6, category = color & 0x10. Category 1 draws opaque under
  the sprites, category 0 draws transparent over them (MAME transmasks).
- Sprites 8x16 4bpp in two banks of 256 (MAME `spritelayout`, 64 bytes each;
  planes 2/3 live 0x4000 past planes 0/1, i.e. the upper two of the four
  sprite 2764s). Bank 1 decodes 16 bytes into the region. X = ram[offs],
  Y = 241 - ram[offs+3], code/bank/color/flip per MAME `draw_sprites`
  (flip X inverted, pen 0 transparent).
- Sprite ROM order in the region: `.6 .5 .8 .7` at 0x0000/0x2000/0x4000/0x6000
  (MAME `ROM_LOAD`s; sockets 9D/8D/7D/6D on the Konami silkscreen).
- Cocktail flip mirrors both axes (declared via orientation, applied
  centrally like Time Pilot).

## Palette (read off the sheet, bottom right)

- 2A: 6331 32x8 PROM (pr3, 32 bytes). Bits 0-2 red, 3-5 green through
  1K/470/220 ladders, bits 6-7 blue through 470/220, each gun with a 470 ohm
  pulldown (MAME `resnet` weights).
- Lookup PROMs: 6F (sprites, pr1) into the lower 16 palette entries, 3E
  (chars, pr2) into the upper 16 (low nibble of each byte; only 0x40 char
  entries used). The sheet marks these "6301 1Kx4" with an "(82S129)" note
  beside 3E; neither marking matches the 256-byte dumps exactly, so the
  part number is recorded as read and the behavior follows MAME.
- The color index latch is an LS174 (3A) feeding the PROM address lines.
