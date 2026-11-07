; ---------------------------------------------------------------------------
; Williams gen-1 video timing conformance ROM
;
; Design: docs/designs/williams-video-conformance.md
; Assemble (both images; see the ROMBASE note below):
;   asl -q -o williams_video.p williams_video.asm
;   p2bin williams_video.p williams_video.bin -r 0xD000-0xFFFF -l 0x00
;   asl -q -D ROMBASE=0xE000 -o williams_video_e000.p williams_video.asm
;   p2bin williams_video_e000.p williams_video_e000.bin -r 0xE000-0xFFFF -l 0x00
;
; Hardware build (phosphor-emulator-9sr9.2): the same program plus an on-screen
; readout of the result block behind an assembly-time flag, so the tested build
; and the hardware build stay one source:
;   asl -q -D HWBUILD=1 -o williams_video_hw.p williams_video.asm
;   p2bin williams_video_hw.p williams_video_hw.bin -r 0xD000-0xFFFF -l 0x00
;
; With the flag off every HW block assembles to nothing, and the default image
; must stay byte-identical: assemble it and cmp against the committed binary
; after any edit, HW or otherwise.
;
; Both tools are in the Nix dev shell. p2bin's -r selects the program-ROM window
; and -l zero-fills the gap between the code and the vector table, which is what
; makes each image flat and copyable byte for byte.
;
; Runs on the Joust / Robotron program-ROM map ($D000-$FFFF, 12 KB). Loaded by
; machines/tests/williams_video_timing_test.rs into a ROM-less machine built
; with MachineEntry::create_bare, by poking the image through BusDebug::write
; (AddressSpace16::debug_write ignores AccessKind, so a ReadOnly region takes
; the write).
;
; EVERY WAIT IS A POLL OF THE VIDEO COUNTER, NEVER A DELAY LOOP. That is what
; makes the program immune to a constant cycle offset between implementations,
; and it is why the same binary can be run in MAME for a second opinion.
;
; The counter at $CB00 reads `scanline & $FC`, so its resolution is four
; scanlines. Every expectation in the design doc is stated to that resolution.
; ---------------------------------------------------------------------------

            cpu     6809

; --- Hardware ---------------------------------------------------------------

PALETTE     equ $C000           ; 16 entries, BBGGGRRR
ROMPIA_PRA  equ $C80C           ; ROM PIA: port A data / DDRA
ROMPIA_CRA  equ $C80D           ;          control A
ROMPIA_PRB  equ $C80E           ;          port B data / DDRB
ROMPIA_CRB  equ $C80F           ;          control B
BANKSEL     equ $C900           ; ROM bank select (bit 2 also gates Sinistar's
                                ; blitter window clip — must stay 0 here)
BLIT_CTRL   equ $CA00           ; write triggers the blit
BLIT_SOLID  equ $CA01
BLIT_SRCHI  equ $CA02
BLIT_SRCLO  equ $CA03
BLIT_DSTHI  equ $CA04
BLIT_DSTLO  equ $CA05
BLIT_W      equ $CA06
BLIT_H      equ $CA07
VIDCOUNT    equ $CB00           ; scanline & $FC
WATCHDOG    equ $CBFF           ; write $39 to clear (Williams quirk)

; Blitter control bits
B_SRC256    equ $01
B_DST256    equ $02
B_SLOW      equ $04
B_FGONLY    equ $08
B_SOLID     equ $10
B_SHIFT     equ $20
B_NOODD     equ $40
B_NOEVEN    equ $80

; Solid fast fill, both strides 256. The source is read even in SOLID mode, so
; stride-256 keeps the dummy reads inside VRAM and away from the PIAs, whose
; read side-effects would clear interrupt flags.
FILLCTL     equ B_SOLID+B_DST256+B_SRC256
FILLSLOW    equ FILLCTL+B_SLOW

; SC1 (Joust, Robotron) XORs 4 into the width and height registers, then clamps
; zero to one. Every size is written pre-XORed, as a literal with its
; derivation beside it -- `size+4` is only the same as `size^4` when bit 2 of
; the size is clear, which is a trap worth not setting.
W_8         equ $0C             ;   8 ^ 4
H_128       equ $84             ; 128 ^ 4
W_146       equ $96             ; 146 ^ 4   (the 146 displayed columns)
H_85        equ $51             ;  85 ^ 4   (one third of 255 rows)
WH_XORPROBE equ $04             ;   4 ^ 4 = 0, clamped to 1

