# Time Pilot CPU board (KT-5207-1B)

Source: `timepilot-cpu.pdf` (Centuri C.P.U. BOARD KT-5207-1B, 11/01/82,
part 905-4897), same drawing as manual p4. Read 2026-10-06. The decode and
video sequencing live in Konami customs whose truth tables are not on the
sheet; where the drawing goes dark the MAME driver (`konami/timeplt.cpp`) is
the fallback and is flagged as such below.

## Clocks

- 18.432 MHz crystal (bottom left, LS368 oscillator) into an LS107 dual-JK
  divider producing CK1/CK1'/CK2/CK2'. The Z80 runs at master/6 = 3.072 MHz
  (MAME: "not confirmed, but common for Konami games of the era"); the LS107
  divide-by-2-then-3 shape is consistent with /6 but was not traced gate by
  gate.
- Pixel clock is master/3 = 6.144 MHz (same division as Scramble's board,
  same crystal).

## CPU and reset

- Z80A (E3). Address bus buffered through LS244 E2/E4 (AB0-AB14).
- RESET from an LS08/LS04 RC network (pin 21 RESET'); a 555 + LS293
  bottom-middle is the watchdog; MAME resets it on a 0xC200 write.

## Memory map (from MAME; decode is the K526 custom E1)

- 0x0000-0x5FFF program ROM, 3x 2764 (H2-H4 populated of four H2-H5 sockets).
- 0xA000-0xA3FF color RAM, 0xA400-0xA7FF video RAM (2114 matrix D6-D9 area).
- 0xA800-0xAFFF work RAM (2128 F9/F10).
- 0xB000/0xB400 sprite RAM banks, 256 bytes each, mirrored.
- 0xC000 read: scanline counter (vpos); write: sound-data strobe to the sound
  board latch.
- 0xC200 read DSW1, write watchdog reset. 0xC300 IN0, 0xC320 IN1, 0xC340 IN2,
  0xC360 DSW0. 0xC300-0xC30F write the LS259 main latch (B3).

## Main latch (LS259 B3) and NMI

Latch bits per MAME, consistent with the nets on the sheet (FP = flip):

- Q0 NMI enable, Q1 flip screen (inverted), Q2 sound IRQ trigger,
  Q3 mute, Q4 video enable, Q5/Q6 coin counters, Q7 pay-out (unused).
- NMI: VBLANK edge through an LS74 pair (F4/D2, CK1-synchronized) into Z80
  pin 17, gated by Q0. Modeled as asserted during vblank while enabled;
  identical to a pulse for the edge-triggered Z80 NMI.

## Video

- 32x32 tilemap, 8x8 2bpp chars from 2764 gfx ROMs (C10/C11 + F11);
  tile code = video + 0x100 color bit, color = attr & 0x1F, flip = attr >> 6,
  category = attr & 0x10 (category 0 under sprites, 1 over).
- Sprites 16x16 2bpp, RAM banks at 0xB000/0xB400; 24 used entries.
- Frame timing is not on the readable part of the sheet (H/V chains run
  through the 082/083/824 customs). Implementation takes Gyruss's
  396x256 (same crystal, same 16-239 visible window, one year apart):
  198 CPU cycles/line, 256 lines, 60.6 Hz. Revisit if raster behavior
  disagrees once ROMs arrive.

## Palette DAC (read off the sheet, top right)

- B4/B5: two 6331-1 32x8 PROMs. 15 of 16 outputs feed the guns,
  5 bits per gun through 390/470/560/820/1.2K ladders with a 1K
  pulldown per node (MSB = 390, LSB = 1.2K). The 16th bit feeds
  the video-mode decode instead of a ladder (MAME's 063 BIT VID).
- Sprite/char lookup PROMs (E9/E12, 256x4) feed PROM address lines
  through an LS298 mux (B6): sprites index pens 128+, chars 0-127
  with the +0x10 offset (MAME `palette()`; bit order there matches
  the 5/5/5 + NC split drawn here).
