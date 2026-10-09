# Gyruss sound/IO board

Source: `arcade-museum.com/manuals-videogames/G/Gyruss__1983__Konami.pdf`
(Centuri manual), manual p28 (PDF p30, a 5000x7002 1-bit image at 768 ppi),
"GYRUSS SOUND BOARD" (Konami sheet, no Centuri title block). Read 2026-10-07.
Companion sheet is the logic board (manual p27); the two link by a 40P flat
cable. Circled pin numbers below are flat-cable pins.

This board carries the edge connector, all three DIP switches, the input
buffers, the video pass-through to the edge connector, the audio Z80 with
5x AY-8910, the 8039 SFX MCU with its DAC, and the LA4460 stereo amp.

## Edge connector and video pass-through

- Power: +5V (B18/A18), +12V (B1), -5V (A1), GND. Coin counter drivers
  (2SC2320 Q3/Q4) on B11/B12.
- Video out: RED A14, GREEN A13, BLUE B13, SYNC B14, sourced from the logic
  board over the flat cable.
- Speakers: SPL A15/B15, SPR A2/B2, driven by the LA4460 (6F/6E).

## Audio CPU (Z80 6B, 14.318181/4 = 3.579545 MHz, MAME)

- Program: 2x 2764 at 7A/8A (the 9A socket is empty); RAM: 2x 2114 (5A/4A).
- Map (MAME): 0x0000-0x5FFF ROM, 0x6000-0x63FF RAM, 0x8000 sound-latch read.
- IRQ from the main CPU (LS74 5D, "SOUND ON" strobe); the 8039 raises its own
  IRQ line back (see below).
- IO map (MAME, mask 0xFF): five AY-8910 triples (address/read/write) at
  0x00/0x04/0x08/0x0C/0x10, 0x14 asserts the 8039 INT, 0x18 writes latch 2.

## AY-8910s (5x, at 14.318181/8 = 1.789772 MHz, MAME)

Re-read 2026-10-08 from the 768 ppi page image (`pdfimages`, rotated, crops
at native resolution). Sockets: 11D and 12D carry the filters; 8B, 9B and 10B
do not.

- Filtered voices, all six channels of 11D and 12D, each the same shape:
  AY pin, a 1K series resistor (12D: R66 A, R38 B, R42 C; 11D: R64 A, R65
  B, R43 C), a node that two 4066 switches can hang capacitors on, then a
  2.2K leg to the bus (12D: R67, R39, R41; 11D: R40, R68, R44). The 1K and
  the 2.2K are in series, so a filtered voice reaches the bus through 3.2K.
- On each node, one switch carries a 0.047 uF mylar and the other a 0.22 uF
  tantalum (12D: C45/C44 A, C38/C39 B, C41/C40 C; 11D: C46/C47 A, C48/C49 B,
  C42/C43 C). Traced from each 4066 control pin to the chip's own port B:
  B0/B1 switch channel A's 0.047/0.22, B2/B3 channel B's, B4/B5 channel C's.
  Port B is configured as an output (R7 bit 7).
- The capacitor sees the 1K (plus the AY's output resistance) in parallel
  with the 2.2K leg, about 839 ohms with the bus near ground: corners near
  4.0 kHz (0.047), 860 Hz (0.22) and 710 Hz (both). A capacitor switched
  out keeps its charge.
- Unfiltered voices: 8B (R31/R30/R32), 9B (R36/R35/R37) and 10B (R62/R61/R63),
  3.3K per channel, labeled "3,3Kx3" under each chip.
- Bus wiring, traced end to end:
  - VR2 bus = 12D's three 2.2K legs + 8B's three 3.3K legs + the DAC's R34
    (4.7K). VR2 feeds 6F, whose outputs go to the SPL pins (left).
  - VR1 bus = 11D's three 2.2K legs + 9B's and 10B's six 3.3K legs (the 9B
    and 10B sums are tied together before reaching it). VR1 feeds 6E (right).
  - A two-pin RTB-1.5-2F connector, drawn dashed, sits across the two buses
    (pin 4 left, pin 3 right): fitted with a link it would make the board
    mono. Treated as absent.
