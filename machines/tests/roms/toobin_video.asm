; ---------------------------------------------------------------------------
; Toobin' (Atari SP-320) video conformance ROM
;
; Design: docs/designs/toobin-video-conformance.md
; Assemble:
;   asl -q -o toobin_video.p toobin_video.asm
;   p2bin toobin_video.p toobin_video.bin -r 0x0000-0x1FFF -l 0xA5
;
; Both tools are in the Nix dev shell. p2bin's -r fixes the image at exactly
; 8 KB and -l fills the gaps between the vector table, the code and the end of
; the window, which is what makes the load a flat copy and the checksum below a
; fixed quantity.
;
; THE FILL BYTE IS 0xA5 AND NOT ZERO, for the reason the Road Runner ROM
; records: a ROM-less board's program ROM is already all zeroes, so a zero fill
; makes the checksum blind over the padding and a truncated load sums the same
; as a complete one. $A5A5 also decodes as line-A, which vectors to the stray
; handler, so a runaway program counter records itself.
;
; Loaded by machines/tests/toobin_video_timing_test.rs into a ROM-less machine,
; by poking the image through BusDebug::write (AddressSpace32's debug_write
; ignores AccessKind, so the ReadOnly program-ROM region takes the write). The
; 68010 fetches its stack pointer from 0 and its program counter from 4 through
; the bus, so both come from this image.
;
; EVERY WAIT IS A POLL OF HARDWARE STATE, NEVER A DELAY LOOP, and every position
; is measured in iterations of one shared poll loop whose rate is calibrated in
; the same run. T1 counts iterations across the 384 active lines, which gives
; iterations-per-line, and every later figure divides by that. So a constant
; cycle offset between two implementations cancels and nothing here is compared
; against a number that was measured once and written down.
;
; WHAT THIS BOARD HAS THAT ROAD RUNNER DID NOT, and it is the reason Toobin' is
; worth a third conformance ROM rather than a second one: a LIVE HORIZONTAL
; BLANK LEVEL in the status port. Williams had a scanline counter, Road Runner
; had a vertical level and a programmable interrupt, and this board has a
; vertical level, a programmable interrupt AND a horizontal one. The horizontal
; level is what lets T2 measure the poll loop against a scanline directly
; instead of only against a frame, so the calibration T1 establishes has an
; independent check inside the same run.
; ---------------------------------------------------------------------------

            cpu     68010

; --- Hardware ---------------------------------------------------------------
;
; Addresses are the board's own, before the decoder folds A21-A19 away. The
; register block sits at FF8000 with A0-A5 undecoded, so each register answers
; across a 64-byte span and the names below are the canonical address of each.

PF          equ $C00000         ; playfield RAM, 128 x 64 cells of TWO words
ALPHA       equ $C08000         ; alpha RAM, 64 x 48 cells of one word
MOB         equ $C09800         ; motion-object RAM, 256 entries of four words
PAL         equ $C10000         ; palette RAM, 1024 entries of one word

WATCHDOG    equ $FF8000         ; any write clears the counter
SNDCMD      equ $FF8100
INTENSITY   equ $FF8300         ; global intensity, 5 bits
INTSCAN     equ $FF8340         ; scanline the interrupt fires at, 9 bits
SLIP        equ $FF8380
SCANACK     equ $FF83C0         ; write clears the scanline interrupt latch
SNDRESET    equ $FF8400         ; strobe: reboots the sound board
EEPROMEN    equ $FF8500
XSCROLL     equ $FF8600
YSCROLL     equ $FF8700
SWITCHES    equ $FF8800
STATUS      equ $FF9000
SNDRESP     equ $FF9800         ; reading it clears the sound response flag

; The status word, all ACTIVE LOW.
HB_MASK     equ $8000           ; horizontal blank
VB_MASK     equ $4000           ; vertical blank
SC_MASK     equ $2000           ; sound command pending
SV_MASK     equ $1000           ; operator self-test

SR_MASKON   equ $0700           ; SR interrupt mask bits, all set (level 7)

; --- Image ------------------------------------------------------------------

IMGBASE     equ $000000
IMGWORDS    equ $1000           ; 8 KB as words, the p2bin -r window