; Palette entries, BBGGGRRR
BLACK       equ $00
RED         equ $07             ; R = 7
GREEN       equ $38             ; G = 7

; --- Direct page variables ($0000-$001F, VRAM column 0, never displayed) -----

            assume  dpr:$00

WTARGET     equ $00             ; WaitLine target
T1PREV      equ $01
T1TRANS     equ $02
T1WRAPS     equ $03
T1MAX       equ $04
IRQCNT      equ $05
IRQPTR      equ $06             ; 2 bytes
TMP0        equ $08
CNTA        equ $09
CNTB        equ $0A
EDGES       equ $10             ; 4-byte capture table for IRQ edges

STACKTOP    equ $AFFF           ; VRAM column $AF, never displayed

; --- Result block ($B000, VRAM column $B0, never displayed) -----------------

RES         equ $B000
R_MAGIC     equ RES+0           ; $5A on completion
R_PHASE     equ RES+1
R_T1TRN     equ RES+2           ; counter transitions in one frame   (expect 64)
R_T1WRP     equ RES+3           ; wraps                              (expect 1)
R_T1MAX     equ RES+4           ; highest value seen                 (expect $FC)
R_T1DW0     equ RES+5           ; poll iterations while counter == 0
R_T1DW4     equ RES+6           ; poll iterations while counter == 4
R_T2CNT     equ RES+7           ; CA1 (count240) IRQs per frame      (expect 1)
R_T2LIN     equ RES+8           ; counter inside that handler        (expect $F0)
R_T3RCNT    equ RES+9           ; CB1 rising edges in [16,240)       (expect 4)
R_T3RLIN    equ RES+10          ; 4 bytes            (expect $20 $60 $A0 $E0)
R_T3FCNT    equ RES+14          ; CB1 falling edges in [16,240)      (expect 3)
R_T3FLIN    equ RES+15          ; 3 bytes                (expect $40 $80 $C0)
R_T4FST     equ RES+18          ; counter delta, 1024-byte fast blit (expect $10)
R_T4SLW     equ RES+19          ; ... with CTRL_SLOW                 (expect $20)
R_T5A       equ RES+20          ; $A000 after the 4x4 blit           (expect $EE)
R_T5B       equ RES+21          ; $A100 after the 4x4 blit           (expect $00)
RESLEN      equ 32

; ---------------------------------------------------------------------------

; Link address. Joust and Robotron carry 12 KB of program ROM at $D000-$FFFF;
; Sinistar shrinks that to 8 KB at $E000-$FFFF and puts 4 KB of work RAM at
; $D000 instead (williams.rs:461-470). Nothing else about the program differs,
; so the same source is assembled twice rather than forked:
;
;   asl -q -o ...                   -> $D000, 12 KB image
;   asl -q -D ROMBASE=0xE000 -o ... ->  $E000, 8 KB image
;
; Everything the program touches other than its own code lives in video RAM
; below $C000, so relocating the code is the whole difference.
            ifndef  ROMBASE
ROMBASE     equ     $D000
            endif

; Hardware display build: -D HWBUILD=1. Off by default, and every block it
; guards assembles to nothing when off, so the default image cannot drift.
            ifndef  HWBUILD
HWBUILD     equ     0
            endif

            org     ROMBASE

; ===========================================================================
; Entry
; ===========================================================================
Reset
            orcc    #$50                ; mask IRQ and FIRQ
            lds     #STACKTOP
            clra
            tfr     a,dp                ; direct page = $00 (explicit, not assumed)

            ldx     #RES                ; a zero result block must never read
            ldb     #RESLEN             ; as a pass, so clear it deliberately
ClrRes
            clr     ,x+
            decb
            bne     ClrRes

; STA rather than CLR throughout on I/O: the 6809's CLR performs a read cycle
; before the write, and a gratuitous read of a device register is not something
; to leave lying in a conformance ROM.
            clra
            sta     BANKSEL             ; bank 0; Sinistar window clip off

; PIA control registers: bit 2 selects the data register at offsets 0 and 2.
; It has to be set or a read of the data register returns the DDR and never
; clears the interrupt flag, which would wedge in the handler.
            lda     #$04
            sta     ROMPIA_CRA          ; data select, IRQA1 disabled
            sta     ROMPIA_CRB          ; data select, IRQB1 disabled

            lda     #BLACK
            sta     PALETTE+0
            lda     #RED
            sta     PALETTE+1
            lda     #GREEN
            sta     PALETTE+2

            jsr     PetDog

