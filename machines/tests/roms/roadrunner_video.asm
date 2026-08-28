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
; machine built with MachineEntry::create_bare, by poking the image through
; BusDebug::write (AddressSpace32::debug_write ignores AccessKind, so the
; ReadOnly program-ROM region takes the write). The 68000 fetches its stack
; pointer from 0 and its program counter from 4 through the bus, so both come
; from this image.
;
; THIS IS THE SKELETON STEP AND IT ASSERTS NOTHING ABOUT VIDEO. It proves the
; loader carries to a 68000 board on AddressSpace32, that a program executes out
; of poked ROM, and that it survives the watchdog. VBLANK, IRQ4 and the
; placeable IRQ3 belong to the next issue.
;
; EVERY WAIT IS A POLL OF HARDWARE STATE, NEVER A DELAY LOOP, so a constant
; cycle offset between two implementations cancels.
; ---------------------------------------------------------------------------

            cpu     68000

; --- Hardware ---------------------------------------------------------------

WATCHDOG    equ $880001         ; any write clears the counter
VBLANK_ACK  equ $8A0001         ; write acks the VBLANK IRQ4 latch
SWITCHES    equ $F60000         ; bit 4 = VBLANK, ACTIVE LOW (0 during blank)

VB_MASK     equ $0010           ; the VBLANK bit inside the word read of F60000

; The image, and therefore the range the CPU checksums back through the real
; bus. Fixed by p2bin's -r window; the harness computes the same sum over the
; committed .bin file, so a load at the wrong offset or a short image moves it.
IMGBASE     equ $000000
IMGWORDS    equ $1000           ; 8 KB / 2

; Work RAM is 400000-401FFF (8 KB, undisplayed). Unlike Williams there is no
; video RAM to hide scratch in, and no need: the result block and the stack both
; live here with kilobytes to spare between them.
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
R_VBCOUNT   equ RES+14          ; vblank edges observed
RESLEN      equ 16

MAGIC       equ $5A5A
TRAPPED     equ $DEAD

; Vblank edges to survive before declaring the watchdog fed. Deliberately double
; the 8-frame timeout: the program cannot reach this count unless every strobe
; landed, because a reboot clears the result block and starts the count over.
VB_TARGET   equ 16

; ===========================================================================
; Exception vectors
;
; Vector 0 is the supervisor stack pointer and vector 1 is the entry point;
; cpu.reset fetches both through the bus, so they are the load-bearing halves of
; this image. EVERY OTHER VECTOR POINTS AT A HANDLER rather than at zero, so a
; mistake records itself instead of executing whatever happens to be at address
; 0. That includes the autovectored interrupt levels, which are masked here but
; will not be in the next issue.
; ===========================================================================
            org     IMGBASE

            dc.l    STACKTOP            ; 0: reset SSP
            dc.l    Reset               ; 1: reset PC
            rept    254
            dc.l    StrayException
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

; A zero result block must never read as a pass, so clear it deliberately.
; Clearing it is also what makes a watchdog reboot visible: reset() does not
; clear work RAM, so without this a reboot would leave a plausible
; half-finished block behind instead of restarting the counts.
            lea     RES,a0
            moveq   #(RESLEN/2)-1,d0
ClrRes
            move.w  #0,(a0)+
            dbra    d0,ClrRes

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
; the strobe lands.
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
; Done
; ===========================================================================
            move.w  #5,R_PHASE
            move.w  #MAGIC,R_MAGIC
Spin
            bsr     WaitVblank
            bsr     PetDog
            bra     Spin

; ===========================================================================
; Helpers
; ===========================================================================

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
WVOut
            move.w  SWITCHES,d0
            andi.w  #VB_MASK,d0
            beq.s   WVOut               ; still in blank, wait for active display
WVIn
            move.w  SWITCHES,d0
            andi.w  #VB_MASK,d0
            bne.s   WVIn                ; wait for the edge into blank
            move.l  (a7)+,d0
            rts

; Any write to 880001 clears the watchdog counter. A byte write there becomes a
; read-modify-write of the word at 880000, which the board does not decode and
; which returns $FFFF with no side effect, so the byte form is safe and is what
; the hardware expects at an odd-addressed register.
PetDog
            move.b  #0,WATCHDOG
            rts

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