; --- Storage ----------------------------------------------------------------
;
; Work RAM is 16 KB at FFC000. The result block, the variables and the stack all
; live here with kilobytes to spare between them.

STACKTOP    equ $FFDF00

; --- Result block (FFC000, work RAM) ----------------------------------------
;
; Words rather than bytes. A byte write on this bus is a read-modify-write of
; the containing word, and there is no shortage of work RAM, so there is no
; reason to pack.

RES         equ $FFC000
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

R_T2_HB     equ RES+22          ; iterations HBLANK is asserted, over HB_LINES
R_T2_HTOT   equ RES+24          ; ... and the whole-line total over the same span
R_T2_LINES  equ RES+26          ; how many lines that was measured over

R_T3_LINE   equ RES+28          ; the line T3 programmed
R_T3_POS    equ RES+30          ; loop count at the first IRQ1 entry
R_T3_CNT    equ RES+32          ; IRQ1 entries in that frame

R_T4_LINE   equ RES+34          ; the line T4 programmed, further down the frame
R_T4_POS    equ RES+36
R_T4_CNT    equ RES+38

R_SND       equ RES+40          ; level-2 or level-3 entries: the sound board
                                ; asserting when this program expects silence
R_TIMEOUT   equ RES+42          ; $DEAD if a wait gave up; R_PHASE says where

RESLEN      equ 48

MAGIC       equ $5A5A
TRAPPED     equ $DEAD
IRQSTORM    equ $DEA1

; How many IRQ1 entries in one measurement are too many to be a latch the
; handler acks. A bound rather than an expectation: nothing asserts against it,
; it only stops the program from disappearing into an interrupt storm. Only the
; handler can bound the handler, because during a storm RTE drops straight back
; in and the polling loop never runs an iteration to expire a count.
IRQ1CAP     equ 64

; Vblank edges to survive before declaring the watchdog fed. Deliberately double
; the board's 8-frame timeout: the program cannot reach this count unless every
; strobe landed, because a reboot clears the result block and starts over.
VB_TARGET   equ 16

; Scanlines T3 and T4 aim the interrupt at. Both are inside active display and
; well apart, so the difference between their two positions is a second reading
; of iterations-per-line that does not share T1's endpoints.
T3_LINE     equ 100
T4_LINE     equ 300

; Lines T2 samples the horizontal blank over. Enough that one sample of jitter
; at each end is small against the total, few enough to sit inside one frame's
; active display with room to spare.
HB_LINES    equ 64

; T2's down-counter, which is its bound as well as its clock. At about 22 cycles
; an iteration this is a little over two frames, so a wait that never sees its
; edge gives up well before the 8-frame watchdog reboots the machine and clears
; the evidence, while leaving an honest measurement (~900 iterations) a wide
; margin.
T2BOUND     equ $3FFF

; --- Variables (FFC100, work RAM, above the result block) -------------------
;
; Everything the interrupt handlers touch lives here rather than in registers,
; so a handler cannot disturb the loop it interrupted.

VARS        equ $FFC100
V_COUNTER   equ VARS+0          ; the shared poll loop's iteration count
V_IRQ1CNT   equ VARS+2
V_IRQ1FIRST equ VARS+4
V_LIMIT     equ VARS+6          ; iterations a wait has left before giving up
V_SNDCNT    equ VARS+8
VARLEN      equ 16

; EVERY WAIT IS BOUNDED. A wait that spins forever makes the watchdog reboot the
; machine, which clears the result block, and the harness then reports a wedge
; at whatever phase the restarted run happened to be in rather than at the stage
; that broke. The bound has to clear the longest honest wait here (a little over
; one frame) and expire well before the 8-frame reboot destroys the evidence.
WAITLIMIT   equ 20000