; ===========================================================================
; Phase 1 -- T1: video counter survey
;
; From line 16 round to line 16, count value transitions, wraps and the
; maximum. Then measure how long the counter dwells at 0 against a 4-line
; reference step: current_scanline() is a u8, so lines 256-259 alias onto 0-3
; and the value 0 should occupy eight lines against every other value's four.
; ===========================================================================
T1
            jsr     WaitWrap
            ldb     #$10
            jsr     WaitLine

            clr     <T1TRANS
            clr     <T1WRAPS
            clr     <T1MAX
            lda     VIDCOUNT
            sta     <T1PREV
T1Loop
            lda     VIDCOUNT
            cmpa    <T1PREV
            beq     T1Loop              ; no change yet
            inc     <T1TRANS
            cmpa    <T1PREV
            bhs     T1NoWrap            ; unsigned: rose, so not a wrap
            inc     <T1WRAPS
T1NoWrap
            cmpa    <T1MAX
            blo     T1NoMax
            sta     <T1MAX
T1NoMax
            sta     <T1PREV
            ldb     <T1WRAPS
            beq     T1Loop              ; keep going until we have wrapped
            cmpa    #$10
            bne     T1Loop              ; ... and are back where we started

            lda     <T1TRANS
            sta     R_T1TRN
            lda     <T1WRAPS
            sta     R_T1WRP
            lda     <T1MAX
            sta     R_T1MAX

; Dwell. Sit at $FC, wait for the wrap, then count identical-length poll
; iterations first while the counter reads 0 and then while it reads 4. Both
; loops are 16 cycles, so the ratio is meaningful even though each count is one
; high (the iteration that observes the change is counted).
            ldb     #$FC
            jsr     WaitLine
T1DwWait
            lda     VIDCOUNT
            bne     T1DwWait
            clr     <CNTA
T1Dw0
            inc     <CNTA
            lda     VIDCOUNT
            cmpa    #$00
            beq     T1Dw0
            clr     <CNTB
T1Dw4
            inc     <CNTB
            lda     VIDCOUNT
            cmpa    #$04
            beq     T1Dw4

            lda     <CNTA
            sta     R_T1DW0
            lda     <CNTB
            sta     R_T1DW4

            lda     #1
            sta     R_PHASE

; ===========================================================================
; Phase 2 -- T2: CA1 (count240) fires once per frame, at line 240
;
; Window is a whole frame, line 16 to line 16, so the single rising edge at
; 240 falls inside it exactly once.
; ===========================================================================
T2
            jsr     WaitWrap
            ldb     #$10
            jsr     WaitLine

            jsr     ArmEdges
            lda     #$07                ; CRA: data select, rising edge, IRQA1 on
            sta     ROMPIA_CRA
            lda     #$04                ; CRB: data select, IRQB1 off
            sta     ROMPIA_CRB
            lda     ROMPIA_PRA          ; clear any stale flags before unmasking
            lda     ROMPIA_PRB
            andcc   #$EF                ; unmask IRQ

            jsr     WaitWrap
            ldb     #$10
            jsr     WaitLine

            orcc    #$10                ; mask IRQ
            lda     #$04
            sta     ROMPIA_CRA          ; disarm

            lda     <IRQCNT
            sta     R_T2CNT
            lda     <EDGES
            sta     R_T2LIN

            lda     #2
            sta     R_PHASE

; ===========================================================================
; Phase 3 -- T3R: CB1 (VA11) rising edges
;
; VA11 is scanline bit 5, so it rises at 32, 96, 160 and 224. Window is
; [line 16, line 240) so the edge set is unambiguous and does not depend on
; where in vblank the arming landed.
; ===========================================================================
T3R
            jsr     WaitWrap
            ldb     #$10
            jsr     WaitLine

            jsr     ArmEdges
            lda     #$04                ; CRA: IRQA1 off
            sta     ROMPIA_CRA
            lda     #$07                ; CRB: data select, rising edge, IRQB1 on
            sta     ROMPIA_CRB
            lda     ROMPIA_PRA
            lda     ROMPIA_PRB
            andcc   #$EF

            ldb     #$F0
            jsr     WaitLine

            orcc    #$10
            lda     #$04
            sta     ROMPIA_CRB

            lda     <IRQCNT
            sta     R_T3RCNT
            ldx     #EDGES
            ldy     #R_T3RLIN
            ldb     #4
            jsr     CopyB

            lda     #3
            sta     R_PHASE

