; ---------------------------------------------------------------------------
; Atari System 1 (Road Runner) video timing conformance ROM
;
; Design: docs/designs/roadrunner-video-conformance.md
; Assemble:
;   asl -q -o roadrunner_video.p roadrunner_video.asm
;   p2bin roadrunner_video.p roadrunner_video.bin -r 0x0000-0x1FFF -l 0xA5
;
; Both tools are in the Nix dev shell. p2bin's -r fixes the image at exactly
; 8 KB and -l fills the gaps between the vector table, the code and the end of
; the window, which is what makes the load a flat copy and the checksum below a
; fixed quantity.
;
; THE FILL BYTE IS 0xA5 AND NOT ZERO, FOR TWO REASONS. A ROM-less board's
; program ROM is already all zeroes, so a zero fill would make the checksum
; blind over the 85% of the image that is padding: a truncated load would sum
; the same as a complete one. And $A5A5 decodes as line-A, which vectors to the
; stray handler, so a runaway program counter that wanders into the padding
; records itself instead of grinding through 3.5 KB of ORI.B #0,D0.
;
; Loaded by machines/tests/roadrunner_video_timing_test.rs into a ROM-less
; machine, by poking the image through BusDebug::write (AddressSpace32's
; debug_write ignores AccessKind, so the ReadOnly program-ROM region takes the
; write). The 68000 fetches its stack pointer from 0 and its program counter
; from 4 through the bus, so both come from this image.
;
; EVERY WAIT IS A POLL OF HARDWARE STATE, NEVER A DELAY LOOP, so a constant
; cycle offset between two implementations cancels.
;
; AND EVERY POSITION IS MEASURED IN ITERATIONS OF ONE SHARED POLL LOOP, never in
; cycles or scanlines. The program has no line counter to read: this board's
; only beam-position primitives are the VBLANK level and the motion-object timer
; interrupt. So the loop is calibrated in the same run that uses it -- T1 counts
; iterations across the 240 active lines, which gives iterations-per-line, and
; every later figure is divided by that. Nothing here is compared against a
; constant that was measured once and written down.
; ---------------------------------------------------------------------------

            cpu     68000

; --- Hardware ---------------------------------------------------------------

INT3STATE   equ $2E0000         ; bit 7 = motion-object scanline interrupt (IRQ3)
XSCROLL     equ $800000         ; playfield scroll, both left at 0 here
YSCROLL     equ $820000
PRIORITY    equ $840000         ; colour-0 pens the playfield draws in front of
PF          equ $A00000         ; playfield RAM, 64 x 64 cells of one word
MOB         equ $A02000         ; motion-object RAM, 8 banks of 64 entries
ALPHA       equ $A03000         ; alphanumerics RAM, 64 x 32 cells of one word
PAL         equ $B00000         ; palette RAM, 1024 entries of one IRGB-4444 word
BANKSELECT  equ $860001         ; bit 7 = sound-CPU run, 5-3 = MO bank, 2 = PF bank
WATCHDOG    equ $880001         ; any write clears the counter
VBLANK_ACK  equ $8A0001         ; write acks the VBLANK IRQ4 latch
SWITCHES    equ $F60000         ; bit 4 = VBLANK, ACTIVE LOW (0 during blank)
SNDRESP     equ $FC0001         ; sound response; reading it acknowledges IRQ6

VB_MASK     equ $0010           ; the VBLANK bit inside the word read of F60000
INT3_MASK   equ $0080           ; the IRQ3 bit inside the word read of 2E0000

SR_MASKON   equ $0700           ; SR interrupt mask bits, all set (level 7)
SR_MASKOFF  equ $F8FF           ; ... and their complement, for ANDI to SR

; A motion-object entry is four words in SPLIT layout: entry N's words sit
; 0x40 words apart, at base+N, base+0x40+N, base+0x80+N and base+0xC0+N. Word 1
; holding $FFFF marks the entry as a scanline timer rather than a sprite.
MOB_E0_W0   equ MOB+$000
MOB_E0_W1   equ MOB+$080
MOB_E0_W2   equ MOB+$100
MOB_E0_W3   equ MOB+$180
MOB_E1_W0   equ MOB+$002
MOB_E1_W1   equ MOB+$082
MOB_E1_W2   equ MOB+$102
MOB_E1_W3   equ MOB+$182

TIMER_FLAG  equ $FFFF

; The band a timer entry fires at is (256 - (word0 >> 5) - vsize*8 - 1) & $1FF,
; with vsize = (word0 & $0F) + 1. Taking vsize = 1 (low nibble zero) that is
; 247 - (word0 >> 5), so word0 = (247 - line) << 5 names the line directly.
; Written out as literals with the arithmetic beside them, so the ROM asserts
; against a line rather than against whatever the board computes.
TIMER_L1    equ $16E0           ; (247 -  64) << 5 = 183 << 5  -> line 64
TIMER_L2    equ $0AE0           ; (247 - 160) << 5 =  87 << 5  -> line 160
TIMER_L3    equ $0FE0           ; (247 - 120) << 5 = 127 << 5  -> line 120

; --- The picture -----------------------------------------------------------
;
; The harness installs a synthetic font and tile set before the program runs; a
; ROM-less board has no graphics at all, and without them the playfield and the
; motion objects are every pen 0 and nothing can be drawn. The tiles are solid
; blocks of pens 1 to 4 and the font is a handful of glyphs, both defined in
; roadrunner_video_timing_test.rs, so the picture below is derivable rather than
; captured.
;
; Palette indices, from the compositor's shared 1024-entry space: alpha at
; colour*4 + pen, motion objects at $100 + pen, playfield at $200 + pen (the
; synthetic PROM puts both layers in colour 0).

PAL_AL1     equ PAL+$002        ; alpha colour 0, pen 1
PAL_AL2     equ PAL+$004        ; alpha colour 0, pen 2
PAL_MO1     equ PAL+$202        ; motion object pen 1
PAL_MO2     equ PAL+$204        ;                pen 2
PAL_MO3     equ PAL+$206        ;                pen 3
PAL_MO4     equ PAL+$208        ;                pen 4
PAL_PF0     equ PAL+$400        ; playfield pen 0
PAL_PF1     equ PAL+$402        ;           pen 1, the colour T6 changes mid-frame
PAL_PF2     equ PAL+$404        ;           pen 2, what T7's cells become
PAL_ALO     equ PAL+$008        ; alpha colour 1, pen 0: what a force-opaque cell
                                ; paints, since colour*4 + pen = 4