; ===========================================================================
; Exception vectors
;
; Vector 0 is the supervisor stack pointer and vector 1 is the entry point;
; cpu.reset fetches both through the bus, so they are the load-bearing halves of
; this image. EVERY OTHER VECTOR POINTS AT A HANDLER rather than at zero, so a
; mistake records itself instead of executing whatever happens to be at 0.
;
; Autovector N is 24 + N. This board wires the scanline interrupt to level 1 and
; the sound response to level 2, and asserts level 3 when both are up at once,
; so vectors 25, 26 and 27 all get handlers. Levels 2 and 3 are not expected to
; fire at all: the sound board is held in reset and its response latch drained
; at entry. They are handled rather than left stray so that the program records
; the fact and carries on, because a bare board's sound 6502 has no ROM and what
; it does with an empty memory map is not something this ROM should assert.
; ===========================================================================
            org     IMGBASE

            dc.l    STACKTOP            ;  0: reset SSP
            dc.l    Reset               ;  1: reset PC
            rept    23
            dc.l    StrayException      ;  2-24
            endm
            dc.l    Irq1Handler         ; 25: autovector level 1, scanline
            dc.l    Irq2Handler         ; 26: autovector level 2, sound response
            dc.l    Irq3Handler         ; 27: autovector level 3, both at once
            rept    228
            dc.l    StrayException      ; 28-255
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

; PARK THE SCANLINE INTERRUPT AND ACK IT BEFORE ANYTHING UNMASKS.
;
; interrupt_scan comes out of reset at 0, so the latch is set at scanline 0 of
; every frame from power-on and is cleared only by a write to SCANACK. A program
; that lowered its mask without doing this would take an IRQ1 it never asked for,
; at a line it did not choose. Park it on the last blanked line, which no
; measurement here uses, and clear the latch that is already standing.
            move.w  #415,INTSCAN
            move.w  #0,SCANACK

; Quiet the sound board and drain anything its response latch is holding. On a
; bare board the sound 6502 has no ROM at all, so what it executes is whatever an
; empty map returns; it must not be able to raise level 2 into the middle of a
; measurement. The reset here is a strobe rather than a level, so one write is
; the whole of it.
            move.w  #0,SNDRESET
            tst.w   SNDRESP             ; clear a response the latch still holds

; A zero result block must never read as a pass, so clear it deliberately.
; Clearing it is also what makes a watchdog reboot visible: reset() does not
; clear work RAM, so without this a reboot would leave a plausible half-finished
; block behind instead of restarting the counts.
            lea     RES,a0
            moveq   #(RESLEN/2)-1,d0
ClrRes
            move.w  #0,(a0)+
            dbra    d0,ClrRes

            lea     VARS,a0
            moveq   #(VARLEN/2)-1,d0
ClrVars
            move.w  #0,(a0)+
            dbra    d0,ClrVars

; CLEAR THE VIDEO RAM THIS PROGRAM DOES NOT OTHERWISE WRITE, because it cannot
; assume the machine was cold when it took over. Under MAME it will not be: the
; game runs from power-on until the script patches the image and soft-resets, so
; the palette and the object list arrive holding the attract mode's contents.
            lea     PAL,a0
            move.w  #1024-1,d0
ClrPal
            move.w  #0,(a0)+
            dbra    d0,ClrPal

            lea     MOB,a0
            move.w  #1024-1,d0
ClrMob
            move.w  #0,(a0)+
            dbra    d0,ClrMob
            bsr     PetDog

; Full intensity, so nothing downstream reads as a palette fault when it is the
; global dimmer. Scroll to a known origin for the same reason.
            move.w  #31,INTENSITY
            move.w  #0,XSCROLL
            move.w  #0,YSCROLL

            move.l  d7,R_SSP
            move.w  #1,R_PHASE

; ===========================================================================
; Phase 2 -- checksum the whole image back through the real bus
;
; The poke went in through the debug bus; this reads all 8 KB out through the
; bus the CPU actually drives, which is what makes "the loader worked" a
; measurement rather than an inference.
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
; The watchdog is the thing that breaks a program on this board first: the
; machine reboots after 8 vertical blanks with no write to FF8000. Riding twice
; that many is the assertion that the strobe lands.
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
; STATUS bit 14 is the live vertical blank and is active low. The board blanks
; from scanline 384 to 415 of 416, so the expectation is 32 blanked lines
; against 384 active ones, and R_T1_ACTIVE over 384 DEFINES iterations-per-line
; for every figure below.
;
; Measured as three consecutive dwells rather than two, because a ratio that is
; right once can be right by accident; the second blank shows the first was not.
; ===========================================================================
            lea     STATUS,a0
            move.w  #VB_MASK,d1

            bsr     WaitVblank          ; return on the edge INTO blank
            clr.w   V_COUNTER
            bsr     WaitSet             ; ... until the bit goes high: end of blank
            move.w  d0,R_T1_BLANK
            clr.w   V_COUNTER
            bsr     WaitClear           ; ... and across the whole active display
            move.w  d0,R_T1_ACTIVE
            clr.w   V_COUNTER
            bsr     WaitSet             ; the same blank one frame later
            move.w  d0,R_T1_BLANK2
            bsr     PetDog

            move.w  #5,R_PHASE

