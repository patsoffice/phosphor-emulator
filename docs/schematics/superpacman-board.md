# Super Pac-Man (Bally Midway, Namco hardware): CPU and video boards

Source: `arcade-museum.com/manuals-videogames/S/superpacman-schematics.pdf`, 14
scanned pages, read 2026-10-08 from the embedded images (`pdfimages`, 1 bit, 300 or
600 dpi, magnified nearest-neighbor only). CPU board is A084-91436-F316 (schematic
M051-00316-F009, sheet 9-5, dated 12/7/82, J. Szerszen); video board is
A084-91435-D316 (schematic M051-00316-C007, sheet 9-3). Page numbers below are the
PDF's, counted from 0.

Implemented in `machines/src/superpacman.rs`, with the 15XX in
`core/src/device/namco_15xx.rs`, the 56XX in `core/src/device/namco56.rs`, and the
sound output stage as `BoardParams::SUPERPAC` in `machines/src/namco_wsg_output.rs`.
Tracked as phosphor-emulator-fygh. Section 11 says what the code does with each
finding.

Status tags used on every answer:

- **READ**: seen on the drawing (native-resolution crop, nearest-neighbor magnification).
- **INFERRED**: deduced; the basis is stated.
- **NOT FOUND**: not on any of the 14 pages.

Where the drawing and the parts list disagree, both are given and the one chosen is stated.

The scans are stored rotated: pages 1, 2, 3, 6 and 7 read correctly turned -90
degrees, and pages 4 and 5 turned +90.

---

## 1. Page index

| Page | Sheet | Content |
|---|---|---|
| p-000 | 9-1 | Cabinet wiring (power chassis, power supply, CPU board, video board, monitor, credit bypass board). READ |
| p-001 | 9-2 (label partly cut) | Video board assembly drawing A084-91435-D316 with designation list and cross-reference list (part numbers). READ |
| p-002 | 9-3 | Video board schematic, right half: palette PROM 4C, palette resistor DAC and J1 video connector, color lookup PROMs 4E/3L, PAL 4D (SPV-5), sprite line buffers 4M/4N (2148) with counters, 4J/4K logic, title block. READ |
| p-003 | (9-3 continued, 600 dpi) | Video board schematic, left half: P1 50-pin ribbon to CPU board, bus buffers, 07XX timing custom (2C), 00XX (2D), video RAM 2E, sprite RAMs 2K/2J/2H, 04XX (2F), 12XX (3J), 11XX (3D), tile ROM 3C, sprite ROMs 3F/3E, address decoder 1C, flip latch 4A. READ |
| p-004 | 9-4 | CPU board assembly drawing A084-91436-F316 with designation list (component values). READ |
| p-005 | 9-4 (600 dpi version) | CPU board assembly drawing plus cross-reference list with Bally part numbers. READ |
| p-006 | 9-5 | CPU board schematic, right part: 18.432 MHz oscillator and dividers, 07XX (1N), shared sound RAM 3K/3L, address multiplexers 3F/3H/3J, 15XX (3N), waveform PROM 3M, 4M latch, 4066 volume switch, MB3730 amp, 74LS259 latch 2M, 16XX (4B), 56XX I/O chips (4F, 4C), input connectors, DIP switches, I/O nibble RAM 2D, power connector J3, title block. READ |
| p-007 | (9-5 continued, 600 dpi) | CPU board schematic, left part: P1 ribbon connector to video board, main 6809E (1A), sound 6809E (1L), ROM sockets 1B/1C/1D and 1K/1J, PALs SPC-6 (2B) and SPC-5 (1M), 2C 74LS138, IRQ latches 2J, watchdog 3D, reset circuit Q1, address buffers 1E/1H/2H, data buffers 1F/2K. READ |
| p-008 | 9-6 | Medium power supply 70VA (A082-90427-A000) assembly drawing and designation list. READ |
| p-009 | (9-6 continued) | Power supply cross-reference list (part numbers). READ |
| p-010 | 9-7 | Medium power supply 70VA schematic (two LM305 regulators U2/U5 with 2N3772/2N2905 pass transistors; +5 V and +12 V; unregulated aux). READ |
| p-011 | 9-8 | Power chassis M051-00945-A051 (MT-105A and MT-101A transformers, line filter, fuses, voltage selection). READ |
| p-012 | 9-9 (printed upside down) | Power chassis, CT model M051-00945-A046 (MT-105B variant). READ |
| p-013 | 9-10, 9-11 | Credit multiplier bypass board A082-91109-A000: assembly (9-10) and schematic (9-11), a 7-pin KK-156 jumper board. READ |

---

## 2. Clock tree

All on p-006 except where noted.