; A high-priority sprite does not draw its own colour: it blends through the
; translucent bank at $300 + (playfield pen << 4) + sprite pen. Over the pen-1
; background with a pen-2 sprite that is $312.
PAL_TRANS   equ PAL+$624

; IRGB-4444: IIII RRRR GGGG BBBB, an intensity nibble scaling each component.
C_BLACK     equ $0000
C_RED       equ $FF00
C_GREEN     equ $F0F0
C_BLUE      equ $F00F
C_WHITE     equ $FFFF
C_YELLOW    equ $FFF0
C_MAGENTA   equ $FF0F

; A playfield cell word is flip:tile-select:code. With the synthetic PROM the
; high byte selects lookup entry 0 for every value, so the low byte is the tile.
PF_TILE1    equ $0001           ; solid pen 1
PF_TILE2    equ $0002           ; solid pen 2
PF_TILE5    equ $0005           ; left half pen 1, right half pen 2
PF_FLIP5    equ $8005           ; ... and mirrored, which swaps the halves
PF_HFLIP    equ $8000           ; cell bit 15 mirrors the 8x8 tile
                                ; tile 6 is top half pen 1, bottom half pen 2

; Cells the compositor phase writes. Cell (row, col) is at PF + (row*64+col)*2.
PF_PRIOCELL equ PF+((10*64)+7)*2   ; screen (56-63, 80-87), under a sprite
PF_MIRROR   equ PF+((20*64)+2)*2   ; screen (16-23, 160-167)
PF_MIRRORF  equ PF+((20*64)+3)*2   ; the mirrored one beside it

; Alpha cell (22, 2): screen (16-23, 176-183). Bit 13 forces the cell opaque even
; where its glyph is pen 0, and bits 12-10 are the colour.
AL_OPAQUE   equ ALPHA+((22*64)+2)*2
AL_OPAQVAL  equ $2400           ; force-opaque, colour 1, code 0 (blank glyph)

; THIS BOARD'S ALPHA LAYER HAS NO FLIP, and the pair below is what says so.
; An alpha cell word is code in bits 0-9, colour in 10-12 and the force-opaque
; flag in 13; bits 14 and 15 do nothing. Two cells of the same asymmetric glyph,
; one with both spare bits set, must come out pixel for pixel identical.
;
; Both cells are at alpha row 24, screen rows 192-199.
AL_PLAIN    equ ALPHA+((24*64)+6)*2    ; screen columns 48-55
AL_SPARE    equ ALPHA+((24*64)+8)*2    ; screen columns 64-71
AL_GLYPH    equ 1                      ; 'R', asymmetric in both axes
AL_SPAREBITS equ $C000

; Colour-0 playfield pens the playfield draws in FRONT of, written to 840000. Bit
; 2 set means a low-priority sprite loses to playfield pen 2 and still wins over
; pen 1, which is what makes the pair of sprites below say something.
PRIO_PENS   equ $0004

; The two cells T7 writes, one well above the beam at the moment of the write and
; one well below it. Cell (row, col) is at PF + (row*64 + col)*2, and covers
; screen rows row*8 to row*8+7 with both scrolls at zero.
PF_ABOVE    equ PF+((6*64)+4)*2    ; screen rows 48-55
PF_BELOW    equ PF+((25*64)+4)*2   ; screen rows 200-207

; Where the text goes: alpha cell (2, 8), i.e. screen row 16, column 64.
TEXTCELL    equ ALPHA+((2*64)+8)*2

; The sprite: 4 tiles tall at screen (160, 80), tile codes 1 to 4 down its
; length. draw_mo_entry reads height from word0's low nibble, the Y position from
; word0 bits 5-13 as `-(v) - 256 - height*8` wrapped into 512, and X from word2
; bits 5-13. So v = 512 - ((80 + 256 + 32) mod 512) = 144, and word0 is
; (144 << 5) | 3.
SPR_W0      equ $1203           ; height 4, Y 80
SPR_W1      equ $0001           ; colour:code -> lookup entry 0, base tile 1
SPR_W2      equ $1400           ; X 160 (160 << 5), low priority

; The single-tile sprites the compositor phase adds. Same arithmetic, height 1,
; so v = 512 - ((Y + 256 + 8) mod 512):
;   Y 40 -> v 208 -> $1A00     Y 60 -> v 188 -> $1780     Y 80 -> v 168 -> $1500
; word0 bit 15 mirrors the tile; word2 bit 15 makes the sprite high priority.
SPR_Y40     equ $1A00
SPR_Y60     equ $1780
SPR_Y80     equ $1500
SPR_Y120    equ $1000           ; Y 120 -> v 128

; NEITHER LAYER ON THIS BOARD HAS A VERTICAL FLIP. A motion-object word0 is
; height in bits 0-3, Y in 5-13 and the horizontal mirror in 15; word2 is X in
; 5-13 and priority in 15. Bit 4 and bit 14 of word0, and bits 0-4 and 14 of
; word2, are not decoded by anything. Two sprites of the same vertically
; asymmetric tile, one with every one of those bits set, must come out identical.
SPR_SPARE0  equ $4010
SPR_SPARE2  equ $401F
SPR_X40     equ $0500           ;  40 << 5
SPR_X56     equ $0700           ;  56 << 5
SPR_MIRROR  equ $8000           ; word0: mirror
SPR_HIPRIO  equ $8000           ; word2: high priority

; Scroll applied by the last capture. The playfield map is 512 x 512 and the
; visible origin is the scroll, so screen (x, y) shows map (x + 8, y + 8) and
; everything drawn on the playfield moves up and left by 8. The alpha and motion
; object layers do not scroll, which is half of what this pins.
SCROLL_BY   equ 8

; --- The image, and the range the CPU checksums back through the real bus ----

IMGBASE     equ $000000
IMGWORDS    equ $1000           ; 8 KB / 2

; Work RAM is 400000-401FFF (8 KB, undisplayed). Unlike Williams there is no
; video RAM to hide scratch in, and no need: the result block, the variables and
; the stack all live here with kilobytes to spare between them.
STACKTOP    equ $401F00