; ===========================================================================
; Phase 6 -- T2: the horizontal blank, which Road Runner had no way to see
;
; STATUS bit 15 is the live horizontal blank, active low. Counting iterations
; with the bit asserted against iterations over the same span gives the blank's
; share of a line, and the span divided by HB_LINES gives iterations-per-line a
; second time, from endpoints T1 does not share.
;
; The raster is 640 dots by 416 lines with 512 visible, so the expectation is
; 128 of 640 dots blanked: exactly a fifth of every line.
;
; Sampled inside active display on purpose. The horizontal blank runs during the
; vertical one too, so measuring across a frame boundary would mix a line's
; structure with a frame's and neither figure would mean anything.
; ===========================================================================
; THIS PHASE HAS ITS OWN LOOP, AND IT IS THE TIGHTEST ONE IN THE FILE. That is
; not an optimization, it is what makes the measurement exist at all, and it
; took two tries to get there.
;
; A horizontal blank is 64 of a line's 320 CPU cycles. Built from WaitSet and
; WaitClear like every other phase, the bsr and rts around each wait cost more
; time than the thing being measured: it read 3.0 iterations per line where T1
; read 5.3, and the missing 2.3 was call overhead elapsing without the counter
; advancing. Inlined but still counting into memory and checking V_LIMIT, the
; loop ran at 4.375 iterations per line, and 4.375 is 35/8: the sampling phase
; repeated exactly every 8 lines instead of drifting, and the blank read exactly
; ONE sample on all 64 lines. A share of 1/4.375 = 0.229 then looks like a
; plausible answer for a blank that is really 0.200 of a line, and it is not a
; measurement at all: it is the loop period, and it would have read the same on
; a board whose blank was half as wide.
;
; So the inner loops are four instructions, counting into a register rather than
; memory, with `dbne`/`dbeq` doing the wait and the bound together. That is
; about 24 cycles an iteration, so a line is ~13 samples and a blank ~3, which
; resolves the blank rather than landing on its floor.
;
; THE SAMPLE COUNT IS EXPLICIT AND THE DBcc COUNTER IS NOT USED AS ONE, which is
; the third thing this phase got wrong. `DBcc` does not decrement on the
; iteration where its condition comes true, so the register skips exactly the
; sample that detected each edge: two per line, one at each end of the blank.
; Read off the register the blank came out at 15.6% of the line against 20%, and
; every part of that deficit was the two uncounted samples rather than anything
; the board did. Counting at the top of each loop instead makes the detecting
; sample count, and then the +1 at the blank's start and the -1 at its end
; cancel exactly: the difference of two snapshots IS the number of samples that
; saw the blank, with no correction anywhere.
;
; THE TWO INNER LOOPS ARE DELIBERATELY IDENTICAL, instruction for instruction,
; and the blank's width is the difference of two snapshots of the shared counter
; taken outside them. A second counter inside the blank loop alone would make
; the blank read systematically wide by its own cost.
;
; Nothing here is on T1's scale and nothing compares the two. This loop counts
; at its own rate; what it can say is a RATIO, where both halves come from the
; same loop and the rate cancels.
            bsr     WaitVblank
            bsr     WaitActive          ; start of active display, a known line
            lea     STATUS,a0
            move.w  #HB_MASK,d1
            move.w  #T2BOUND,d3         ; the bound, and only the bound
            moveq   #0,d6               ; summed blank width
            move.w  #HB_LINES,d5
T2Align
            move.w  (a0),d0
            and.w   d1,d0
            dbne    d3,T2Align          ; out of blank: a known point in a line
            moveq   #0,d2               ; samples taken, counted explicitly
