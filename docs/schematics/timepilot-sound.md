# Time Pilot sound board (KT-5112-2B)

Source: `timepilot-sound.pdf` (SOUND BOARD KT-5112-2B, part 905-4896),
read 2026-10-06 at 300 DPI. The manual (PDF pp7-8) carries the same
drawing but with index pins misread; the standalone sound PDF is the
authority for pin-level detail.

## Clocks and CPU

- 14.31818 MHz crystal divided by an LS163 + LS74 chain (clock box
  top left) to 1.7897725 MHz at the Z80 (pin 6; MAME crystal/8).
- Z80 (D6), 4K program ROM tm7 (D7), 1K work RAM (C7).

## Memory map

From MAME `timeplt_sound_map` (the Loco-Motion map differs; this is the
Time Pilot one), consistent with the SEN1/SEN2 select decode drawn around
LS00 E6 / LS32 E5:

- 0x0000-0x2FFF program ROM (tm7 populates 0x0000-0x0FFF; socket D7).
- 0x3000-0x33FF work RAM, 1K mirrored by 0x0C00.
- 0x4xxx AY1 data, 0x5xxx AY1 address, 0x6xxx AY2 data, 0x7xxx AY2
  address (low 12 bits don't-care within each page).
- 0x8000-0xFFFF write: filter select; the address carries 12 bits,
  2 per AY channel (MAME `filter_w`).

## Sound latch and IRQ (sheet-confirmed)

- SOUND DATA (edge pin 32, strobed by a main-board 0xC000 write)
  buffers through an LS367 and clocks an LS273 (G6) whose D
  inputs are the main data bus (DC0-DC7). Q outputs feed AY1
  port A (A7-A0) directly: the command latch is read as AY1
  port A, as MAME wires it.
- IRQ: SOUND ON (pin 26, main latch bit 2) inverts through D9
  and clocks an LS74 (D5); Q drives Z80 pin 16 (/INT). The MAME
  rising-edge trigger matches the wired edge.
- Release: the LS74 CLR net runs off-crop; MAME treats the line
  as HOLD_LINE (released on CPU acknowledge). Implementation
  clears the pending flag when the latch is read (AY1 port A),
  which is the acknowledge the sound program performs.

## AY-8910 output mixing

- AY1 port B feeds a resistor DAC, bits B0-B5 as weights
  1K/2.2K/4.7K/10K/22K/47K into the non-inverting 741 amp A7
  (MAME's AY port-B gun switch: every bit routes to its own
  RC voice). AY1 port B also drives the 4066 control bus via
  an LS273 (G4) clocked from a second LS74 (D4), selecting
  which of six parallel RC filters it passes through.
- Six 4066 channels (E7), one per gun voice, each an RC lowpass
  (resistor/cap values as drawn, 10K-47K range) into a common
  summing amp, so the voice mix follows the port-B switch.
- AY1 channels A/B/C + AY2 channels A/B/C feed the second 741
  (C7) through a second resistor summer; the two amp outputs
  mix at the volume pot and the edge connector.

## Gun timer

- Divide-by-5120 off the sound clock (divide-by-512 plus an LS90
  bi-quinary divide-by-10), read as a 10-entry table on AY1 port B
  (MAME `portB_r`: index `(total_cycles / 512) % 10` into
  00/10/20/30/40/90/A0/B0/A0/D0). Earlier notes placed a timer flag
  at 0x6000; that was a misread, the port-B table is the model and
  matches the LS90 sequence quoted in the MAME comment.

## Mute

- Main latch bit 3 drives a MUTE' net that gates the 4066
  control and the output amps; MAME's analog mute (all filters
  off, AYs silenced) matches the wired gate.

## Input

- Coin, start and service switches route physically through the sound board
  connector (J4 area) but are read by the main CPU on IN0, not through AY
  port reads. (An earlier note claimed AY port reads; the MAME input ports
  and the IN0 decode agree it is IN0.)