; --- Result block ($400000, work RAM) ---------------------------------------
;
; Words rather than bytes. A byte write on this bus is a read-modify-write of
; the containing word (atari_system1.rs:39-43), and there is no shortage of work
; RAM, so there is no reason to pack.

RES         equ $400000
R_MAGIC     equ RES+0           ; $5A5A on completion
R_PHASE     equ RES+2
R_TRAP      equ RES+4           ; $DEAD if a stray exception was taken
R_TRAPV     equ RES+6           ; ... and the 68010 frame's vector-offset word
R_SSP       equ RES+8           ; long: A7 as cpu.reset handed it over
R_CKSUM     equ RES+12          ; 16-bit wrapping sum of the whole image
R_VBCOUNT   equ RES+14          ; vblank edges ridden with the watchdog strobed

R_T1_BLANK  equ RES+16          ; poll iterations while VBLANK is asserted
R_T1_ACTIVE equ RES+18          ; ... and while the display is active
R_T1_BLANK2 equ RES+20          ; the blank again, a frame later

R_T2_COUNT  equ RES+22          ; IRQ4 entries in one frame, acked at once
R_T2_HELD   equ RES+24          ; ... with the ack deferred by one entry
R_T2_VB     equ RES+26          ; 1 if VBLANK was asserted inside the handler

R_T3_POLL_A equ RES+28          ; iterations from the vblank edge to IRQ3, line 64
R_T3_END_A  equ RES+30          ; ... to the end of that pulse
R_T3_POLL_B equ RES+32          ; the same measurement with the timer at line 160

R_T4_CNT    equ RES+34          ; IRQ3 handler entries in one frame
R_T4_FIRST  equ RES+36          ; loop count at the first handler entry

R_T5_POLL   equ RES+38          ; line-160 pulse, with that timer installed
                                ; mid-frame by the line-64 pulse

R_TIMEOUT   equ RES+40          ; $DEAD if a wait gave up; R_PHASE says where

RESLEN      equ 48

MAGIC       equ $5A5A
TRAPPED     equ $DEAD           ; R_TRAP, and R_TIMEOUT for a wait that gave up
IRQ3STORM   equ $DEA3           ; R_TIMEOUT when IRQ3 would not stop firing

; How many IRQ3 entries in one measurement are too many to be a one-scanline
; pulse. A scanline is 456 cycles and this handler cannot be much under 60 of
; them, so a real pulse cannot re-enter more than about eight times; a level
; latched for the rest of a frame re-enters hundreds. The cap is an order of
; magnitude above the first and two below the second, and it is a bound rather
; than an expectation: nothing asserts against it, it only stops the program
; from disappearing into an interrupt storm.
;
; It is needed because the obvious bound does not work. A count-based limit in
; the polling loop cannot expire during a storm: RTE drops straight back into
; the handler, so the loop that maintains the count never runs a single
; iteration. Only the handler can bound the handler.
IRQ3CAP     equ 64

; Vblank edges to survive before declaring the watchdog fed. Deliberately double
; the 8-frame timeout: the program cannot reach this count unless every strobe
; landed, because a reboot clears the result block and starts the count over.
VB_TARGET   equ 16

; --- Variables ($400100, work RAM, above the result block) ------------------
;
; Everything the interrupt handlers touch lives here rather than in registers,
; so a handler cannot disturb the loop it interrupted.

VARS        equ $400100
V_COUNTER   equ VARS+0          ; the shared poll loop's iteration count
V_IRQ4CNT   equ VARS+2
V_ACKSKIP   equ VARS+4          ; IRQ4 entries still to pass without acking
V_VBSEEN    equ VARS+6
V_IRQ3CNT   equ VARS+8
V_IRQ3FIRST equ VARS+10
V_LIMIT     equ VARS+12         ; iterations a wait has left before giving up

; EVERY WAIT IS BOUNDED, and that is a lesson rather than defensive habit. The
; first version spun forever, so a board whose IRQ3 never released made the
; program hang, the watchdog reboot it, and the harness report a wedge at
; whatever phase the restarted run happened to be in. The stage that actually
; broke was nowhere in the message. A bounded wait turns every such regression
; into "gave up in phase N", which is the difference between a test that fails
; and a test that says what failed.
;
; The bound has to be long enough for the longest honest wait (a little over one
; frame, about 2000 iterations) and short enough to give up before the 8-frame
; watchdog reboots and destroys the evidence. 12000 iterations is about six
; frames in the counted loops and four in the vblank loop, which sits between
; the two with room on both sides.
WAITLIMIT   equ 12000

; ===========================================================================
; Exception vectors
;
; Vector 0 is the supervisor stack pointer and vector 1 is the entry point;
; cpu.reset fetches both through the bus, so they are the load-bearing halves of
; this image. The two autovectors the board actually drives get handlers.
; EVERY OTHER VECTOR POINTS AT A HANDLER rather than at zero, so a mistake
; records itself instead of executing whatever happens to be at address 0.
;
; Autovector N is 24 + N, so level 3 (the motion-object scanline interrupt) is
; vector 27 and level 4 (VBLANK) is vector 28. Levels 6 (sound response) and 2
; (the ADC) are left on the stray handler on purpose: the sound CPU is held in
; reset and the ADC is never started, so neither line can assert, and if one
; does it is a finding rather than something to swallow.
; ===========================================================================
            org     IMGBASE

            dc.l    STACKTOP            ;  0: reset SSP
            dc.l    Reset               ;  1: reset PC
            rept    25
            dc.l    StrayException      ;  2-26
            endm
            dc.l    Irq3Handler         ; 27: autovector level 3
            dc.l    Irq4Handler         ; 28: autovector level 4
            rept    227
            dc.l    StrayException      ; 29-255
            endm

; ===========================================================================
; Entry
; ===========================================================================
            org     $000400
Reset
            move.l  a7,d7               ; the SSP cpu.reset fetched from vector 0,
                                        ; recorded before anything can disturb it
            move.l  #STACKTOP,a7        ; ... then set explicitly, as a program would

            bsr     PetDog