T2Active
            addq.w  #1,d2
            move.w  (a0),d0
            and.w   d1,d0
            dbeq    d3,T2Active         ; ... until this line's blank starts
            move.w  d2,d4
T2Blank
            addq.w  #1,d2
            move.w  (a0),d0
            and.w   d1,d0
            dbne    d3,T2Blank          ; ... and until it ends again
            move.w  d2,d7
            sub.w   d4,d7
            add.w   d7,d6
            subq.w  #1,d5
            bne.s   T2Active
            move.w  d2,R_T2_HTOT
            move.w  d6,R_T2_HB
            move.w  #HB_LINES,R_T2_LINES
; The bound doubles as the give-up check. If the counter ran out, one of the
; waits never saw its edge and every figure above is the bound rather than a
; measurement.
            cmpi.w  #16,d3
            blt     WaitGaveUp
            bsr     PetDog

            move.w  #6,R_PHASE

; ===========================================================================
; Phase 7 -- T3: the scanline interrupt, at a line the program picks
;
; Nothing in the status port reports this interrupt, so unlike Road Runner's it
; can only be observed by taking it. The handler snapshots the shared loop's
; counter, so the position is on the same scale as everything above.
; ===========================================================================
            move.w  #T3_LINE,d3
            bsr     ScanlineTest
            move.w  #T3_LINE,R_T3_LINE
            move.w  V_IRQ1FIRST,R_T3_POS
            move.w  V_IRQ1CNT,R_T3_CNT

            move.w  #7,R_PHASE

; ===========================================================================
; Phase 8 -- T4: the same, 200 lines further down
;
; The DIFFERENCE between this position and T3's is the assertion that matters:
; it is 200 lines measured with endpoints that share neither the vblank edge nor
; each other, so it cannot be right by a calibration error that moved both.
; ===========================================================================
            move.w  #T4_LINE,d3
            bsr     ScanlineTest
            move.w  #T4_LINE,R_T4_LINE
            move.w  V_IRQ1FIRST,R_T4_POS
            move.w  V_IRQ1CNT,R_T4_CNT

            move.w  #8,R_PHASE

; ===========================================================================
; Done
; ===========================================================================
            move.w  V_SNDCNT,R_SND
            bsr     PetDog
            move.w  #9,R_PHASE
            move.w  #MAGIC,R_MAGIC

Idle
            bsr     WaitVblank
            bsr     PetDog
            bra     Idle

; ===========================================================================
; Subroutines
; ===========================================================================

; Arm the scanline interrupt at the line in d3, then poll from the vblank edge
; until the handler has fired once. Leaves V_IRQ1FIRST and V_IRQ1CNT for the
; caller to publish.
;
; THE ACKNOWLEDGE GOES AFTER THE VBLANK WAIT AND NOT BEFORE IT, which is the one
; ordering that works and cost a run to find. Arming the line and then waiting a
; whole frame for the origin means the beam crosses the target line DURING the
; wait, so the latch is already standing by the time the mask comes down: the
; handler runs before the polling loop has completed one iteration and records
; position zero. Every scanline position read exactly 0.00 lines, which looked
; like the interrupt never arriving rather than like it arriving too early.
;
; Acking at the origin clears whatever the wait armed, so the entry the handler
; records is the one the next frame's target line raises.
ScanlineTest
            ori.w   #SR_MASKON,sr       ; mask while the arrangement changes
            move.w  d3,INTSCAN

; THE WAIT IS WaitSet AND NOT A LOOP OF ITS OWN, and that is the difference
; between this figure meaning something and not. T1's calibration counts
; iterations of WaitSet/WaitClear; a bespoke polling loop here counted at a
; different rate, because `tst.w` on an absolute long address plus a far branch
; is not the same cost as `move.w (a0),d0` and `and.w d1,d0` with a short one.
; Dividing one loop's count by another loop's rate read every position 6% low:
; line 100 arrived at 123.25 lines against 132, and the 200-line move between
; the two tests read 187.59. Nothing about the board was wrong.
;
; So the handler's own counter is what gets polled, through the same routine the
; calibration was measured with: a0 points at V_IRQ1CNT and the mask is every
; bit, so WaitSet returns on the first entry and its count is on T1's scale by
; construction rather than by hope.
            bsr     WaitVblank          ; origin: the edge into blank
            move.w  #0,SCANACK          ; drop the latch the wait itself set
            clr.w   V_IRQ1CNT
            clr.w   V_IRQ1FIRST
            clr.w   V_COUNTER
            lea     V_IRQ1CNT,a0
            move.w  #$FFFF,d1
            andi.w  #$F8FF,sr           ; unmask: the handler can now run
            bsr     WaitSet

            ori.w   #SR_MASKON,sr       ; mask again before the caller publishes
            move.w  #415,INTSCAN        ; park it off every measured line
            move.w  #0,SCANACK
            bsr     PetDog
            rts