- VR1/VR2 are 200 ohm pots from bus to ground; the wiper goes to LA4460 pin 2
  directly, with a 0.1 uF mylar to ground at the wiper (C36 right, C37 left)
  and a three-pin B6P-SHF-1AA header (ground, wiper, bus top) beside it.
  With the wiper at the top, C36/C37 against the bus's own resistance
  (about 134/145 ohms) put a corner near 11-12 kHz.
- LA4460 (6E/6F): pin 10 is the +12V supply with 1000 uF decoupling (C25/C29;
  the same line carries the coin-counter flyback diodes), pin 6 has a 100 uF
  electrolytic to ground (C26/C30) that takes the amplifier's DC gain to
  unity, and pins 7 and 9 each carry a 0.033 uF + 4.7 ohm Zobel to ground and
  run to the speaker pins. There is no series capacitor at the input.
- AY3 port A upper nibble is a /10240 timer off the sound CPU clock (LS90
  bi-quinary + /1024; MAME `porta_r` table). The game polls it for timing.

Two easy misreadings of this circuit: taking the filtered legs as 2.2K alone
(the 1K belongs to the leg as well as to the filter) makes those voices 3.3 dB
too loud against the 3.3K ones, and taking the pole as the capacitor against
the 1K alone, ignoring the 2.2K load, puts the corners at 2.5 kHz, 530 Hz and
440 Hz. The IO-order chip numbering (AY1 at 0x00 through AY5 at 0x10) was not
traced through the decode; the sheet does confirm that one filtered chip, one
unfiltered chip and the DAC share the left bus while one filtered and two
unfiltered chips share the right.

## 8039 SFX (7H, 8 MHz crystal as drawn)

- Program: 2732 at 11H (4 KB); latches LS374 (12H)/LS373 (10H); decode
  LS138 (7D).
- Reads latch 2 over its BUS port (MAME IO map); INT asserted by audio-CPU
  IO 0x14, cleared by a P2 write (MAME `irq_clear_w`).
- The program executes EN I once at reset and never again. Its handler reads
  the command, clears INT with a P2 write, rewrites stack entry 0 to return
  to the dispatcher at 0x016, forces SP to 1 with MOV PSW,A and leaves with
  RETR, abandoning whatever sample was playing. Each new command therefore
  depends on the MCS-48 keeping the INT enable set across interrupt entry.
- P1 is an 8-bit R-2R DAC: 200K series legs R45-R52 (P17 at the output end,
  P10 at the far end) with 100K rungs R53-R59 and a 200K terminator R60, into
  a uPC324 follower (J6, pins 12/13/14) and out through R34 (4.7K) to the
  left bus. The 324 is powered from +5V only (the sheet's arrow, per its
  notes legend), so its output cannot rise above about 3.5V.

## Inputs and DIPs (all on this board, read by the main CPU)

- Input buffers (LS253/LS367) with 2.2Kx8 pull-up arrays. SYSTEM: coin 1/2,
  service, start 1/2. P1/P2: four directions + shoot 1 + an unused shoot 2
  (bit 0x20); the sheet also wires shoot 3 for both players, which no port
  reads.
- DIPSW1 (2B, coinage), DIPSW2 (3B, gameplay), DIPSW3 (4B, music) through
  LS367s onto D0-D7; selected by AB5/AB6/IOEN from the main board.
- Coinage and gameplay tables below are transcribed from the manual pp5-6,
  which is the Centuri book, so these are the gyrussce settings and defaults
  (* = normal). The Konami parent's bonus and difficulty defaults follow MAME
  and are flagged in the machine file.

DIPSW1 coinage (SW1-SW8, 1 coin/1 play is all off):

- 1/1, 1/2, 1/3, 1/4, 1/5, 1/6, 1/7, 2/1, 2/3, 2/5, 3/1, 3/2, 3/4, 4/1, 4/3,
  free play (all on). Coin and plays fields advance together down the table;
  see the manual p5 for the exact bit rows.

DIPSW2: SW1-2 spaceships (3* off/off, 4 on/off, 5 off/on, 256 on/on);
SW3 game type (table off = 1 or 2 players, upright on = 1 player only);
SW4 bonus (off = 50k then every 70k, *on = 60k then every 80k);
SW5-7 difficulty (very easy, easy -1/-2/-3, average, *difficult, very
difficult, most difficult); SW8 attract sound (off = none, *on = sound).

DIPSW3: SW1 music (*on = on, off = off); SW2-8 unused.