; ===========================================================================
; Phase 4 -- T3F: CB1 falling edges
;
; Falls at 64, 128 and 192 inside the window. The fourth fall belongs to
; scanline 256, which begin_scanline skips (`if scanline != 256`), pushing it
; to 257 -- both read counter $00 and the PIA does not expose the CB1 level, so
; that one-line shift is not observable from software. See the design doc.
; ===========================================================================
T3F
            jsr     WaitWrap
            ldb     #$10
            jsr     WaitLine

            jsr     ArmEdges
            lda     #$04
            sta     ROMPIA_CRA
            lda     #$05                ; CRB: data select, FALLING edge, IRQB1 on
            sta     ROMPIA_CRB
            lda     ROMPIA_PRA
            lda     ROMPIA_PRB
            andcc   #$EF

            ldb     #$F0
            jsr     WaitLine

            orcc    #$10
            lda     #$04
            sta     ROMPIA_CRB

            lda     <IRQCNT
            sta     R_T3FCNT
            ldx     #EDGES
            ldy     #R_T3FLIN
            ldb     #3
            jsr     CopyB

            lda     #4
            sta     R_PHASE

; ===========================================================================
; Phase 5 -- T4: blitter halt duration, T5: the SC1 XOR-4 bug
;
; 8 x 128 = 1024 bytes into undisplayed columns $98-$9F. Fast is 1 cycle per
; byte (1024 cycles = 16 scanlines = counter delta $10); slow is 2 (delta $20).
; Both blits start at a known mid-frame line so neither crosses the counter
; wrap, where the u8 aliasing would corrupt the subtraction.
;
; NOTE: at the time of writing, machines/src/williams.rs:316 discards the cycle
; count do_dma_cycle() returns, so SLOW costs the same as FAST and T4_SLW is
; expected to come back $10. That is the bug this test exists to find.
; ===========================================================================
T4
            jsr     WaitWrap

            lda     #$00
            sta     BLIT_SOLID
            ldd     #$0000              ; dummy source, stride 256, stays in VRAM
            std     BLIT_SRCHI
            ldd     #$9800              ; columns $98-$9F, rows 0-127
            std     BLIT_DSTHI
            lda     #W_8
            sta     BLIT_W
            lda     #H_128
            sta     BLIT_H

            ldb     #$10                ; start at line 16
            jsr     WaitLine
            lda     VIDCOUNT
            sta     <TMP0
            lda     #FILLCTL
            sta     BLIT_CTRL           ; CPU is halted until the blit finishes
            lda     VIDCOUNT
            suba    <TMP0
            sta     R_T4FST

            ldb     #$40                ; start at line 64
            jsr     WaitLine
            lda     VIDCOUNT
            sta     <TMP0
            lda     #FILLSLOW
            sta     BLIT_CTRL
            lda     VIDCOUNT
            suba    <TMP0
            sta     R_T4SLW

; T5: width = height = 4. SC1 XORs 4 in, giving 0, which clamps to 1, so a
; single byte lands at $A000 and $A100 stays clear. Without the XOR this would
; be a 4x4 blit and $A100 would take $EE too.
            clra
            sta     $A000
            sta     $A100
            lda     #$EE
            sta     BLIT_SOLID
            ldd     #$A000
            std     BLIT_DSTHI
            lda     #WH_XORPROBE
            sta     BLIT_W
            sta     BLIT_H
            lda     #FILLCTL
            sta     BLIT_CTRL

            lda     $A000
            sta     R_T5A
            lda     $A100
            sta     R_T5B

            lda     #5
            sta     R_PHASE

; ===========================================================================
; Phase 6 -- fill the displayed area solid with pen 1
;
; Columns 3-148, rows 0-254, which covers every visible scanline (7-246).
; Split into three passes of 85 rows so the CPU is never halted for longer
; than a frame: 146 x 85 = 12,410 cycles, and the watchdog gets petted between
; passes. (Our watchdog is cosmetic, but the same binary should behave on
; hardware and in MAME.)
; ===========================================================================
Fill
            jsr     WaitWrap
            lda     #$11                ; pen 1 in both nibbles
            sta     BLIT_SOLID
            ldd     #$0000
            std     BLIT_SRCHI
            lda     #W_146
            sta     BLIT_W
            lda     #H_85
            sta     BLIT_H

            ldd     #$0300              ; column 3, rows 0-84
            jsr     FillPass
            ldd     #$0355              ; rows 85-169
            jsr     FillPass
            ldd     #$03AA              ; rows 170-254
            jsr     FillPass

            lda     #6
            sta     R_PHASE