; Hold the sound CPU in reset, and do it as a TRANSITION rather than a level.
;
; A running sound CPU latches responses and those drive IRQ6, which outranks
; everything measured here. Writing 0 alone is not enough to stop it: the reset
; line is driven from bit 7 of this latch, and a board that only acts when bit 7
; CHANGES will do nothing at all if the latch already reads 0, which is exactly
; what it reads from power-on. Writing $80 and then $00 forces the edge whatever
; the latch held, and on hardware that acknowledges the sound-response latch on
; that edge it also clears any response already waiting.
;
; This cost a run. Under MAME the sound CPU runs free from power-on for that
; reason, had a response latched by the time this program took over, and IRQ6
; fired into the stray-exception handler at the first unmask. See the design doc.
            move.b  #$80,BANKSELECT     ; release, forcing a change on bit 7
            move.b  #$00,BANKSELECT     ; ... then assert reset and acknowledge
            tst.b   SNDRESP             ; drain a response the latch still holds

; Bit 7 clear also selects motion-object bank 0 and playfield bank 0, which is
; what the rest of the program assumes.

; A zero result block must never read as a pass, so clear it deliberately.
; Clearing it is also what makes a watchdog reboot visible: reset() does not
; clear work RAM, so without this a reboot would leave a plausible
; half-finished block behind instead of restarting the counts.
            lea     RES,a0
            moveq   #(RESLEN/2)-1,d0
ClrRes
            move.w  #0,(a0)+
            dbra    d0,ClrRes

            lea     VARS,a0
            moveq   #7,d0
ClrVars
            move.w  #0,(a0)+
            dbra    d0,ClrVars
            move.w  #WAITLIMIT,V_LIMIT

; CLEAR THE VIDEO RAM THIS PROGRAM DOES NOT OTHERWISE WRITE, because it cannot
; assume the machine was cold when it took over.
;
; Every entry of palette RAM, and every word of the motion-object list. The
; program writes nine palette entries and ten sprite-list entries and used to
; leave the other 1015 and 2038 holding whatever was there. On a board that
; powered up into this program that is zeroes; on a board that was running a game
; first it is the game's palette and the game's sprites, and the picture then
; depends on what was on screen beforehand. Under MAME it did: leftover motion
; objects drew down the left edge and unwritten palette entries came out in the
; attract mode's colours.
;
; The playfield and the alpha layers are filled outright in phase 11, so they
; need nothing here.
            lea     PAL,a0
            move.w  #1024-1,d0
ClrPal
            move.w  #C_BLACK,(a0)+
            dbra    d0,ClrPal

            lea     MOB,a0
            move.w  #2048-1,d0
ClrMob
            move.w  #0,(a0)+
            dbra    d0,ClrMob
            bsr     PetDog

            move.l  d7,R_SSP
            move.w  #1,R_PHASE

; ===========================================================================
; Phase 2 -- checksum the whole image back through the real bus
;
; The poke went in through the debug bus; this reads all 8 KB out through the
; bus the CPU actually drives, which is what makes "the loader worked" a
; measurement rather than an inference. The range stops well short of the
; slapstic window at 080000, whose state machine advances on any data access to
; it and changes banks silently rather than faulting.
; ===========================================================================
            lea     IMGBASE,a0
            moveq   #0,d0
            move.w  #IMGWORDS-1,d1
CkLoop
            add.w   (a0)+,d0
            dbra    d1,CkLoop
            move.w  d0,R_CKSUM

            bsr     PetDog
            move.w  #2,R_PHASE

; ===========================================================================
; Phase 3/4 -- ride VB_TARGET vblank edges with the watchdog strobed at each
;
; The watchdog is the thing that breaks a program on this board first:
; run_frame reboots the machine after 8 frames without a write to 880001
; (roadrunner.rs:773-775). Riding twice that many frames is the assertion that
; the strobe lands, and the frame the harness first sees phase 4 in is the
; assertion that one frame produces one vblank edge.
; ===========================================================================
            moveq   #0,d6               ; vblank edges seen
VbLoop
            bsr     WaitVblank
            bsr     PetDog
            addq.w  #1,d6
            move.w  d6,R_VBCOUNT
            cmpi.w  #1,d6
            bne.s   VbNotFirst
            move.w  #3,R_PHASE
VbNotFirst
            cmpi.w  #VB_TARGET,d6
            bne     VbLoop

            move.w  #4,R_PHASE

; ===========================================================================
; Phase 5 -- T1: the VBLANK level, and the calibration everything else uses
;
; F60000 bit 4 is the live VBLANK line, active low. The board blanks from
; scanline 240 to 261 of 262, so the expectation is 22 blanked lines against 240
; active ones. Measured as three consecutive dwells rather than two, because a
; ratio that is right once can be right by accident; the second blank is there
; to show the first was not a fluke.
;
; R_T1_ACTIVE divided by 240 is iterations-per-line, and every position measured
; below is divided by it. That is why the whole program shares one poll loop.
; ===========================================================================
            bsr     PetDog
            lea     SWITCHES,a0
            move.w  #VB_MASK,d1

            bsr     WaitVblank          ; land on the transition into blank
            clr.w   V_COUNTER
            bsr     WaitSet             ; count the blank out
            move.w  d0,R_T1_BLANK
            clr.w   V_COUNTER
            bsr     WaitClear           ; count the active area out
            move.w  d0,R_T1_ACTIVE
            clr.w   V_COUNTER
            bsr     WaitSet             ; and the next blank
            move.w  d0,R_T1_BLANK2

            bsr     PetDog
            move.w  #5,R_PHASE