| Stage | Part / pins | Output | Ratio | Status |
|---|---|---|---|---|
| Oscillator | X1 18.432 MHz crystal; 4A 74LS368 gates 6->7 (R6 330 feedback, C2 100 pF) and 10->9 (R8 330), buffered by 4A 12->11 (enable pins 1/15 grounded). C1 30 pF and R43 drawn dashed (BOM: C1, R43 NOT USED; R7 NOT USED) | 18.432 MHz | 1 | READ |
| Divide by 3 | 3B 74LS109 dual JK. Both CK (pins 4, 12) on 18.432 MHz. FF1: J (2) from /2Q (pin 9), /K (3) grounded. FF2: J (14) = 1Q (6), /K (13) grounded. PR/CLR to +5 via R5. Output 2Q pin 10 | 6.144 MHz, 1/3 duty on 2Q | /3 | Parts READ; ratio INFERRED from the JK equations (Q1' = /Q2 and /Q1, Q2' = Q1 and /Q2 cycles 00 -> 10 -> 01) |
| 6M distribution | 3B 2Q -> 4A 4->5 -> R46 100 = **6M**; 4A 14->13 -> R45 100 = **6M SYNC** (to video board P1-5); 3A 74LS04 9->8 -> R44 100 = **6MB** (to both PALs pin 1) | 6.144 MHz | | READ |
| 15XX master clock | 18.432 MHz node -> 3A 11->10 -> resistor labeled **R47** 100 (duplicate designator, see note) -> 4J 74LS08 pins 12,13 -> 11 -> **15XX pin 1** | 18.432 MHz (inverted) | 1 | READ (traced the net end to end) |
| 15XX other clocks | 15XX pin 2 = 6M, pin 3 = 1H, pin 4 = 2H, pin 5 = 8H (glyph reads ".8H"; 8H is the only reading consistent with the 8H net at 1N pin 5) | | | READ / 8H INFERRED |
| H counter | 1N 07XX custom, pin 1 = 6M in. Pin 2 -> R47 100 -> **1H** (3.072 MHz); pin 3 -> **2H** (1.536 MHz); pin 5 = 8H | 1H = 6M/2, 2H = 6M/4 | | Pins READ; ratios INFERRED (ripple counter naming) |
| 2H / /2H | 4H 74LS04 13->12 = /2H; 2H 74LS367 12->11 -> R48 100 = **2H\*** (buffered, same polarity) | | | READ |
| 1H, /1H | 4H 74LS04 1->2 = /1H | | | READ |
| 1H\* | 2A 74LS74 FF1: D (2) = 1H, CK (3) = 3B 2Q (i.e. before the 6M inverter), Q (5) = 1H\*, /Q (6) = /1H\* | 1H resampled on the opposite 6M edge | | READ |
| Main CPU E | 1A pin 34 E = 2H through 4A 74LS368 pin 12->11 (inverting), pull-up R3 470 | **E = /2H, 1.536 MHz** | 18.432/12 | READ |
| Main CPU Q | 2A 74LS74 FF2: D (12) = 2H\*, CK (11) = 3C 74LS32 pin 8 = (1H OR 6M) (pins 9, 10), Q (9) = net **Q**; PR (10) pull-up R4 | Q = 2H delayed to the next rising edge of (1H or 6M), roughly 1/4 E cycle | | Wiring READ; phase INFERRED |
| Sound CPU E | 1L pin 34 E = net Q (main CPU's Q) through 2H 74LS367 14->13, pull-up R30 470 | **Sound E = main Q** | | READ |
| Sound CPU Q | 1L pin 35 = **2H\*** | Sound Q = 2H | | READ |
| Pixel clock | 6M SYNC to video board 2C 07XX pin 1; video board 2B 74LS368 4->5 and 12->11 -> R52 100 = 6M; 2A 74LS04 11->10 -> R50 100 = 6MB (p-003) | **6.144 MHz** | 18.432/3 | READ (net), INFERRED (that 6M is the dot clock: the tile and color latches 4F 74LS377 and the palette PAL are clocked by 6M) |

Note on the two "R47": p-006 has one R47 (100) between 1N pin 2 and 1H and a second
resistor also printed "R47 100" in the 15XX clock path. The BOM lists R44-R49 as 100 ohm
and only R44-R48 are otherwise accounted for, so the second one is probably R49 misprinted.
INFERRED.

Consequence for an emulator: both 6809Es run at 1.536 MHz (BOM: "MC68A09E CPU (1.5 MHZ)"),
locked to the video counter, with the sound CPU a quarter cycle ahead of the main CPU
(its E is the main CPU's Q). The 15XX gets the raw 18.432 MHz plus 6M/1H/2H/8H.

---

## 3. Video timing

- The H and V counters are **inside the 07XX custom**; there is no discrete counter chain
  on either board. Counts per line and lines per frame are **NOT FOUND** on the drawing.
- There are **two 07XX chips**: 1N on the CPU board and 2C on the video board. Both have
  open-collector-style /HRESET (pin 13) and /VRESET (pin 16) with 1K pull-ups (CPU: R31,
  R33; video: R2, R12), tied together through P1-7 (/H RESET) and P1-8 (/V RESET). Pin 19
  is pulled up by 1K on both (CPU R39, video R11); pin 15 grounded. So the two counters
  are kept in step by the shared reset lines. READ. (Which one is master is not shown;
  both are wired identically. INFERRED.)
- 07XX pin functions (video board 2C, p-003; CPU board 1N, p-006):

  | 07XX pin | Signal | Evidence |
  |---|---|---|
  | 1 | 6M / 6M SYNC in | READ |
  | 2 | 1H (via 100 ohm R54 video, R47 CPU) | READ |
  | 3 | 2H (via R53 100 on video) | READ |
  | 4 | 4H | video board tap labeled 4H on the line into 00XX pin 2. INFERRED from that tap |
  | 5 | 8H | READ (CPU board label) |
  | 6, 7, 8, 9 | 16H, 32H, 64H, 128H | INFERRED from pin order into 00XX pins 4..7 |
  | 10 | next H bit (256H?), goes through 2B 74LS368 10->9 inverter before 00XX pin 8 | wiring READ, name INFERRED |
  | 11 | ANDed with pin 18 in 3A 74LS10 (pins 3,4 = pin 18; 5 = pin 11; out 6). Likely /HBLANK | INFERRED |
  | 12 | /HSYNC | READ (label HSYNC with overbar) |
  | 13 | /H RESET | READ |
  | 16 | /V RESET | READ |
  | 17 | gated with /HSYNC in 3A 74LS10 (1,2 = /HSYNC; 13 = pin 17; out 12) -> 2B 6->7 = **/CMPSYNC**. Pin 17 is therefore /VSYNC | READ wiring, INFERRED name |
  | 18 | /VBLANK. CPU board: 1N pin 18 -> 3A 74LS04 5->6 = VBLANK | READ |
  | 20-24 | V count bits into 00XX pins 13..9 | READ wiring; bit names INFERRED (8V..128V) |
  | 25, 26, 27 | 4V, 2V, 1V | READ |

- Composite sync to monitor: /CMPSYNC -> R14 100, R13 1K to -5 V, C12 and C11 (drawn
  100 pF; BOM says C11, C12 470 pF), R55 2.2K, ferrite L1, to J1-2 and J1-3. READ.
- Total lines per frame and visible lines: **NOT FOUND**. (Not derivable from the
  drawing because the counter decode is internal to the 07XX.)

---

## 4. Interrupts, reset, watchdog

### Main CPU 1A (MC68A09E), p-007

| Pin | Driven by | Status |
|---|---|---|
| 3 /IRQ | 2J 74LS74 FF1 /Q (pin 6). D (2) tied high via R16 1K; PR (4) high via R16; **CK (3) = VBLANK**; **CLR (1) = INTON** | READ |
| 2 /NMI | Pull-up R1 2.2K drawn (BOM: R1 NOT USED) and a jumper link (round split-pad symbol) to the FIRQ/HALT node | READ |
| 4 /FIRQ, 40 /HALT | Tied together, pull-up R2 to +5. R2 value glyph is illegible on the drawing; BOM and part-number list say **1K** | READ (value from BOM) |
| 39 TSC | Ground | READ |
| 37 /RESET | Q1 2N3391A collector, pull-up R10 1K | READ |

Behavior (INFERRED from 74LS74 truth table): a rising edge on VBLANK sets FF1, /Q goes low,
IRQ asserted. It stays asserted until software writes INTON = 0 (74LS259 Q1), which clears
the flop and holds it clear. Writing INTON = 1 re-arms it. So the acknowledge is "write 0
then 1 to the INTON latch bit", and IRQ is level-held, one per frame at the start of VBLANK.

### Sound CPU 1L (MC68A09E), p-007

| Pin | Driven by | Status |
|---|---|---|
| 3 /IRQ | 2J FF2 /Q (pin 8). D (12) and PR (10) high via R16 1K; **CK (11) = VBLANK**; **CLR (13) = INTON2** | READ |
| 2 /NMI | R28 2.2K drawn dashed (BOM: R28 NOT USED); jumper link to FIRQ/HALT node | READ |
| 4 /FIRQ, 40 /HALT | Tied, pull-up R29 1K | READ |
| 39 TSC | Ground | READ |
| 37 /RESET | **SUBRESET** (74LS259 Q5) directly | READ |

Neither CPU has an NMI or FIRQ source. HALT is never used.

### Reset circuit, p-007

- SW1 (tact switch MX-1) across C4 22 uF (from +5 V to node X). D1 1N914B from ground to
  X (clamp). X -> R11 -> Q1 base. R11 is drawn "4 7K"; BOM and part-number code say
  **4.7K**. C3 1000 pF base to ground. Q1 2N3391A, emitter ground, collector = main /RESET
  with R10 1K pull-up. READ.
- Collector -> 3A 74LS04 1->2 (active-high RESET) -> 3A 3->4 = **/RESET** (board-wide).
  READ.
- R9 47K from 3A pin 2 (active-high reset) back to Q1 base: positive feedback, so reset
  latches once asserted. READ (wiring), INFERRED (purpose).
- Power-on: C4 starts discharged, X at +5 V, base driven, reset asserted. READ/INFERRED.

### Watchdog, p-007

- 3D 74LS161: CK (2) = **VBLANK**, CLR (1) = **WDR** (from SPC-6 pin 14), /LD (9) =
  **/RESET**, A-D (3-6) grounded, EP (7) and ET (10) to +5 via R13 1K. QD (11) -> R12 2.2K
  (drawn "2 2K"; BOM 2.2K) -> Q1 base. READ.
- Behavior (INFERRED): counts VBLANK rising edges; if WDR is not strobed, QD goes high at
  the 8th VBLANK and pulls Q1 on, asserting reset. While reset is asserted the counter
  is loaded with 0, but 74LS161 load is synchronous, so reset is held until the next
  VBLANK edge loads 0 and QD drops; R12 then out-pulls R9 and the latch releases. So a
  watchdog reset lasts until the next VBLANK. Timeout: 8 frames without a WDR access.
- 74LS259 is cleared by /RESET, so every reset also re-asserts SUBRESET (sound CPU held),
  4 RESET (56XX held), clears INTON/INTON2 and SOUND ON.

### Sound CPU reset / enable

- SUBRESET = 74LS259 (2M) Q5 pin 10 -> 1L /RESET. After any main reset it is 0 (sound
  CPU held in reset) until main CPU writes 1. READ (wiring), INFERRED (sequence).

---

## 5. Address decode

### Main CPU (p-007)

- Address lines MA0-MA15 from 1A. ROM sockets: **1B, 1C, 1D are 2764 (8K)**, A0-A12 from
  MA0-MA12. READ.
- **2C 74LS138**: A = MA13 (1), B = MA14 (2), C = MA15 (3), G1 (6) = CPU R/W (only on
  reads), /G2A (5) and /G2B (4) grounded. READ.
  - Y5 (pin 10) -> **1D** CS (pin 20): A000-BFFF. 1D is "NOT USED" on Super Pac-Man per
    BOM. READ.
  - Y6 (pin 9) -> **1C** CS: C000-DFFF, EPROM **SPC-2**. READ.
  - Y7 (pin 7) -> **1B** CS: E000-FFFF, EPROM **SPC-1**. READ.
- **PAL 10L8 SPC-6 (SPI-1) at 2B**. Inputs: 1 = 6MB, 2 = /1H\*, 3 = 1H, 4 = 2H\*,
  5 = MA11, 6 = MA12, 7 = CPU R/W, 8 = MA15, 9 = MA14, 11 = MA13. Outputs: 19 = R/W
  (buffered/strobed R/W), 18 = **SOUND** (main access to sound RAM: enables 2F 74LS245),
  17 = **SND WR** (to SPC-5), 16 = **LATCH** (to SPC-5 pin 5), 15 = **FBIT** (to 16XX
  pin 3, I/O chips), 14 = **WDR** (watchdog clear), 13 = **CK** (video board strobe, to
  P1-48 and to video 1C 74LS138 /G2A), 12 = enable of main address buffers 1E/1H/2H
  (pins 1, 15). READ.
  - The fuse map is not on the drawing, so exact ranges are **NOT FOUND**. With only
    MA11-MA15 into the PAL the decode granularity is 2K. INFERRED. The map the code
    uses (0000-07FF video RAM, 0800-1FFF work and sprite RAM, 2000-27FF flip,
    4000-43FF sound RAM, 4800-481F I/O, 5000-500F latch, 8000 watchdog, C000-FFFF
    ROM) is consistent with this wiring, but the ranges decoded by the PAL rather
    than by a 74LS138 were inferred from the program's behavior, not read here.
- Video board decode (p-003): **1C 74LS138**, A = BMA11, B = BMA12, C = BMA13, G1 = BMA15,
  /G2B = BMA14, /G2A = CK. READ. BMA15 is MA15 inverted (CPU board 3A 74LS04 13->12 ahead of
  2H 74LS367), so the 138 is active for MA15 = 0, MA14 = 0. INFERRED. Outputs:
  - Y0 (15) AND /2H (1B 74LS08 1,2 -> 3) = **/RAM** to video RAM 2E. 0000-07FF.
  - Y1 (14), Y2 (13), Y3 (12) each ANDed with /2H in 1B -> CS of sprite RAMs 2K, 2J, 2H
    (0800-0FFF, 1000-17FF, 1800-1FFF). Gates READ; which output reaches which RAM is
    INFERRED from layout order.
  - Y4 (11) = **FCK** -> 4A 74LS74 FF2 CK (11), D (12) = **BMD0**, Q (9) = **FLIP**.
    Flip screen = bit 0 written to 2000-27FF. READ.

### Sound CPU (p-007)

- 1K = 2764 (EPROM **SPC-3**), A0-A12. 1J drawn as 2732 with only SA0-SA10 and pin 21 =
  sound R/W (RAM-style /WE pinout); BOM: **1J NOT USED**. READ.
- **PAL 10L8 SPC-5 (SPI-2) at 1M**. Inputs: 1 = 6MB, 2 = /1H\*, 3 = 1H, 4 = Q (via 4J
  74LS08 4,5 -> 6), 5 = LATCH (from SPC-6), 6 = SND WR (from SPC-6), 7 = sound R/W,
  8 = SA15, 9 = SA14, 11 = SA13. Outputs: 19 = 1J CS, 18 = 1K /OE, 17 = **SRAMWR**,
  16 = **SOUND 2** (sound CPU access to shared RAM: enables 2K 74LS245), 15 = 1J /OE,
  14 = **LTWR** (74LS259 /G), 13 = not traced, 12 = 1K CS. READ (pin 13 NOT FOUND).
- Decode granularity 8K (only SA13-SA15). INFERRED.

### 74LS259 addressable latch at 2M (p-006)

- /G (14) = **LTWR** (from SPC-5, which combines the main CPU's LATCH strobe with the
  sound CPU's own decode, so **both CPUs can write this latch**). D (13) = **A20**
  (address bit 0), A (1) = A21 (bit 1), B (2) = A22 (bit 2), C (3) = A23 (bit 3).
  CLR (15) = **/RESET**. READ.
  So the bit written is address bit 0 and the latch selected is address bits 3..1
  (16 addresses, 8 latches). INFERRED from the 259 truth table.

| Output | Pin | Net | Drives | Status |
|---|---|---|---|---|
| Q0 | 4 | INTON 2 | 2J FF2 CLR: sound CPU IRQ enable/acknowledge | READ |
| Q1 | 5 | INTON | 2J FF1 CLR: main CPU IRQ enable/acknowledge | READ |
| Q2 | 6 | none drawn | NOT FOUND (unconnected) | READ (absence) |
| Q3 | 7 | SOUND ON | 4M 74LS273 /CLR (pin 1): 0 forces DAC and volume latch to zero | READ |
| Q4 | 9 | 4 RESET | 56XX pin 3 on 4F and 4C (I/O chip reset) | READ |
| Q5 | 10 | SUBRESET | 1L /RESET (sound CPU) | READ |
| Q6 | 11 | none drawn | NOT FOUND | READ (absence) |
| Q7 | 12 | none drawn | NOT FOUND | READ (absence) |

Flip screen is **not** on the 259; it is the 74LS74 at video 4A (see above).

---

## 6. Shared sound RAM and arbitration (p-006)

- RAM: **3K and 3L, MB8148 (2148-type 1K x 4)**, together 1K x 8. /CS (pin 8) of both
  grounded (always selected). Data: 3K I/O = BSD7-BSD4, 3L I/O = BSD3-BSD0. /WE (pin 10)
  of both = **4J 74LS08 pin 8 = SRAMWR (pin 9) AND 15XX pin 7 (pin 10)**, so either
  active-low strobe writes. READ.
- Address is a separate multiplexed bus **A20-A29** (A2n = RAM address bit n; 2148 pin
  map A0=5, A1=6, A2=7, A3=4, A4=3, A5=2, A6=1, A7=17, A8=16, A9=15). READ.
- Time-division multiplex, no wait states, no arbitration logic: READ (parts),
  INFERRED (phases):

| Phase | Condition | Address source | Parts |
|---|---|---|---|
| 15XX | 2H = 1 | A20 = 1N pin 4 (4H), A21 = 1N pin 5 (8H), **A22 = 15XX pin 6**, A23-A25 = 1N pins 6, 7, 8 (16H, 32H, 64H); A26-A29 forced HIGH | 2N 74LS367 enabled by /2H (pins 1, 15); 3F/3H 74LS257 tri-stated (STR = 2H); 3J 74LS158 forced high by STR = 2H |
| Sound CPU | 2H = 0, 1H = 0 | SA0-SA9 | 3F, 3H 74LS257 "A" inputs, SEL = 1H; 3J 74LS158 (SA6-SA9, **inverting**) |
| Main CPU | 2H = 0, 1H = 1 | BMA0-BMA9 | same muxes, "B" inputs; BMA6-BMA9 through the inverting 74LS158 |

- Because 3J is a 74LS158, CPU address bits 6-9 are stored inverted, and the strobe forces
  them to 1111 during the 15XX phase. The 15XX therefore reads CPU-visible offsets
  **0x000-0x03F** of the shared RAM (bits 6-9 = 0). INFERRED from 74LS158 behavior; this
  is the 15XX's register window.
- Data paths: main side BMD <-> BSD through **2F 74LS245** (/G = SOUND, DIR = R/W); sound
  side SD <-> BSD through **2K 74LS245** (/G = SOUND 2, DIR from sound R/W). The 15XX and
  the 4M latch read BSD directly. READ.

---

## 7. Video

From p-001 BOM, p-002 and p-003 schematic.

| Function | Part | Size | Status |
|---|---|---|---|
| Tile (character) ROM | **3C**, "PROM SPV-1", 24-pin 2732-style pinout, A0-A11 | 4K x 8 | READ |
| Sprite ROM | **3F**, "PROM SPV-2", 2764 | 8K x 8 | READ |
| Second sprite ROM socket | **3E**, 2764; BOM line for 3E is blank (not fitted on Super Pac-Man). 3E /CS = 3F /CS inverted through 4H 74LS86 (other input +5 via R15 1K); the select is D40 | 8K x 8 | READ |
| Tile color lookup PROM | **4E**, BP-ROM SPV-6 (SPI-5), A0-A7, D0-D3, CE1/CE2 grounded | 256 x 4 | READ |
| Sprite color lookup PROM | **3L**, BP-ROM SPV-3 (SPI-4), A0-A7, D0-D3 | 256 x 4 | READ |
| Palette PROM | **4C**, BP-ROM SPV-4 (SPI-6), A0-A4, D0-D7, CS grounded | 32 x 8 | READ |
| Priority/mixer PAL | **4D**, PAL SPV-5 (SPI-7): pin 1 = 6M, 2-5 = 4E D3-D0, 6-9 = sprite pixel, 12 and 19 = further inputs, 13-17 -> 4C A0-A4 | equations NOT FOUND | READ |
| Video RAM | **2E**, N58725P 2K x 8, address from 00XX (2D) pins 16-26 as BMA0-BMA10, /CS = /RAM | 2K x 8 | READ |
| Sprite RAM | **2K, 2J, 2H**, N58725P 2K x 8 each, address MA30-MA36 and MA17-MA20 | 3 x 2K x 8 | READ |
| Sprite line buffer | **4M, 4N**, 2148 (1K x 4); bank chosen through 4K 74LS157 by 1V and HSYNC; address counters 2L/2M/2N 74LS161 | | READ (parts); operation INFERRED |
| Tile code latch | **1D** 74LS273, D50-D57 -> 3C A4-A11 | | READ |
| Tile attribute latch | **4F** 74LS377 clocked by 6M, /E = 3A 74LS10 NAND(4H, 2H\*, 1H); D50-D56 | | READ |
| Customs | 00XX (2D, tile address generator, has FLIP on pin 27), 04XX (2F), 11XX (3D, tile shifter fed by 3C), 12XX (3J, sprite) | | READ (names) |

Palette DAC (p-002), READ. 4C data lines run straight to the resistors in order:

| PROM bit | Resistor | Color |
|---|---|---|
| D0 (pin 1) | R3 1K | Red |
| D1 (pin 2) | R4 470 | Red |
| D2 (pin 3) | R5 220 | Red |
| D3 (pin 4) | R6 1K | Green |
| D4 (pin 5) | R7 470 | Green |
| D5 (pin 6) | R8 220 | Green |
| D6 (pin 7) | R9 470 | Blue |
| D7 (pin 9) | R10 220 | Blue |

- Each color node has a 2.2K load to the return line (R58 red, R57 green, R56 blue), a
  100 pF cap each side of a ferrite bead (C17/C18 red, C15/C16 green, C13/C14 blue;
  BOM C13-C18 100 pF), to J1: Blue 6 (return 5), Green 8 (return 9), Red 10 (return 11).
  READ.
- No buffer between PROM and resistors: the PROM output drives the resistors directly.
  READ.
- Scroll register: **NOT FOUND**. No scroll latch on either board; the 00XX takes only
  the counters and FLIP. INFERRED from absence.
- Flip screen: video 4A 74LS74 (see section 5). The FLIP net also feeds 3B 74LS86 XOR
  gates with 4H, 4V, 2V, 1V (sprite/tile address flipping). READ.

---

## 8. Sound output chain (p-006)

All values cross-checked against the p-004 designation list and the p-005 part-number
list (Bally part number 0062-NNNB3 encodes the E48 index: 179 = 1K, 195 = 2.2K,
211 = 4.7K, 227 = 10K, 231 = 12K, 243 = 22K, 251 = 33K, 259 = 47K, 275 = 100K).
The schematic prints decimal points as a gap, so "4 7K" and "2 2K" are ambiguous on the
drawing; the BOM settles each one below.

### 8.1 Waveform path (digital)

- **15XX (3N)** pins 19-26 = waveform PROM address A0-A7 into **3M** (BP-ROM SPC-4 (SPI-3),
  256 x 4; pinout matches an 82S129/82S126: 19 -> A0 pin 5, 20 -> A1 pin 6, 21 -> A2 pin 7,
  22 -> A3 pin 4, 23 -> A4 pin 3, 24 -> A5 pin 2, 25 -> A6 pin 1, 26 -> A7 pin 15; enables
  13 and 14 grounded). READ.
- 3M outputs pulled up by **RM17 1K x 4** (open-collector style). READ.
- **4M 74LS273** latch, CK (11) = **15XX pin 8**, /CLR (1) = **SOUND ON**:
  - 1D-4D (pins 3, 4, 7, 8) = BSD0-BSD3 (volume nibble read from RAM by the 15XX) ->
    1Q-4Q (2, 5, 6, 9).
  - 5D-8D (13, 14, 17, 18) = 3M output pins 9, 10, 11, 12 -> 5Q-8Q (12, 15, 16, 19).
  READ.

### 8.2 Waveform DAC (4 bits, TTL-driven)

| Latch out | PROM pin | Resistor | BOM | Weight |
|---|---|---|---|---|
| 5Q (12) | 9 (O4) | **R37 470** | 470 | MSB |
| 6Q (15) | 10 (O3) | **R36 1K** | 1K | |
| 7Q (16) | 11 (O2) | **R35 2.2K** | 2.2K | |
| 8Q (19) | 12 (O1) | **R34 4.7K** | 4.7K | LSB |

All four resistors join at one node D (READ: junction dots on all four). Relative
conductances 1 : 0.47 : 0.214 : 0.100, not exactly binary. Source impedance of node D
about 271 ohm (470 || 1K || 2.2K || 4.7K). Drive levels are 74LS273 TTL outputs (high is
roughly 3.4 V, not 5 V). INFERRED.

### 8.3 Volume (4 bits, analog switches)

**4L TC4066B**. Every switch input is node D; each output goes through its own resistor
to the output node V. READ.

| Volume bit | Latch | 4066 control pin | Switch pins | Series resistor | Schematic print | BOM / part no. |
|---|---|---|---|---|---|---|
| BSD0 (bit 0) | 1Q (2) | 12 | 11 -> 10 | **R24** | 100K | 100K (275) |
| BSD1 (bit 1) | 2Q (5) | 5 | 4 -> 3 | **R23** | "4 7K" | **47K** (259) |
| BSD2 (bit 2) | 3Q (6) | 6 | 8 -> 9 | **R25** | "2 2K" | **22K** (243) |
| BSD3 (bit 3) | 4Q (9) | 13 | 1 -> 2 | **R22** | 10K | 10K (227) |

Chosen: R23 = 47K and R25 = 22K. The drawing alone could read 4.7K and 2.2K, but the
designation list (R23 47K, R25/R27 22K), the cross-reference quantities (47K: R9, R23;
22K: R25, R27) and the part-number codes all agree, and the resulting conductances
(1/10K : 1/22K : 1/47K : 1/100K = 1 : 0.45 : 0.21 : 0.10) form a sensible binary-ish
volume ladder. The 4066 supply pins are not drawn (NOT FOUND); 4066 on-resistance is in
series with each of these.

### 8.4 Output node, bias, filter, volume pot

Node V (READ, all on one vertical net with junction dots):

- R24, R23, R25, R22 from the switches (above).
- **R27 22K** to +5 V (drawn "2 2K"; BOM and part-number list 22K).
- **R17 12K** to ground.
- **C25 0.0047 uF** to ground (BOM: .0047 MF AX CER).
- **R26 33K** to node W.

Node W:

- **R18 33K** to **EXDATA** (P1-42 from the video board; on the video board P1-42 is
  marked N.U., so EXDATA is open and R18 carries nothing). READ.
- **VR1 1K pot** (BOM: 1K OHM POT) from W to ground; the wiper is the volume control.

Derived (INFERRED, ideal switches, DAC source impedance ignored):

- Quiescent bias at V with all volume bits 0: 5 V x (12K || 34K) / (22K + 12K || 34K)
  ~ 1.44 V (34K = R26 + VR1).
- Gain from node D to V at full volume (all four switches on): G_vol / G_total =
  0.1767 mS / 0.3349 mS ~ 0.53. Base conductance (R27 + R17 + R26/VR1) = 0.158 mS.
- V to W: 1K / 34K ~ 0.029 with the wiper at the top.
- C25 low-pass corner depends on the volume setting, because the volume resistors are
  part of node V's source resistance: about 5.4 kHz with all switches off
  (R ~ 6.3K) rising to about 11 kHz with all on (R ~ 3.0K).

### 8.5 Power amplifier

**5N Fujitsu MB3730** (BOM: MB3730), single 7-pin package, supply **+12 V**. READ.

| MB3730 pin | Connection | Value | Status |
|---|---|---|---|
| 1 (input) | VR1 wiper -> **C26 0.1 uF** (BOM .1 MF AX CER) -> pin 1; **C29 0.01 uF** pin 1 to ground | | READ |
| 2 | **C30 22 uF 10 V** to ground | electrolytic | READ (function, feedback decoupling, INFERRED) |
| 3 | **C31 220 uF 10 V** to ground | electrolytic | READ (function, ripple filter, INFERRED) |
| 4 | ground | | READ |
| 5 | output B -> **C27 0.15 uF 25 V** tantalum to ground -> C84 0.01 uF -> ferrite **FB3** -> C85 0.01 uF -> **J1-3 SPKR** | | READ |
| 6 | output A -> **C28 0.15 uF 25 V** tantalum to ground -> C86 0.01 uF -> ferrite **FB4** -> C87 0.01 uF -> **J1-2 SPKR** | | READ |
| 7 | +12 V, decoupled by **C32 100 uF 16 V** and **C33 0.1 uF** | | READ |

- The speaker is driven **bridge-tied (BTL)** between J1-2 and J1-3; neither terminal is
  ground. J1-4 is the key. READ.
- MB3730 internal gain, input impedance and output power: **NOT FOUND** on the drawing.
- +12 V enters on J3-5 through FB2 with C46/C47 390 pF. READ.

---

## 9. Inputs: 16XX and 56XX (p-006)

Structure (READ unless noted):

- The 56XX chips (**4F**, **4C**) are not on the CPU bus. **16XX (4B)** sits between them
  and a **2114 1K x 4 RAM at 2D** that the main CPU sees.
- 16XX 4B: pin 1 = 6M, 2 = 2H\*, 3 = **FBIT** (PAL SPC-6), 4 = R/W, 5 = BMA4, 6 = BMA5,
  7 = DI4, 8 = DI5, 10-13 = DI3-DI0, 9 -> 3C 74LS32 pin 13 (other input /1H, out pin 11),
  15-25 -> the two 56XX. 28 VCC, 14 GND.
- 2114 at 2D: A0 = DI5, A1 = DI4, A2-A5 grounded, A6-A9 from **3E 74LS157** (A side =
  BMA0-BMA3, B side from the 16XX side; S = 2H\*, strobe grounded), /CS = /1H,
  I/O1-I/O4 = DI0-DI3. Main data BMD0-BMD3 <-> DI0-DI3 through **2E 74LS245**.
  So the CPU sees 4-bit nibbles: address bits 0-3 pick the nibble, bits 4-5 (via 16XX)
  pick the bank. INFERRED.
- Each 56XX: pin 1 = 2H\* buffered through 4J 74LS08 (1, 2 -> 3); pin 3 = **4 RESET**
  (74LS259 Q4); pin 4 = VBLANK (overbar on the label is ambiguous; polarity not settled);
  pins 9, 10, 11, 12, 34-37, 5, 6 from the 16XX bus.

### 56XX at 4F: player controls. All through 2.2K series resistors (SIPs RM12, RM13,
RM15, RM16), 1K pull-ups (two 1K x 8 SIPs), 0.01 uF to ground each side (C67-C82 at the
connector, C9-C24 at the chip). READ.

| 4F pin | Input | Connector |
|---|---|---|
| 38 | 1 COIN | J2-7 |
| 39 | 2 COIN | J2-8 |
| 40 | NOT USED | J2-5 |
| 41 | SERVICE | J2-6 |
| 22 | PLYR 1 UP | J2-3 |
| 23 | PLYR 1 RIGHT | J2-4 |
| 24 | PLYR 1 DOWN | J2-1 |
| 25 | PLYR 1 LEFT | J2-2 |
| 26 | PLYR 2 UP | J1-8 |
| 27 | PLYR 2 RIGHT | J1-9 |
| 28 | PLYR 2 DOWN | J1-6 |
| 29 | PLYR 2 LEFT | J1-7 |
| 30 | 1 PLYR FAST | J2-13 |
| 31 | 2 PLYR FAST | J2-12 |
| 32 | 1 PLYR START | J2-11 |
| 33 | 2 PLYR START | J2-10 |

(Mapping READ by row: each connector line runs straight across to its series resistor.)

4F outputs: pin 18 -> 4H 74LS04 5->6; pin 17 -> 4H 9->8; pin 16 -> 4H 3->4, with a
jumper link between the pin 16 and pin 17 nets. The inverter outputs end in unlabeled
stubs. Destination (coin counters or lamps): **NOT FOUND**. 4C pin 11 feeds 4H 11->10,
also unlabeled. Pull-ups RM18 4.7K x 4 (designator beyond the BOM's RM17; as printed).

### 56XX at 4C: DIP switches and test

| 4C pin(s) | Source |
|---|---|
| 22-29 | **SW2** (8-pos DIP at 5B) switches 1-8 respectively (22 = SW1 ... 29 = SW8), pulled up by RM1 4.7K x 8 (BOM RM1-RM3 4.7K), switches to ground |
| 13 | **4E 74LS157** S (select), pull-up R38 4.7K |
| 38, 39, 40, 41 | 4E 1Y, 2Y, 3Y, 4Y. 4E inputs from **SW3** (8-pos DIP at 5E): 1A = sw1, 1B = sw5, 2A = sw2, 2B = sw6, 3A = sw3, 3B = sw7, 4A = sw4, 4B = sw8; pull-ups RM3 4.7K x 8; strobe grounded. So pin 13 selects SW3 switches 1-4 or 5-8 onto pins 38-41 |
| 33 | J2-14 **TEST** |
| 32 | J2-15 **C.T. VERSION** (cocktail) |
| 31 | J2-16 N.U. |
| 30 | J2-17 N.U. |

Pins 30-33 series resistors are labeled R32, R40, R19, R14 under a "2.2K x 4" header,
but the BOM and part-number list make R14, R19, R32, R40 **1K**; pull-ups R15, R20, R41,
R42 are 2.2K; caps C5-C8 and C63-C66 0.01 uF. Values conflict; 1K chosen per BOM. READ.
J2-9 is the key.

---

## 10. Things an emulator is likely to miss

1. **Both CPUs write the same 74LS259.** SPC-5 merges the main CPU's LATCH strobe with the
   sound CPU's own decode into LTWR. Data is address bit 0; the bit index is address
   bits 1-3. INFERRED from wiring.
2. **IRQ is a held latch, not a pulse.** VBLANK rising edge sets 2J; only writing the
   INTON/INTON2 bit to 0 clears it. If software leaves INTON = 1 without toggling, a
   second IRQ cannot occur; a never-acknowledged IRQ stays asserted.
3. **Watchdog counts VBLANKs** (3D 74LS161), times out at 8, and the resulting reset
   persists until the next VBLANK edge (synchronous load). Reset also clears the 259,
   so the sound CPU and 56XX go back into reset.
4. **No NMI/FIRQ sources**; NMI is jumpered to the FIRQ/HALT pull-up.
5. **CPU clocks are tied to the video counter**: main E = /2H, sound E = main Q
   (sound CPU leads by a quarter cycle). Shared RAM access is pure time slicing
   (2H high: 15XX; 2H low, 1H low: sound CPU; 2H low, 1H high: main CPU). No contention,
   no wait states.
6. **Address bits 6-9 of the shared RAM are inverted** through the 74LS158, and the
   15XX's window is the CPU-visible bottom 64 bytes (bits 6-9 forced to the "zero"
   value). The 15XX itself drives address bit 2 (pin 6); bits 0, 1, 3, 4, 5 come from
   4H, 8H, 16H, 32H, 64H.
7. **The 15XX can write the RAM**: 3K/3L /WE is SRAMWR AND 15XX pin 7. What it writes
   is not determinable from the drawing. READ (wiring).
8. **The 15XX clock is 18.432 MHz** (via 4J), not 6M; 6M, 1H, 2H, 8H are also inputs.
9. **Volume is analog**, not a digital multiply: 4066 switches select parallel
   10K/22K/47K/100K paths from the waveform DAC node into a node biased by 22K to +5 V,
   12K to ground and 33K + 1K pot to ground. The transfer is therefore not linear in the
   volume nibble, and the C25 (4.7 nF) low-pass corner moves with volume (about 5.4 kHz
   to about 11 kHz). The waveform DAC (470/1K/2.2K/4.7K) is also not exactly binary.
10. **SOUND ON = 0 clears the 74LS273**, which opens every 4066 switch: the output sits
    at the DC bias, silent, rather than at a "zero sample".
11. **BTL speaker output** (MB3730 pins 5 and 6 to J1-3 and J1-2).
12. **Sprite transparency is decided after the color lookup**: 3L outputs go into 4J
    74LS20 (pins 4, 5, 1, 2 -> 6); a lookup value of 0xF (all ones) blocks the line
    buffer write (4J second gate with OBJEN and /6M). So pen 15 after lookup is
    transparent, not raw pixel 0. READ (gates), INFERRED (meaning).
13. **Flip screen lives on the video board** (4A 74LS74, bit 0 written to the CK 2000
    page), not on the 259.
14. **I/O chips are behind a 2114 nibble RAM** managed by the 16XX; CPU reads of the
    "I/O" space read that RAM, which the 16XX/56XX update.
15. **Unpopulated sockets**: 1D (main ROM at A000), 1J (sound, 2K socket with RAM-style
    /WE on pin 21) and 3E (second sprite ROM) are wired but not fitted on Super
    Pac-Man. The board was clearly designed for a family of games.
16. **Two 07XX timing chips** kept in step by shared open-collector /HRESET and /VRESET
    (P1-7, P1-8).

### Drawing versus BOM discrepancies noted

- R1 (main NMI pull-up 2.2K drawn): BOM NOT USED.
- R28 (sound NMI pull-up 2.2K, drawn dashed): BOM NOT USED.
- R2 value illegible on drawing: BOM 1K.
- R14/R19/R32/R40 drawn under "2.2K x 4": BOM 1K.
- Video C11/C12 drawn 100 pF: BOM 470 pF.
- Video R52 drawn 100: BOM 82 ohm.
- Video designation list says R50, R51 100K; cross-reference and drawing say 100 ohm.
- Two CPU-board resistors printed "R47".

---

## 11. What the emulator does with this

| Finding | In the code | Status |
|---|---|---|
| Both CPUs on 2H, 1.536 MHz; 6.144 MHz dot clock | `clock_tree`, `TIMING` (96 CPU cycles a line) | Modeled |
| Lines per frame | 264, 224 visible: inside the 07XX, so not from this drawing | Inferred from the program, not read |
| Sound CPU leads by a quarter cycle (its E is the main CPU's Q) | `step_cycle` steps the sound CPU first. Moved no frame of the committed movie | Modeled |
| Shared RAM time-sliced on 1H/2H, no waits | Both CPUs step every cycle against one backing | Modeled |
| 15XX register window is CPU offsets 0x000-0x03F | `sound_ram_write` forwards those to `Namco15xx` | Modeled |
| 15XX voice record picked by H bits 4-6, half by its pin 6 | 24 kHz voice update; equal 1/8 duty in `SLOT_DUTY_15XX` | Modeled, duty inferred from the addressing |
| 15XX writes the RAM (pin 7 into /WE) | Not modeled: what it writes is not on the drawing | Gap |
| IRQs: 2J 74LS74 set by VBLANK, cleared only by INTON/INTON2 low | `begin_scanline`, `latch_write` | Modeled and tested |
| No NMI or FIRQ source | Never asserted | Modeled |
| LS259 at 2M, data = A0, select = A1-A3, both CPUs strobe it | `latch_write`, reached from 0x5000 and the sound CPU's 0x2000 | Modeled and tested |
| SUBRESET holds the sound CPU; release runs its reset sequence | `pending_sub_reset` | Modeled and tested |
| 4 RESET holds both 56XX | `Namco56::set_reset` | Modeled |
| Watchdog 3D, 8 VBLANKs | Not modeled, as on the other boards here | Gap |
| Flip is video 4A, D0 of a write to 2000-27FF | `main_write`. A read also sets it, which is the PAL's and inferred | Modeled |
| Palette DAC with a 2.2K load on each gun | `superpac_palette`: blue full drive 253 against red and green's 255 | Modeled and tested |
| Sprite pen 15 after 3L blocks the line-buffer write (4J) | `draw_sprite_line` | Modeled and tested |
| Priority and the char/sprite palette halves | The SPV-5 PAL's: inferred from the program | Inferred, tested |
| Sample ladder R37-R34, volume legs R22-R25 in 4L, node with R27/R17/R26+VR1, C25 | `BoardParams::SUPERPAC`, the stage Pac-Man shares | Modeled |
| C26 into the MB3730, input impedance not drawn | Galaga's 20 Hz coupling stands in for the same gap | Stand-in |
| C29 0.01 uF at the amp input | Left out: above 15 kHz behind the 1k pot | Gap, out of band |
| MB3730 bridge-tied speaker | Mono | Gap |
| 56XX pin 4 is VBLANK | Both chips run once a frame, one scanline into VBLANK | Modeled; latency is a stand-in |
| 56XX command modes | Undumped MB8843 program; modes inferred from the program's behavior | Inferred, tested |
| 56XX pin assignments, SW2 at 5B, SW3 at 5E through the 4E 74LS157 | `run_io` and the DIP tables | Modeled and tested |