; ===========================================================================
; Phase 7 -- T6: change the palette mid-frame  ->  capture A
;
; One store at line 120. begin_scanline renders line N before the CPU runs
; line N, so the change shows from the following line. With the counter's
; four-line resolution the boundary lands in scanlines 121-124, i.e. screen
; rows 114-117.
; ===========================================================================
T6
            jsr     WaitWrap
            ldb     #$78                ; line 120
            jsr     WaitLine
            lda     #GREEN
            sta     PALETTE+1
            ldb     #$F0                ; publish the phase late in the same frame
            jsr     WaitLine
            lda     #7
            sta     R_PHASE

; ===========================================================================
; Phase 8 -- T7: write VRAM above and below the beam  ->  capture B
;
; The load-bearing test. At line 120, write pen 2 into column 80 at rows 60 and
; 200. Row 200 has not been scanned out yet and must show this frame; row 60
; was drawn at scanline 60 and must not.
;
;   $503C = column 80, row 60   -> screen row 53,  x = 154,155
;   $50C8 = column 80, row 200  -> screen row 193, x = 154,155
; ===========================================================================
T7
            jsr     WaitWrap
            lda     #RED                ; undo T6 before this frame's first line
            sta     PALETTE+1
            ldb     #$78
            jsr     WaitLine
            lda     #$22                ; pen 2 in both nibbles
            sta     $503C
            sta     $50C8
            ldb     #$F0
            jsr     WaitLine
            lda     #8
            sta     R_PHASE

; ===========================================================================
; Phase 9 -- idle frame  ->  capture C
;
; No writes. Both rows must now show pen 2: row 60 because the beam reaches it
; from the top of this frame with the new VRAM already in place.
; ===========================================================================
Idle
            jsr     WaitWrap
            ldb     #$F0
            jsr     WaitLine
            lda     #9
            sta     R_PHASE

; ===========================================================================
; Done
;
; Hold phase 9 for a whole frame before publishing the last one. The harness
; reads the phase once per run_frame(), so a phase is only observable if it is
; the last one written in its frame. Writing 10 straight after 9 puts both at
; line 240 of the idle frame and phase 9 is never seen, which cost capture C.
; ===========================================================================
Done
            jsr     WaitWrap
            ldb     #$F0
            jsr     WaitLine
            lda     #10
            sta     R_PHASE
            lda     #$5A
            sta     R_MAGIC
            if      HWBUILD
            jmp     HwShow              ; render results, poll the button
            endif
Spin
            jsr     PetDog
            bra     Spin

; ===========================================================================
; Helpers
; ===========================================================================

; Return in the tail of the frame that just ended (scanlines 256-259), after
; every visible line, so a store issued on return cannot affect it.
; Preserves A.
WaitWrap
            pshs    a
            jsr     PetDog
WW1
            lda     VIDCOUNT
            cmpa    #$C0
            blo     WW1                 ; climb to the bottom of the frame
WW2
            lda     VIDCOUNT
            cmpa    #$10
            bhs     WW2                 ; ... then wait for the wrap
            puls    a,pc

; Spin until the counter reads B. The loop is ~14 cycles against a 256-cycle
; counter step, so it cannot step over the target. Preserves A.
WaitLine
            pshs    a
            stb     <WTARGET
WL1
            lda     VIDCOUNT
            cmpa    <WTARGET
            bne     WL1
            puls    a,pc

; Reset the interrupt edge capture: count to zero, write pointer to the table.
ArmEdges
            pshs    a,x
            clr     <IRQCNT
            ldx     #EDGES
            stx     <IRQPTR
            clr     <EDGES
            clr     <EDGES+1
            clr     <EDGES+2
            clr     <EDGES+3
            puls    a,x,pc

; Copy B bytes from X to Y.
CopyB
            pshs    a
CopyB1
            lda     ,x+
            sta     ,y+
            decb
            bne     CopyB1
            puls    a,pc

; One blitter pass with the destination in D. Clobbers nothing the callers rely
; on beyond D.
FillPass
            std     BLIT_DSTHI
            lda     #FILLCTL
            sta     BLIT_CTRL
            jsr     PetDog
            rts

; The Williams watchdog only clears on a write of $39 to $CBFF.
PetDog
            pshs    a
            lda     #$39
            sta     WATCHDOG
            puls    a,pc