; ===========================================================================
; Phase 6 -- T2: the VBLANK interrupt fires once a frame and is held until acked
;
; Two frames, differing only in whether the handler acks on its first entry.
; With an immediate ack the level drops and the count is 1. With the ack
; deferred by one entry the level is still asserted when RTE restores the mask,
; so the handler is re-entered at once and the count is 2. That pair is the
; assertion that 8A0001 is what clears the latch, and it needs both halves:
; either count alone is satisfiable by a board that behaves the other way.
; ===========================================================================
            bsr     WaitVblank          ; land in the blank this frame's IRQ4 set
            move.b  #0,VBLANK_ACK       ; ... and clear it, so the window is clean
            clr.w   V_IRQ4CNT
            clr.w   V_VBSEEN
            clr.w   V_ACKSKIP           ; ack on the first entry
            andi.w  #SR_MASKOFF,sr      ; unmask
            bsr     WaitVblank          ; next frame's edge: IRQ4 is raised here
            bsr     WaitSet             ; ride the blank out so the handler has run
            ori.w   #SR_MASKON,sr       ; mask
            move.w  V_IRQ4CNT,R_T2_COUNT
            move.w  V_VBSEEN,R_T2_VB
            bsr     PetDog

            bsr     WaitVblank
            move.b  #0,VBLANK_ACK
            clr.w   V_IRQ4CNT
            move.w  #1,V_ACKSKIP        ; pass the first entry without acking
            andi.w  #SR_MASKOFF,sr
            bsr     WaitVblank
            bsr     WaitSet
            ori.w   #SR_MASKON,sr
            move.w  V_IRQ4CNT,R_T2_HELD
            bsr     PetDog

            move.w  #6,R_PHASE

; ===========================================================================
; Phase 7 -- T3: the programmable scanline interrupt, at line 64, poll path
;
; Entry 0 of motion-object bank 0 becomes a timer targeting line 64, with its
; link pointing at entry 1 so the walk reaches it later. Entry 1 is left as a
; non-timer for now; phase 10 turns it into one mid-frame.
;
; INTERRUPTS STAY MASKED HERE, AND THAT IS NOT AN OVERSIGHT. IRQ3 is a level the
; board holds for one scanline and nothing acknowledges it: RTE lowers the mask
; while the line is still running, so the handler is re-entered before the
; interrupted instruction retires. With interrupts enabled the CPU spends the
; whole scanline inside exception entry and RTE, the polling loop gets zero
; iterations while the bit is high, and the poll path never observes the pulse
; it is meant to be measuring. That was found by running it. The two paths
; therefore get a frame each, and agreeing across those two frames is what makes
; them a check on one another; the timer entry is static, so the position is the
; same in both.
; ===========================================================================
            move.w  #TIMER_L1,MOB_E0_W0
            move.w  #TIMER_FLAG,MOB_E0_W1
            move.w  #0,MOB_E0_W2
            move.w  #1,MOB_E0_W3        ; link to entry 1
            move.w  #0,MOB_E1_W0
            move.w  #0,MOB_E1_W1        ; not a timer yet
            move.w  #0,MOB_E1_W2
            move.w  #1,MOB_E1_W3        ; self-link terminates the walk

            bsr     T3Frame
            move.w  d0,R_T3_POLL_A
            move.w  d1,R_T3_END_A

            move.w  #7,R_PHASE

; ===========================================================================
; Phase 8 -- T3 again, at line 160
;
; A single placement is satisfied by an interrupt that fires at a fixed line and
; ignores the list entirely, which is exactly the wrong thing to pin. The second
; placement is 96 lines away and the interrupt has to move with it.
; ===========================================================================
            move.w  #TIMER_L2,MOB_E0_W0
            bsr     T3Frame
            move.w  d0,R_T3_POLL_B

            move.w  #8,R_PHASE

; ===========================================================================
; Phase 9 -- T4: the same pulse down the interrupt path
;
; One frame with the mask down, spent counting iterations from the vblank edge
; all the way round to the next one, so the loop has an origin the handler can
; snapshot against. The timer is back at line 64, and the handler's first
; snapshot has to land on the position the poll path measured in phase 7.
;
; The entry count is recorded but only asserted as "more than one": with no ack
; and a handler far shorter than a scanline, re-entry is inevitable, and that
; contrast with IRQ4's count of exactly 1 is the level-versus-ack distinction
; seen from the interrupt side. Its exact value is a function of how long this
; handler happens to be, which is not a property of the hardware and is not
; something to pin.
; ===========================================================================
            move.w  #TIMER_L1,MOB_E0_W0

            lea     SWITCHES,a0
            move.w  #VB_MASK,d1
            bsr     WaitVblank
            move.b  #0,VBLANK_ACK       ; clear the latch this frame's edge set
            clr.w   V_IRQ3CNT
            clr.w   V_IRQ3FIRST
            clr.w   V_ACKSKIP           ; the IRQ4 handler acks immediately
            clr.w   V_COUNTER
            andi.w  #SR_MASKOFF,sr
            bsr     WaitSet             ; count the blank out
            bsr     WaitClear           ; ... and the active area, past line 64
            ori.w   #SR_MASKON,sr
            move.w  V_IRQ3CNT,R_T4_CNT
            move.w  V_IRQ3FIRST,R_T4_FIRST
            bsr     PetDog

            move.w  #9,R_PHASE

; ===========================================================================
; Phase 10 -- T5: the display list is read live, so a mid-frame edit takes
;             effect in the same frame
;
; timer_irq_at_scanline reads the live sprite RAM while the compositor renders
; from mo_shadow, a copy taken at the start of vblank. So a timer entry written
; part way down a frame changes the interrupt in that frame and the picture only
; in the next one.
;
; The interrupt half is measured here without any timing constant: entry 0's
; line-64 interrupt is itself the trigger for the write. When it arrives, entry 1
; is turned into a timer at line 160, and the second assertion has to appear 96
; lines later in the same frame.
; ===========================================================================
            move.w  #TIMER_L1,MOB_E0_W0 ; back to line 64
            move.w  #0,MOB_E1_W1        ; entry 1 not a timer at the frame's start

            lea     SWITCHES,a0
            move.w  #VB_MASK,d1
            bsr     WaitVblank
            move.b  #0,VBLANK_ACK

            lea     INT3STATE,a0
            move.w  #INT3_MASK,d1
            clr.w   V_COUNTER
            bsr     WaitSet             ; the line-64 pulse
            move.w  #TIMER_L2,MOB_E1_W0 ; ... and now, mid-frame, add line 160
            move.w  #TIMER_FLAG,MOB_E1_W1
            bsr     WaitClear           ; ride out the line-64 pulse
            bsr     WaitSet             ; the line-160 pulse, this same frame
            move.w  d0,R_T5_POLL
            bsr     PetDog

            move.w  #10,R_PHASE