; Return on the frame's transition into vertical blank. STATUS bit 14 is the
; live VBLANK line and is ACTIVE LOW, so "out of blank" is the bit set. Waiting
; for the high state first makes this an edge rather than a level, so two calls
; in a row cannot both return inside the same blank.
WaitVblank
            movem.l d0/d1/a0,-(a7)
            lea     STATUS,a0
            move.w  #WAITLIMIT,V_LIMIT
WVOut
            subq.w  #1,V_LIMIT
            beq     WaitGaveUp
            move.w  (a0),d0
            andi.w  #VB_MASK,d0
            beq.s   WVOut               ; still in blank, wait for active display
            move.w  #WAITLIMIT,V_LIMIT
WVIn
            subq.w  #1,V_LIMIT
            beq     WaitGaveUp
            move.w  (a0),d0
            andi.w  #VB_MASK,d0
            bne.s   WVIn                ; wait for the edge into blank
            movem.l (a7)+,d0/d1/a0
            rts

; Return at the transition OUT of vertical blank, on the first line of active
; display. ONLY CALL THIS FROM INSIDE THE BLANK, i.e. straight after
; WaitVblank: called from active display it returns immediately and means
; nothing.
WaitActive
            movem.l d0/a0,-(a7)
            lea     STATUS,a0
            move.w  #WAITLIMIT,V_LIMIT
WAOut
            subq.w  #1,V_LIMIT
            beq     WaitGaveUp
            move.w  (a0),d0
            andi.w  #VB_MASK,d0
            beq.s   WAOut               ; still blanked; wait for active display
            movem.l (a7)+,d0/a0
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

; Any write to FF8000 clears the watchdog counter.
PetDog
            move.w  #0,WATCHDOG
            rts

; ===========================================================================
; Interrupt handlers
; ===========================================================================

; Level 1, the scanline interrupt. The latch is held until SCANACK is written,
; so this must ack or RTE drops straight back in. The first entry's position is
; the one worth recording.
Irq1Handler
            move.l  d0,-(a7)
            tst.w   V_IRQ1CNT
            bne.s   I1NotFirst
            move.w  V_COUNTER,d0
            move.w  d0,V_IRQ1FIRST
I1NotFirst
            addq.w  #1,V_IRQ1CNT
            move.w  #0,SCANACK
            cmpi.w  #IRQ1CAP,V_IRQ1CNT
            bcc     Irq1Overrun         ; unsigned >=; never returns
            move.l  (a7)+,d0
            rte

; The scanline latch is asserting far past the one line it names, and the
; program is now living inside its own interrupt handler. Leave the exception
; frame where it is, mask, say so, and park: an RTE here would only come back.
Irq1Overrun
            ori.w   #SR_MASKON,sr
            move.w  #IRQSTORM,R_TIMEOUT
            bra     GaveUpSpin

; Levels 2 and 3, the sound response, alone and combined with the scanline. The
; sound board is held in reset and its latch was drained at entry, so neither
; should ever run. They count rather than trap because a bare board's sound 6502
; has no ROM and this ROM should report what it does rather than assert it.
; R_SND carries the count out, and a nonzero one invalidates the IRQ1 positions
; above rather than the run as a whole.
Irq2Handler
            move.l  d0,-(a7)
            addq.w  #1,V_SNDCNT
            move.w  SNDRESP,d0          ; drain, which drops the line
            move.l  (a7)+,d0
            rte

Irq3Handler
            move.l  d0,-(a7)
            addq.w  #1,V_SNDCNT
            move.w  SNDRESP,d0
            move.w  #0,SCANACK
            move.l  (a7)+,d0
            rte

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

            end