; ===========================================================================
; Interrupt handler
;
; Records the video counter at each PIA edge, then clears the flag by reading
; both data registers. Both control registers keep bit 2 set so those reads hit
; the data register and actually clear.
; ===========================================================================
IrqHandler
            lda     VIDCOUNT
            ldb     <IRQCNT
            cmpb    #4
            bhs     IrqFull             ; table full -- keep counting, stop storing
            ldx     <IRQPTR
            sta     ,x+
            stx     <IRQPTR
IrqFull
            incb
            stb     <IRQCNT
            lda     ROMPIA_PRA
            lda     ROMPIA_PRB
            rti

; Everything else returns immediately rather than wandering into whatever
; happens to be at the vector.
StrayIrq
            rti

            if      HWBUILD
; ===========================================================================
; Hardware build: on-screen results and button paging (HWBUILD=1 only)
;
; After Done publishes the magic byte, this renders the 22 result bytes as hex
; beside their expectations and polls a start button to page and re-run. It
; only READS the result block, so the block stays byte-identical with the
; tested build; re-run jumps to Reset, which re-clears the block, so a wedge
; still reads as a wedge.
;
; Numbers, never PASS: every page shows raw hex, including magic and phase in
; the header row, so a wedged run is legible as one on any page. One pen
; throughout (pen 1 on pen 0): no meaning rides on colour.
;
; Williams is a bitmap board with no character generator, so the ROM carries
; sixteen 8x8 one-bit glyphs (128 bytes), drawn at 2x through plain CPU stores.
; VRAM is column-major (column*256 + row) and a byte holds two 4-bit pixels,
; so one font pixel is one byte, and a glyph row is 8 bytes stepped one column
; ($100) at a time; each row is drawn twice for the 2x height. The blitter is
; left alone once the tests are done.
;
; Layout: 9 cells by 12 rows. A cell is 9 font columns (8 + 1 gap) by 18
; screen rows (16 + 2 gap), so the page is 81 font columns (162 screen pixels)
; by 216 rows, at VRAM columns $23-$73 and rows $13-$E9 (screen x 64-225,
; rows 12-227). Row 0 is the header (magic, phase actuals); rows 1-11 are
; index, expected, actual triplets for 11 of the 22 result bytes, page 0
; covering indices 0-10 and page 1 indices 11-21. Pages identify themselves by
; their index column; no page number is rendered.
;
; The dwell bytes (indices 5, 6) carry no exact expectation: the harness checks
; their ratio, not their value, and the same holds on hardware, where the ratio
; is the finding (about 2 where the counter aliases, about 1 where it
; saturates). HwExact marks them and the renderer leaves those expected cells
; blank rather than printing a number that could never match.
;
; Button: Widget PIA port A at $C804, bits 4-5, active high. Those are the two
; start buttons on Joust (P1 = bit 5) and Robotron (P1 = bit 4), so either
; cabinet's 1P start pages. The PIA comes out of reset with DDRAs as inputs;
; only control bit 2 is set, selecting the data register with IRQs off. A press
; advances the page, a press on the last page re-runs the program; press and
; release are each settled over a frame boundary so contact bounce cannot skip.
; ===========================================================================

WIDGET_PRA  equ $C804           ; Widget PIA port A: player inputs, active high
WIDGET_CRA  equ $C805           ; ... control A: bit 2 selects data register
BTN_START   equ $30             ; either start button (bits 4-5)

HWPAGE      equ $0B             ; displayed page (0/1); cleared in HwShow
HWBIT       equ $0C             ; pixel counter for DxRun
HWIDX       equ $0D             ; result index HwHasExp reads
HWROW       equ $0E             ; text row counter for DrawPage

TX_COL0     equ $23             ; first text column (screen x 64)
TX_ROW0     equ $13             ; first text row (screen row 12)
TX_CLEARW   equ 81              ; text region width in VRAM columns
TX_CLEARH   equ 216             ; ... and height in rows (12 rows x 18)

; Expected results, one byte per result-block index 0-21. Mirrors the harness
; contract in williams_video_timing_test.rs; indices 5-6 are don't-cares (see
; HwExact) and read $00.
HwExpect    fcb $5A,$0A,$40,$01,$FC,$00,$00,$01,$F0,$04
            fcb $20,$60,$A0,$E0,$03,$40,$80,$C0,$10,$20,$EE,$00

; Bit i set = result byte i has an exact expectation. Bits 5,6 clear (dwell
; ratio): byte 0 = $FF with $60 cleared = $9F.
HwExact     fcb $9F,$FF,$3F