; ===========================================================================
; Phase 11 -- draw a picture, and hold it long enough to be captured
;
; All three layers, so the capture exercises the whole compositor rather than
; one corner of it: a playfield of solid pen 1, a line of text through the alpha
; layer with its pen 0 left transparent so the background shows through the
; glyphs, and one motion object whose four tiles step codes 1 to 4 down its
; length. Every colour is written to palette RAM here, so nothing about the
; picture depends on what the board powered up holding.
;
; Two vblanks before the phase is published. The compositor renders motion
; objects from mo_shadow, which is copied at the start of vblank, so a sprite
; written during a frame is not in the picture until the next one.
; ===========================================================================
            move.w  #C_BLACK,PAL_PF0
            move.w  #C_RED,PAL_PF1
            move.w  #C_GREEN,PAL_PF2
            move.w  #C_WHITE,PAL_MO1
            move.w  #C_BLUE,PAL_MO2
            move.w  #C_GREEN,PAL_MO3
            move.w  #C_WHITE,PAL_MO4
            move.w  #C_WHITE,PAL_AL1
            move.w  #C_BLUE,PAL_AL2

            move.w  #0,XSCROLL
            move.w  #0,YSCROLL
            move.w  #0,PRIORITY         ; no playfield pen draws in front of a sprite

            lea     PF,a0               ; playfield: every cell solid pen 1
            move.w  #(64*64)-1,d0
PfFill
            move.w  #PF_TILE1,(a0)+
            dbra    d0,PfFill

            lea     ALPHA,a0            ; alpha: clear to code 0, which the
            move.w  #(64*32)-1,d0       ; synthetic font leaves fully transparent
AlFill
            move.w  #0,(a0)+
            dbra    d0,AlFill
            bsr     PetDog

            lea     TextData,a0
            lea     TEXTCELL,a1
TxLoop
            move.w  (a0)+,d0
            cmpi.w  #TIMER_FLAG,d0
            beq.s   TxDone
            move.w  d0,(a1)+
            bra.s   TxLoop
TxDone

            move.w  #SPR_W0,MOB_E1_W0   ; entry 1 becomes the sprite
            move.w  #SPR_W1,MOB_E1_W1
            move.w  #SPR_W2,MOB_E1_W2
            move.w  #1,MOB_E1_W3
            move.w  #TIMER_L3,MOB_E0_W0 ; entry 0 stays the timer, now at line 120
            move.w  #TIMER_FLAG,MOB_E0_W1
            move.w  #0,MOB_E0_W2
            move.w  #1,MOB_E0_W3

            bsr     PetDog
            bsr     WaitVblank
            bsr     PetDog
            bsr     WaitVblank          ; mo_shadow now holds the sprite
            move.w  #11,R_PHASE

; ===========================================================================
; Phase 12 -- T6: change a palette entry part way down the frame
;
; The whole playfield is pen 1 and pen 1 is red. At the scanline the timer names
; -- named by the interrupt, not by counting cycles -- pen 1 becomes green. On
; hardware the rows the beam has already drawn stay red and the rest come out
; green, one transition, at the line the interrupt fired on.
;
; THIS BOARD RENDERS THE WHOLE FRAME AT THE FRAME BOUNDARY, so the palette is
; read once, after the write, and the entire picture comes out green. The
; assertion in the harness is held as a known defect against
; phosphor-emulator-raster-sampling-6kae.3 and is expected to fail the day that
; lands. See the harness and the design doc; do not "fix" it by deleting it.
; ===========================================================================
            lea     INT3STATE,a0
            move.w  #INT3_MASK,d1
            bsr     WaitSet             ; line 120 of the frame now starting
            move.w  #C_GREEN,PAL_PF1
            bsr     PetDog
            bsr     WaitVblank          ; ride out the frame the write is in
            move.w  #12,R_PHASE

; ===========================================================================
; Phase 13 -- T7: write playfield cells above and below the beam
;
; The Joust bug class restated for a tilemap. Pen 1 is red again before the
; frame's first line. At line 120 two cells become pen 2, which is green: one at
; screen rows 48-55, drawn long ago, and one at 200-207, not yet reached. On
; hardware only the lower one changes in this frame and both have changed by the
; next.
;
; Rendered whole-frame, both change at once. Also held as a known defect.
; ===========================================================================
; A WHOLE FRAME BEFORE THE PALETTE IS PUT BACK, and it has to be. Phase 12 is
; published at the vblank edge, which is still inside the frame the harness is
; running and will capture when it returns; anything written between there and
; the frame boundary lands in that capture. Restoring pen 1 immediately put the
; red back before T6's frame was ever rendered, and T6 read a screen with neither
; colour where it wanted green. So this rides out that frame first and restores
; in the blank of the next one, clear of the capture and clear of the frame T7
; goes on to measure.
            bsr     PetDog
            bsr     WaitVblank
            move.w  #C_RED,PAL_PF1      ; in the blank, before T7's frame starts
            lea     INT3STATE,a0
            move.w  #INT3_MASK,d1
            bsr     WaitSet             ; line 120 again
            move.w  #PF_TILE2,PF_ABOVE
            move.w  #PF_TILE2,PF_BELOW
            bsr     PetDog
            bsr     WaitVblank
            move.w  #13,R_PHASE

; ===========================================================================
; Phase 14 -- the frame after, with no writes at all
;
; Both cells must be green here whichever rendering model is in force, so this
; is the capture that says the writes landed rather than that they landed late.
; ===========================================================================
            bsr     PetDog
            bsr     WaitVblank
            move.w  #14,R_PHASE

