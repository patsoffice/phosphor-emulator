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