; Sixteen 8x8 glyphs, bit 7 = leftmost pixel, seven ink rows + one blank.
HwFont
            fcb $3C,$66,$6E,$7E,$76,$66,$3C,$00     ; 0
            fcb $18,$38,$18,$18,$18,$18,$7E,$00     ; 1
            fcb $3C,$66,$06,$0C,$30,$60,$7E,$00     ; 2
            fcb $7E,$0C,$18,$0C,$06,$66,$3C,$00     ; 3
            fcb $0C,$1C,$3C,$6C,$7E,$0C,$0C,$00     ; 4
            fcb $7E,$60,$7C,$06,$06,$66,$3C,$00     ; 5
            fcb $3C,$66,$60,$7C,$66,$66,$3C,$00     ; 6
            fcb $7E,$66,$0C,$18,$18,$18,$18,$00     ; 7
            fcb $3C,$66,$66,$3C,$66,$66,$3C,$00     ; 8
            fcb $3C,$66,$66,$3E,$06,$66,$3C,$00     ; 9
            fcb $3C,$66,$66,$7E,$66,$66,$66,$00     ; A
            fcb $7C,$66,$66,$7C,$66,$66,$7C,$00     ; B
            fcb $3C,$66,$60,$60,$60,$66,$3C,$00     ; C
            fcb $78,$6C,$66,$66,$66,$6C,$78,$00     ; D
            fcb $7E,$60,$60,$7C,$60,$60,$7E,$00     ; E
            fcb $7E,$60,$60,$7C,$60,$60,$60,$00     ; F

HwShow
            orcc    #$10                ; mask IRQ: the edge handler is done
            clr     <HWPAGE              ; re-runs restart on page 0
            lda     #$04
            sta     WIDGET_CRA          ; port A reads inputs, IRQs stay off
            jsr     HwClear
            jsr     DrawPage
HwBtn
            jsr     PetDog
            lda     WIDGET_PRA
            anda    #BTN_START
            beq     HwBtn               ; wait for a start button
            jsr     WaitWrap            ; settle a frame: still held?
            jsr     PetDog
            lda     WIDGET_PRA
            anda    #BTN_START
            beq     HwBtn               ; bounce, ignore
HwRel
            jsr     PetDog
            lda     WIDGET_PRA
            anda    #BTN_START
            bne     HwRel               ; wait for release
            jsr     WaitWrap            ; settle a frame: still released?
            jsr     PetDog
            lda     WIDGET_PRA
            anda    #BTN_START
            bne     HwRel               ; bounce, keep waiting
            lda     <HWPAGE
            bne     HwRerun             ; last page: re-run the whole program
            inc     <HWPAGE
            jsr     HwClear
            jsr     DrawPage
            bra     HwBtn
HwRerun
            jmp     Reset               ; ClrRes runs again, wedge reads as wedge

; Clear the text region to pen 0, one VRAM column per watchdog pet. VRAM is
; column-major (column*256 + row), so a column is 216 consecutive stores.
HwClear
            pshs    a,y
            clra
            ldy     #TX_CLEARW
            ldx     #TX_COL0*256+TX_ROW0
HwClrCol
            pshs    x
            ldb     #TX_CLEARH
HwClrRow
            clr     ,x+
            decb
            bne     HwClrRow
            puls    x
            jsr     PetDog
            leax    $100,x              ; next column, same first row
            leay    -1,y
            bne     HwClrCol
            lda     #$BB                ; TMP MARKER: clear done
            sta     RES+23
            puls    a,y,pc

; Render the current page: header (magic, phase) plus 11 index/expected/
; actual triplets starting at index 0 (page 0) or 11 (page 1).
DrawPage
            pshs    a,b,x,y
            lda     #$CC                ; TMP MARKER: draw started
            sta     RES+24
            ldx     #TX_COL0*256+TX_ROW0
            lda     R_MAGIC
            jsr     DrawByte            ; header cells 0-1
            leax    $900,x              ; cell 3
            lda     R_PHASE
            jsr     DrawByte            ; header cells 3-4
            lda     <HWPAGE
            beq     HwPg0
            lda     #11
            bra     HwPgIdx
HwPg0
            clra
HwPgIdx
            sta     <HWIDX
            lda     #1
            sta     <HWROW