; ===========================================================================
; Phase 15 -- the compositor paths the first picture missed
;
; Every phase from here on builds its picture, then rides a whole frame before
; publishing. The harness reads the phase once per run_frame, so a phase is only
; observable if it is the last one written in its frame, and anything written
; between publishing and the frame boundary lands in the capture that phase just
; asked for. Both traps have been paid for once already, in the Williams ROM's
; lost third capture and in T6's palette restore.
;
; 78lx drew one path through each layer. This draws the rest of them, in bands
; that do not overlap so one capture covers all of it:
;
;   y 40-47   mirroring, motion objects. Tile 5 is pen 1 on its left half and
;             pen 2 on its right, so a mirrored copy beside an unmirrored one
;             swaps its halves. Solid tiles cannot show this at all, which is
;             why tile 5 exists.
;   y 60-67   the priority merge. A HIGH-priority sprite does not draw its own
;             colour: it blends through the translucent bank at
;             $300 + (playfield pen << 4) + sprite pen, so a pen-2 sprite over
;             the pen-1 background comes out $312. Beside it, a high-priority
;             sprite whose pen is 1 draws NOTHING at all -- the one pen the
;             merge excludes -- and the playfield shows through.
;   y 80-87   the other side of priority. Both sprites are low priority and
;             identical; the left one sits over the pen-1 background and draws,
;             the right one sits over a pen-2 cell and loses, because 840000 bit
;             2 says the playfield stands in front of pen 2. Same sprite, two
;             backgrounds, opposite outcomes.
;   y 160-167 mirroring, playfield. Two adjacent cells of tile 5, one with the
;             cell's own mirror bit set.
;   y 120-127 no vertical flip on a motion object. Tile 6 is pen 1 over pen 2,
;             so a vertically mirrored copy would be visibly upside down. Two
;             sprites of it, one with every bit word0 and word2 do not decode
;             set, must be identical.
;   y 176-183 the alpha layer's force-opaque bit and colour field. Code 0 is the
;             blank glyph, so every pen is 0 and nothing would draw; bit 13
;             forces the cell opaque anyway and colour 1 makes it palette entry
;             colour*4 + 0 = 4.
;   y 192-199 no flip of any kind on the alpha layer. The same asymmetric glyph
;             twice, once with both spare bits of the cell word set.
; ===========================================================================
            move.w  #C_YELLOW,PAL_TRANS
            move.w  #C_MAGENTA,PAL_ALO
            move.w  #PRIO_PENS,PRIORITY

            move.w  #PF_TILE2,PF_PRIOCELL
            move.w  #PF_TILE5,PF_MIRROR
            move.w  #PF_FLIP5,PF_MIRRORF
            move.w  #AL_OPAQVAL,AL_OPAQUE
            move.w  #AL_GLYPH,AL_PLAIN
            move.w  #AL_GLYPH+AL_SPAREBITS,AL_SPARE

            lea     SprData,a0
SprLoop
            move.w  (a0)+,d0            ; entry number, or the terminator
            cmpi.w  #TIMER_FLAG,d0
            beq.s   SprDone
            add.w   d0,d0               ; entry N's words are at N*2 within each
            lea     MOB,a1              ; of the four $80-byte planes
            adda.w  d0,a1
            move.w  (a0)+,(a1)
            move.w  (a0)+,$80(a1)
            move.w  (a0)+,$100(a1)
            move.w  (a0)+,$180(a1)
            bra.s   SprLoop
SprDone

            bsr     PetDog
            bsr     WaitVblank
            bsr     PetDog
            bsr     WaitVblank          ; mo_shadow now holds the new sprites
            move.w  #15,R_PHASE

; ===========================================================================
; Phase 16 -- scroll the playfield out from under everything else
;
; Both scrolls to 8, so the playfield moves up and left by 8 and the alpha and
; motion-object layers do not move at all. The write rides out a frame first: a
; scroll written at the vblank edge would land in the capture phase 15 has just
; asked for, the same trap the T6 palette restore fell into.
; ===========================================================================
            bsr     PetDog
            bsr     WaitVblank          ; clear of the phase-15 capture
            move.w  #SCROLL_BY,XSCROLL
            move.w  #SCROLL_BY,YSCROLL
            bsr     PetDog
            bsr     WaitVblank
            move.w  #16,R_PHASE

; ===========================================================================
; Done
; ===========================================================================
            bsr     PetDog
            bsr     WaitVblank
            move.w  #17,R_PHASE
            move.w  #MAGIC,R_MAGIC
Spin
            bsr     WaitVblank
            bsr     PetDog
            bra     Spin

; ===========================================================================
; Data
; ===========================================================================

; "ROAD RUNNER" as alpha cell words: code in bits 0-9, colour in 12-10, the
; force-opaque flag in 13. Colour 0 and the flag clear, so pen 0 of each glyph
; stays transparent and the playfield shows through the letters. The glyph codes
; are the font the harness builds; code 0 is blank, which is also what the alpha
; layer is cleared to.
TextData
            dc.w    1,2,3,4,0,1,5,6,6,7,1
            dc.w    TIMER_FLAG          ; $FFFF terminator

; The motion-object list phase 15 installs: entry number, then its four words.
; Entry 0 is left alone, still the scanline timer, and still linking to 1. The
; chain runs 1 to 7 and entry 7 links to itself, which is what ends the walk.
SprData
            dc.w    1, SPR_W0, SPR_W1, SPR_W2, 2                    ; the 4-tile sprite
            dc.w    2, SPR_Y40, 5, SPR_X40, 3                       ; tile 5
            dc.w    3, SPR_Y40+SPR_MIRROR, 5, SPR_X56, 4            ; ... mirrored
            dc.w    4, SPR_Y60, 2, SPR_X40+SPR_HIPRIO, 5            ; translucent
            dc.w    5, SPR_Y60, 1, SPR_X56+SPR_HIPRIO, 6            ; pen 1: draws nothing
            dc.w    6, SPR_Y80, 1, SPR_X40, 7                       ; over pen 1: draws
            dc.w    7, SPR_Y80, 1, SPR_X56, 8                       ; over pen 2: loses
            dc.w    8, SPR_Y120, 6, SPR_X40, 9                      ; tile 6, plain
            dc.w    9, SPR_Y120+SPR_SPARE0, 6, SPR_X56+SPR_SPARE2, 9 ; ... spare bits set
            dc.w    TIMER_FLAG

; ===========================================================================
; Helpers
; ===========================================================================

; One IRQ3 poll-path measurement frame, with the timer entries already in place.
; Returns the loop count at the interrupt's assertion in d0 and at its release in
; d1, both on one origin taken at the vblank edge, so d1 - d0 is the pulse width
; on the same scale as everything else.
;
; Interrupts stay masked throughout: see phase 7 for why polling and taking IRQ3
; in the same frame cannot both work.
T3Frame
            lea     SWITCHES,a0
            move.w  #VB_MASK,d1
            bsr     WaitVblank

            lea     INT3STATE,a0
            move.w  #INT3_MASK,d1
            clr.w   V_COUNTER
            bsr     WaitSet
            move.w  d0,d2               ; assertion
            bsr     WaitClear           ; keep the same origin: no clear here
            move.w  d0,d3               ; release
            bsr     PetDog
            move.w  d2,d0
            move.w  d3,d1
            rts

; Return on the frame's transition into vertical blank, i.e. at scanline 240.
; F60000 bit 4 is the live VBLANK line and is ACTIVE LOW, so "out of blank" is
; the bit set. Waiting for the high state first makes this an edge rather than a
; level, so two calls in a row cannot both return inside the same blank.
;
; run_frame runs a whole frame from scanline 0, so scanline 240 is inside the
; frame the harness is currently running and anything published on return is
; observable at the end of that same run_frame().
WaitVblank
            move.l  d0,-(a7)
            move.w  #WAITLIMIT,V_LIMIT
WVOut
            subq.w  #1,V_LIMIT
            beq     WaitGaveUp
            move.w  SWITCHES,d0
            andi.w  #VB_MASK,d0
            beq.s   WVOut               ; still in blank, wait for active display
            move.w  #WAITLIMIT,V_LIMIT
WVIn
            subq.w  #1,V_LIMIT
            beq     WaitGaveUp
            move.w  SWITCHES,d0
            andi.w  #VB_MASK,d0
            bne.s   WVIn                ; wait for the edge into blank
            move.l  (a7)+,d0
            rts

; THE SHARED MEASUREMENT LOOP. Count iterations in V_COUNTER until the word at
; (a0) masked by d1 becomes non-zero, and return the count in d0. WaitClear is
; the same loop with the branch inverted, so the two cost the same and their
; counts are on one scale.
;
; The count lives in memory rather than a register so an interrupt handler can
; snapshot the caller's position without the caller having to hand it over. The
; caller clears V_COUNTER to set an origin; neither routine clears it, so a
; WaitSet followed by a WaitClear measures a span from one origin.
WaitSet
            move.w  #WAITLIMIT,V_LIMIT
WSLoop
            subq.w  #1,V_LIMIT
            beq.s   WaitGaveUp
            addq.w  #1,V_COUNTER
            move.w  (a0),d0
            and.w   d1,d0
            beq.s   WSLoop
            move.w  V_COUNTER,d0
            rts

WaitClear
            move.w  #WAITLIMIT,V_LIMIT
WCLoop
            subq.w  #1,V_LIMIT
            beq.s   WaitGaveUp
            addq.w  #1,V_COUNTER
            move.w  (a0),d0
            and.w   d1,d0
            bne.s   WCLoop
            move.w  V_COUNTER,d0
            rts

; A wait that ran out of patience. Records the fact and stops, holding the
; machine up with the watchdog so the result block survives for the harness to
; read: R_PHASE already says which stage was waiting and for what. Deliberately
; does not return, and does not let the watchdog reboot the machine, because a
; reboot would clear the block and the evidence with it.
WaitGaveUp
            ori.w   #SR_MASKON,sr
            move.w  #TRAPPED,R_TIMEOUT
GaveUpSpin
            bsr     PetDog
            bra     GaveUpSpin

; Any write to 880001 clears the watchdog counter. A byte write there becomes a
; read-modify-write of the word at 880000, which the board does not decode and
; which returns $FFFF with no side effect, so the byte form is safe and is what
; the hardware expects at an odd-addressed register.
PetDog
            move.b  #0,WATCHDOG
            rts

; ===========================================================================
; Interrupt handlers
; ===========================================================================

; Level 4, VBLANK. Counts entries, notes whether the VBLANK line was actually
; asserted when it ran, and acks at 8A0001 unless V_ACKSKIP says to pass this
; one. Deferring the ack is what demonstrates that the latch is held rather than
; edge-triggered: the handler is re-entered the instant RTE lowers the mask.
Irq4Handler
            move.l  d0,-(a7)
            addq.w  #1,V_IRQ4CNT
            move.w  SWITCHES,d0
            andi.w  #VB_MASK,d0
            bne.s   I4NotBlank
            move.w  #1,V_VBSEEN
I4NotBlank
            tst.w   V_ACKSKIP
            beq.s   I4Ack
            subq.w  #1,V_ACKSKIP
            bra.s   I4Done
I4Ack
            move.b  #0,VBLANK_ACK
I4Done
            move.l  (a7)+,d0
            rte

; Level 3, the motion-object scanline interrupt. Nothing acks it: the board holds
; it for the one scanline the timer entry targets and drops it at the next line
; boundary, so RTE lowers the mask into a still-asserted level and this is
; re-entered without the interrupted loop advancing at all. The first entry's
; position is therefore the only one worth recording; every later one is at the
; same count, because the loop that maintains that count never ran in between.
Irq3Handler
            move.l  d0,-(a7)
            tst.w   V_IRQ3CNT
            bne.s   I3NotFirst
            move.w  V_COUNTER,d0
            move.w  d0,V_IRQ3FIRST
I3NotFirst
            addq.w  #1,V_IRQ3CNT
            cmpi.w  #IRQ3CAP,V_IRQ3CNT
            bcc     Irq3Overrun         ; unsigned >=; never returns
            move.l  (a7)+,d0
            rte

; IRQ3 is asserted far past the one scanline it is supposed to last, and the
; program is now living inside its own interrupt handler. Leave the exception
; frame where it is, mask, say so, and park: an RTE here would only come
; straight back.
Irq3Overrun
            ori.w   #SR_MASKON,sr
            move.w  #IRQ3STORM,R_TIMEOUT
            bra     GaveUpSpin

; ===========================================================================
; Stray exception handler
;
; Records a marker and the 68010 format $0 frame's vector-offset word (vector x
; 4) at 6(sp), which names the vector, then holds the machine up by strobing the
; watchdog so the harness can read the marker out. It deliberately does NOT
; return: a program that took an exception it did not arrange has already lost,
; and the useful artifact is the marker rather than the recovery.
;
; A group-0 fault (bus or address error) pushes the larger frame, so 6(sp) is
; not the vector-offset word there; R_TRAP is still set and R_TRAPV is still a
; clue rather than a lie about which vector it names.
; ===========================================================================
StrayException
            move.w  #TRAPPED,R_TRAP
            move.w  6(a7),R_TRAPV
StraySpin
            bsr     PetDog
            bra     StraySpin