HwRowLoop
            lda     <HWROW
            ldb     #18
            mul                           ; D = row*18 (rows are 18 apart)
            ldx     #TX_COL0*256+TX_ROW0
            leax    d,x
            stx     RES+27                ; TMP MARKER: row-start X
            lda     <HWIDX
            jsr     DrawByte            ; index digits, cells 0-1
            leax    $900,x              ; cell 3
            ldb     <HWIDX
            jsr     HwHasExp            ; Z set = no exact expectation
            beq     HwNoExp
            ldy     #HwExpect
            ldb     <HWIDX
            lda     b,y
            jsr     DrawByte            ; expected digits, cells 3-4
            bra     HwGotExp
HwNoExp
            leax    $1200,x             ; blank cell pair, cells 3-4 stay clear
HwGotExp
            leax    $900,x              ; cell 6
            ldy     #RES
            ldb     <HWIDX
            lda     b,y
            jsr     DrawByte            ; actual digits, cells 6-7
            inc     <HWIDX
            inc     <HWROW
            lda     <HWROW
            cmpa    #12
            bne     HwRowLoop
            lda     #$DD                ; TMP MARKER: draw done
            sta     RES+25
            puls    a,b,x,y,pc

; B = result index 0-21 (read from HWIDX so no register juggling). Z set on
; return = no exact expectation (draw the expected cells blank). Preserves
; A,B,X,Y; the Z flag survives the pulls (PULS leaves CC alone).
HwHasExp
            pshs    a,b,y
            ldb     <HWIDX
            lsrb
            lsrb
            lsrb                        ; B = mask byte offset 0-2
            ldy     #HwExact
            lda     b,y                 ; A = mask byte
            ldb     <HWIDX
            andb    #$07                 ; B = bit number
            beq     HwHeOut
HwHeSh
            lsra
            decb
            bne     HwHeSh
HwHeOut
            anda    #$01
            puls    a,b,y,pc

; A = byte value. Draws hi,lo hex digits at X and X+$900; X advances $1200
; (two cells). Clobbers A; preserves B,Y.
DrawByte
            pshs    a
            lsra
            lsra
            lsra
            lsra
            jsr     DrawHex
            leax    $900,x
            puls    a
            anda    #$0F
            jsr     DrawHex
            leax    $900,x
            rts

; A = hex digit 0-15. X = cell top-left VRAM address. Draws the 8x8 glyph at
; 2x: 8 bytes across one screen row per font row, each row drawn twice (the
; second pass is the row below, so the glyph is 16 rows tall). Preserves A,X;
; clobbers B,Y,HWBIT.
DrawHex
            pshs    a,b,x,y
            ldb     #8
            mul                           ; D = digit*8
            ldy     #HwFont
            leay    d,y
            ldb     #8                    ; 8 font rows
DxRowLp
            pshs    b                     ; row counter (B holds the font below)
            ldb     ,y+
            bsr     DxRowPair             ; rows R,R+1; X unchanged
            leax    2,x                   ; next font row starts 2 rows down
            puls    b
            decb
            bne     DxRowLp
            puls    a,b,x,y,pc

; B = font byte, bit 7 first. X = screen-row address. Writes 8 bytes across
; the row (one per font pixel, both nibbles one pen) at columns X..X+$700,
; then the same 8 bytes on the row below (2x tall). X is preserved for the
; caller. Clobbers A,B,HWBIT.
DxRowPair
            pshs    x                     ; save the row origin
            lda     #8
            sta     <HWBIT
DxRowPx
            lslb                          ; pixel into carry, 0 into bit 0
            lda     #$11                  ; pen 1, both nibbles
            bcs     DxRowInk
            clra                          ; pen 0
DxRowInk
            sta     ,x                    ; row R
            sta     1,x                   ; row R+1
            leax    $100,x                ; next column, same row
            dec     <HWBIT
            bne     DxRowPx
            puls    x,pc
            endif

; ===========================================================================
; Vectors
;
; The gap between the code above and this table is not emitted by the
; assembler; p2bin's -r window and -l fill byte are what turn the two segments
; into one flat $D000-$FFFF image with zeroes in between. Getting that wrong
; produces a short file rather than a wrong one, and the harness test
; the_image_fills_the_program_rom_window catches it.
; ===========================================================================
            org     $FFF0
            fdb     StrayIrq            ; $FFF0 reserved
            fdb     StrayIrq            ; $FFF2 SWI3
            fdb     StrayIrq            ; $FFF4 SWI2
            fdb     StrayIrq            ; $FFF6 FIRQ
            fdb     IrqHandler          ; $FFF8 IRQ
            fdb     StrayIrq            ; $FFFA SWI
            fdb     StrayIrq            ; $FFFC NMI
            fdb     Reset               ; $FFFE RESET
