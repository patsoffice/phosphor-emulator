//! Toobin' (Atari SP-320) video conformance, from a synthetic ROM.
//!
//! Design: `docs/designs/toobin-video-conformance.md`.
//!
//! A machine with **no arcade ROMs at all** takes an assembled 68010 image poked
//! into its program-ROM window through `BusDebug::write`
//! (`AddressSpace32::debug_write` ignores `AccessKind`, so the `ReadOnly` region
//! takes the write), and `M68000::reset` fetches both reset vectors out of it
//! through the bus. The program then measures the board it is running on and
//! writes its verdict into work RAM.
//!
//! Two groups of assertion:
//!
//! - **The loader.** The stack pointer the CPU was handed, a checksum of the
//!   whole image read back through the *real* bus, and the number of vblank
//!   edges the program survived. The last is the watchdog: this board reboots
//!   after 8 vertical blanks with no strobe to `FF8000`, and a reboot clears the
//!   result block, so a program that did not feed it cannot reach the target.
//! - **The video signals.** The vertical blank level, the *horizontal* blank
//!   level, and the programmable scanline interrupt at two lines. Every position
//!   is counted in iterations of one self-calibrating poll loop, so there is no
//!   cycle constant anywhere.
//!
//! **The horizontal blank is why this board is worth a third conformance ROM.**
//! Williams exposed a scanline counter, Road Runner a vertical level plus a
//! programmable interrupt; this board adds a live *horizontal* level, which is
//! the first beam signal in the programme that moves faster than the poll loop
//! that reads it. A blank 64 CPU cycles wide is at the edge of what a polling
//! program can see at all, and getting it to resolve took three attempts at the
//! loop; what the assembly learned is recorded there, next to the loop.

use phosphor_core::core::machine::FrontendMachine;
use phosphor_core::gfx::GfxLayout;
use phosphor_machines::registry;
use phosphor_machines::toobin::{ALPHA_LAYOUT, MO_LAYOUT, PLAYFIELD_LAYOUT, ToobinSystem};

// --- Synthetic graphics -----------------------------------------------------
//
// A ROM-less board has no tiles at all, so every playfield, object and alpha
// pixel decodes to pen 0 and the compositor has nothing to composite. The
// picture phases need a tile set, and this builds one.
//
// **The encoder is deliberately the exact inverse of `decode_gfx`**, walking the
// same plane, x and y offsets and setting the bit where the decoder reads it.
// That is the point and also the limit, and both should be stated.
//
// What it guarantees: pixel (x, y) of tile N really does decode to the pen this
// file asked for. That is the only property the picture phases need, because
// what they are testing is what the *compositor* does with known pens.
//
// What it cannot catch: an error in `decode_gfx` itself, or in the three layout
// constants, since encoding and decoding would cancel. Those are pinned
// elsewhere, by the golden frame against the real ROM set, which is the test
// that would notice the board decoding its actual graphics wrongly. Writing the
// bytes out by hand instead would not fix that either; it would only add a
// second place for the layout to drift from.

/// Build a graphics ROM whose decode is exactly `tiles`.
///
/// `tiles[n]` is tile `n` as `width * height` pen values in row-major order.
fn encode_gfx(tiles: &[Vec<u8>], layout: &GfxLayout, len: usize) -> Vec<u8> {
    let mut rom = vec![0u8; len];
    let width = layout.x_offsets.len();
    for (code, tile) in tiles.iter().enumerate() {
        let code_bits = code * layout.char_increment;
        for (py, &y_off) in layout.y_offsets.iter().enumerate() {
            for (px, &x_off) in layout.x_offsets.iter().enumerate() {
                let pen = tile[py * width + px];
                for (p, &plane_off) in layout.plane_offsets.iter().enumerate() {
                    if pen >> p & 1 == 0 {
                        continue;
                    }
                    let bit_pos = code_bits + plane_off + x_off + y_off;
                    let byte = bit_pos / 8;
                    assert!(
                        byte < len,
                        "tile {code} pixel ({px},{py}) plane {p} lands at byte \
                         {byte}, past the {len}-byte region this layout was \
                         given"
                    );
                    rom[byte] |= 1 << (7 - (bit_pos & 7));
                }
            }
        }
    }
    rom
}

/// A tile of one pen throughout.
fn solid(pen: u8, w: usize, h: usize) -> Vec<u8> {
    vec![pen; w * h]
}

/// Playfield tile set: pen 0, a pen with bit 3 clear, and one with it set.
///
/// The two nonzero pens are what let a test cell drive `PFPIX3`, which is the
/// playfield input the shipped merge rule reads.
const PF_TILE_BLANK: u16 = 0;
const PF_TILE_LO: u16 = 1; // pen 2, bit 3 clear
const PF_TILE_HI: u16 = 2; // pen 10, bit 3 set
const PF_PEN_LO: u16 = 2;
const PF_PEN_HI: u16 = 10;

fn playfield_rom() -> Vec<u8> {
    let tiles = vec![
        solid(0, 8, 8),
        solid(PF_PEN_LO as u8, 8, 8),
        solid(PF_PEN_HI as u8, 8, 8),
    ];
    encode_gfx(&tiles, &PLAYFIELD_LAYOUT, 0x8_0000)
}

/// Object tile set: transparent, a pen with bit 3 clear, and one with it set.
///
/// `LBPIX3` is a PAL input the shipped merge ignores entirely, so the two
/// nonzero pens are what a sweep needs to tell whether it does anything.
///
/// **Each pen gets eight consecutive codes, not one.** An entry's tiles are
/// numbered `base + column * height + row`, so an object eight tiles tall reads
/// eight consecutive codes down its single column. The lead probe needs exactly
/// that: one tall object whose whole length changes pen when a single word of
/// its list entry is rewritten. A one-tile-per-pen set would make that eight
/// writes in an interrupt handler instead of one.
const MO_TILE_STRIDE: u16 = 8;
const MO_TILE_BLANK: u16 = 0;
const MO_TILE_LO: u16 = MO_TILE_STRIDE; // pen 5, bit 3 clear
const MO_TILE_HI: u16 = MO_TILE_STRIDE * 2; // pen 13, bit 3 set
const MO_PEN_LO: u16 = 5;
const MO_PEN_HI: u16 = 13;

fn mo_rom() -> Vec<u8> {
    let mut tiles = Vec::new();
    for pen in [0, MO_PEN_LO as u8, MO_PEN_HI as u8] {
        for _ in 0..MO_TILE_STRIDE {
            tiles.push(solid(pen, 16, 16));
        }
    }
    encode_gfx(&tiles, &MO_LAYOUT, 0x20_0000)
}

/// Alpha tile set: one solid tile per 2bpp pen, so a cell's tile code *is* its
/// `ANPIX1:0`. Pen 0 is the transparent one.
fn alpha_rom() -> Vec<u8> {
    let tiles = vec![
        solid(0, 8, 8),
        solid(1, 8, 8),
        solid(2, 8, 8),
        solid(3, 8, 8),
    ];
    encode_gfx(&tiles, &ALPHA_LAYOUT, 0x4000)
}

/// The assembled test program, a flat 8 KB image loaded at `0x000000`.
///
/// Built from `tests/roms/toobin_video.asm` with `asl` and `p2bin`, both in the
/// Nix dev shell; the exact commands are at the top of the source and in
/// [`the_committed_binary_matches_its_source`], which re-assembles and compares
/// so the two cannot drift.
const PROGRAM: &[u8] = include_bytes!("roms/toobin_video.bin");

/// Where the image is poked, and where the 68010 looks for its reset vectors.
const LOAD_ADDR: u32 = 0x00_0000;
/// The `p2bin -r` window, and therefore the range the program checksums.
const IMAGE_LEN: usize = 0x2000;

const MACHINE: &str = "toobin";

/// Frames to run before giving up. The program spends its first frame on entry
/// and the image checksum, rides [`VB_TARGET`] vblank edges, and then takes four
/// more frames of measurement, so this is a wide margin that still fails fast.
const MAX_FRAMES: usize = 96;

// --- Result block, mirroring the equates in the assembly --------------------
//
// **The assembly writes `FFC000` and this reads `C7C000`, and they are the same
// place.** The board decodes only A23, A22 and A18-A16 above A15
// (`ToobinBoard::mask_addr`), so the CPU's `FFC000` lands in the work-RAM region
// whose base *in masked space* is `C7C000`. The program uses the address the
// real game uses, which is also the one MAME's map decodes, so the same image
// runs under both; the debug bus addresses regions where they live.

/// Work RAM as the ROM addresses it. Not used to peek: see above.
const RES_CPU: u32 = 0xFF_C000;
/// The same work RAM as the debug bus addresses it.
const RES: u32 = 0xC7_C000;
const R_MAGIC: u32 = RES;
const R_PHASE: u32 = RES + 2;
const R_TRAP: u32 = RES + 4;
const R_TRAPV: u32 = RES + 6;
const R_SSP: u32 = RES + 8;
const R_CKSUM: u32 = RES + 12;
const R_VBCOUNT: u32 = RES + 14;
const R_T1_BLANK: u32 = RES + 16;
const R_T1_ACTIVE: u32 = RES + 18;
const R_T1_BLANK2: u32 = RES + 20;
const R_T2_HB: u32 = RES + 22;
const R_T2_HTOT: u32 = RES + 24;
const R_T2_LINES: u32 = RES + 26;
const R_T3_LINE: u32 = RES + 28;
const R_T3_POS: u32 = RES + 30;
const R_T3_CNT: u32 = RES + 32;
const R_T4_LINE: u32 = RES + 34;
const R_T4_POS: u32 = RES + 36;
const R_T4_CNT: u32 = RES + 38;
const R_SND: u32 = RES + 40;
const R_TIMEOUT: u32 = RES + 42;
const R_LEAD_LINE: u32 = RES + 44;
const RESLEN: u32 = 48;

const MAGIC: u16 = 0x5A5A;
const TRAPPED: u16 = 0xDEAD;
const IRQ_STORM: u16 = 0xDEA1;

/// The phase the program publishes last, before the magic word.
const FINAL_PHASE: u16 = 11;

/// Vector 0 of the image: the supervisor stack pointer `cpu.reset` fetches
/// through the bus, parked in work RAM well clear of the result block.
///
/// This one is the CPU's own address, unmasked, because it is compared against
/// what the CPU loaded into A7 rather than used to address anything.
const STACK_TOP: u32 = 0xFF_DF00;

/// Vblank edges the program rides before declaring the watchdog fed. Double the
/// board's 8-frame timeout, on purpose.
const VB_TARGET: u16 = 16;

// --- Board geometry, from machines/src/toobin.rs ----------------------------
//
// These are the *derivations* every expectation below is built from, not
// measurements. The raster is 640 dots by 416 lines with 512x384 visible.

/// Total scanlines in a frame.
const TOTAL_LINES: u32 = 416;
/// Visible scanlines; the rest are vertical blank.
const ACTIVE_LINES: u32 = 384;
/// Blanked scanlines, which is what T1's first and third dwells measure.
const BLANK_LINES: u32 = TOTAL_LINES - ACTIVE_LINES;
/// Dots per scanline.
const TOTAL_DOTS: u32 = 640;
/// Visible dots; the rest are horizontal blank.
const ACTIVE_DOTS: u32 = 512;
/// Blanked dots, which is what T2 measures the share of.
const BLANK_DOTS: u32 = TOTAL_DOTS - ACTIVE_DOTS;

/// The two lines the scanline interrupt is aimed at, mirroring the assembly.
const T3_LINE: u16 = 100;
const T4_LINE: u16 = 300;
/// Lines T2 samples the horizontal blank over, mirroring the assembly.
const HB_LINES: u32 = 64;

// --- Harness ----------------------------------------------------------------

struct Run {
    results: Vec<u8>,
    frames: usize,
}

fn peek(m: &dyn FrontendMachine, addr: u32) -> u8 {
    m.debug_bus()
        .expect("machine exposes a debug bus")
        .read(0, addr)
        .unwrap_or_else(|| panic!("{addr:#08X} is not readable through the debug bus"))
}

fn word(m: &dyn FrontendMachine, addr: u32) -> u16 {
    u16::from_be_bytes([peek(m, addr), peek(m, addr + 1)])
}

/// Build a ROM-less machine, poke the program, run it to completion.
fn run() -> Run {
    run_machine().1
}

/// The same run, keeping the machine so a caller can render the picture it left
/// standing.
///
/// The program idles after writing its magic word, strobing the watchdog and
/// waiting on vblank and touching nothing else, so the sweep it painted stays on
/// screen indefinitely and can be read at leisure.
fn run_machine() -> (Box<dyn FrontendMachine>, Run) {
    // The registry lookup is kept even though the machine is built by hand: it
    // is what fails if the name this file is about stops being registered.
    registry::find(MACHINE).unwrap_or_else(|| panic!("{MACHINE} is not registered"));

    let mut sys = ToobinSystem::new();
    // The same entry points the real ROM loader uses, so the graphics go in the
    // way a cartridge's would. Still no arcade ROMs, still CI-safe.
    sys.board.load_playfield_gfx(&playfield_rom());
    sys.board.load_mo_gfx(&mo_rom());
    sys.board.load_alpha_gfx(&alpha_rom());
    let mut m: Box<dyn FrontendMachine> = Box::new(sys);
    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{MACHINE} exposes no debug bus"));
        for (i, b) in PROGRAM.iter().enumerate() {
            bus.write(0, LOAD_ADDR + i as u32, *b);
        }
    }
    // The 68010 fetches the supervisor stack pointer from 0 and the program
    // counter from 4 through the bus, so this picks up the vectors just poked.
    m.reset();

    let mut frames = 0;
    for _ in 0..MAX_FRAMES {
        m.run_frame();
        frames += 1;
        if word(&*m, R_MAGIC) == MAGIC {
            break;
        }
    }

    let results = (0..RESLEN).map(|i| peek(&*m, RES + i)).collect();
    (m, Run { results, frames })
}

impl Run {
    fn word(&self, addr: u32) -> u16 {
        let o = (addr - RES) as usize;
        u16::from_be_bytes([self.results[o], self.results[o + 1]])
    }

    fn long(&self, addr: u32) -> u32 {
        ((self.word(addr) as u32) << 16) | self.word(addr + 2) as u32
    }

    /// Fail loudly and early if the program never finished, and say why.
    ///
    /// A zero result block is a wedge, not a pass. `R_PHASE` says how far the
    /// program got and `R_TRAP` says whether an exception is the reason, which
    /// is the difference between "the loader never worked" and "the program ran
    /// and then fell over", and those two want completely different next steps.
    fn assert_completed(&self) {
        // Checked before the magic word, because a stall is what the magic
        // word's absence would otherwise be blamed on.
        match self.word(R_TIMEOUT) {
            0 => {}
            TRAPPED => panic!(
                "a wait gave up in phase {}: the signal it was polling for never \
                 arrived, or never went away again. This is a stalled stage, not \
                 a wrong number, so look at what phase {} waits on.",
                self.word(R_PHASE),
                self.word(R_PHASE)
            ),
            IRQ_STORM => panic!(
                "the scanline interrupt fired more times in phase {} than an \
                 acked latch can. The handler writes SCANACK on every entry, so \
                 a storm means the ack is not clearing the latch.",
                self.word(R_PHASE)
            ),
            other => panic!("unknown stall marker {other:#06X} in the result block"),
        }
        if self.word(R_TRAP) == TRAPPED {
            panic!(
                "the conformance program took a stray exception at phase {}: \
                 the 68010 frame's vector-offset word was {:#06X} (vector {}). \
                 Every vector but reset and the three the board drives points at \
                 the handler that recorded this.",
                self.word(R_PHASE),
                self.word(R_TRAPV),
                self.word(R_TRAPV) / 4
            );
        }
        assert_eq!(
            self.word(R_MAGIC),
            MAGIC,
            "the conformance program did not finish in {} frames (reached phase \
             {}, expected {FINAL_PHASE}). A zero result block is a wedge, not a \
             pass; phase 0 means it never executed at all, and anything past 3 \
             with a low R_VBCOUNT ({}) means the watchdog rebooted it.",
            self.frames,
            self.word(R_PHASE),
            self.word(R_VBCOUNT)
        );
    }

    /// Iterations of the shared poll loop per scanline, as T1 measured it.
    ///
    /// **This is the calibration, and it is deliberately not a constant.** The
    /// program has no line counter to read, so position is counted in loop
    /// iterations and the loop's rate is measured in the same run that uses it.
    /// Change the loop, the CPU clock or the emulator's cycle counts and every
    /// ratio below is unmoved.
    fn iters_per_line(&self) -> f64 {
        f64::from(self.word(R_T1_ACTIVE)) / f64::from(ACTIVE_LINES)
    }

    /// A raw iteration count expressed in scanlines.
    fn lines(&self, addr: u32) -> f64 {
        f64::from(self.word(addr)) / self.iters_per_line()
    }
}

// --- The loader -------------------------------------------------------------

#[test]
fn the_program_runs_out_of_poked_rom_and_finishes() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_PHASE),
        FINAL_PHASE,
        "finished, but not at the last phase"
    );
}

/// The ROM writes `FFC000` and this file peeks `C7C000`; assert they are one
/// place rather than leaving it as a comment.
///
/// This is the whole reason the same image can run under our harness and under
/// MAME: the program uses the address the real game uses, and the board folds it
/// onto the region base the debug bus addresses.
#[test]
fn the_roms_work_ram_address_and_the_debug_buss_are_the_same_place() {
    // ToobinBoard::mask_addr: A23, A22 and A18-A16 are decoded above A15, and
    // A21-A19 are not wired to the decoder at all.
    const DECODED: u32 = 0x00C7_FFFF;
    assert_eq!(
        RES_CPU & DECODED,
        RES,
        "the assembly's result block at {RES_CPU:#08X} does not fold onto the \
         work-RAM region base at {RES:#08X}, so every figure this file reads is \
         coming from somewhere the program never wrote"
    );
    assert_eq!(
        STACK_TOP & DECODED & !0xFFFF,
        RES & !0xFFFF,
        "the stack is not in the same region as the result block"
    );
}

#[test]
fn the_cpu_fetched_its_stack_pointer_from_the_poked_image() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.long(R_SSP),
        STACK_TOP,
        "vector 0 reached the CPU through the bus; a wrong value here means the \
         image did not land at {LOAD_ADDR:#08X}"
    );
}

#[test]
fn the_whole_image_reads_back_through_the_real_bus() {
    let r = run();
    r.assert_completed();
    // The same sum the program computes: 4096 big-endian words with 16-bit
    // wraparound, over the committed file.
    let expected = PROGRAM.chunks_exact(2).fold(0u16, |acc, w| {
        acc.wrapping_add(u16::from_be_bytes([w[0], w[1]]))
    });
    assert_eq!(PROGRAM.len(), IMAGE_LEN, "the committed image is not 8 KB");
    assert_eq!(
        r.word(R_CKSUM),
        expected,
        "the CPU's sum over the program-ROM window does not match the committed \
         file. A load at the wrong offset, a short image, or a poke that silently \
         dropped the ReadOnly write all move this."
    );
}

#[test]
fn the_watchdog_was_fed_for_twice_its_timeout() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_VBCOUNT),
        VB_TARGET,
        "the program rides {VB_TARGET} vblank edges strobing the watchdog at \
         each. The board reboots after 8 blanks without a strobe and a reboot \
         clears this block, so a lower count is a reboot rather than a slow run."
    );
}

#[test]
fn the_sound_board_stayed_off_the_interrupt_lines() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_SND),
        0,
        "the sound board raised level 2 or level 3 during the run. It is held in \
         reset and its response latch is drained at entry, so this should be \
         impossible; while it is nonzero the scanline-interrupt positions below \
         are measured against a CPU that was being interrupted by something else."
    );
}

// --- T1: the vertical blank level, and the calibration ----------------------

#[test]
fn the_vertical_blank_lasts_the_lines_the_raster_leaves_for_it() {
    let r = run();
    r.assert_completed();
    let blank = r.lines(R_T1_BLANK);
    assert!(
        (blank - f64::from(BLANK_LINES)).abs() < 1.0,
        "the vertical blank measured {blank:.2} lines against the {BLANK_LINES} \
         the raster leaves for it ({TOTAL_LINES} total less {ACTIVE_LINES} \
         visible). Calibration was {:.3} iterations per line.",
        r.iters_per_line()
    );
}

#[test]
fn the_same_blank_measures_the_same_a_frame_later() {
    let r = run();
    r.assert_completed();
    let a = f64::from(r.word(R_T1_BLANK));
    let b = f64::from(r.word(R_T1_BLANK2));
    // The loop samples the beam asynchronously, so any interval reads correct to
    // within one sample. Requiring the two to be *identical* passes only by
    // coincidence of the loop period and breaks as soon as the loop grows.
    assert!(
        (a - b).abs() <= 1.0,
        "two measurements of the same blank a frame apart came out {a} and {b} \
         iterations. One sample of slack is the sampling error; more than that \
         is the blank changing length between frames."
    );
}

// --- T2: the horizontal blank, and the second calibration -------------------

#[test]
fn the_horizontal_blank_takes_its_share_of_every_line() {
    let r = run();
    r.assert_completed();
    let hb = f64::from(r.word(R_T2_HB));
    let total = f64::from(r.word(R_T2_HTOT));
    let share = hb / total;
    let expected = f64::from(BLANK_DOTS) / f64::from(TOTAL_DOTS);
    // 1.5 points. The loop takes about 11 samples a line and 2.3 of them land in
    // the blank, so one sample of jitter on a single line is 9 points; averaged
    // over HB_LINES lines the measurement comes out within one point, and it
    // does (0.2082 against 0.2000). A wider tolerance here would accept the
    // 0.229 the earlier, coarser loop reported, which was its own period rather
    // than the blank.
    assert!(
        (share - expected).abs() < 0.015,
        "the horizontal blank took {:.1}% of the line against the {:.1}% the \
         raster leaves for it ({BLANK_DOTS} blanked dots of {TOTAL_DOTS}). \
         Measured over {} lines: {hb} iterations blanked of {total}.",
        share * 100.0,
        expected * 100.0,
        r.word(R_T2_LINES)
    );
}

#[test]
fn the_horizontal_blank_recurs_once_per_line() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        u32::from(r.word(R_T2_LINES)),
        HB_LINES,
        "T2 did not sample the number of blanks it was asked to"
    );
    // **Not compared against T1's iterations-per-line, deliberately.** T2's loop
    // is inline where T1's is a subroutine, so the two count at different rates
    // and a cross-check between them would be measuring the difference between
    // two loop bodies. What T2 can say on its own is that it found HB_LINES
    // blanks inside one frame's active display, which it could not have done if
    // the blank recurred at any rate but one per line: 64 blanks have to fit in
    // 384 active lines, and they did.
    let span = f64::from(r.word(R_T2_HTOT)) / f64::from(HB_LINES);
    assert!(
        span > 1.0,
        "T2's {HB_LINES} blanks took {} iterations in total, under one iteration \
         per line. A blank that reads as recurring faster than the loop can \
         sample is the loop seeing the same blank twice, not a line period.",
        r.word(R_T2_HTOT)
    );
}

// --- T3/T4: the programmable scanline interrupt -----------------------------

#[test]
fn the_scanline_interrupt_arrives_at_the_line_it_was_programmed_for() {
    let r = run();
    r.assert_completed();
    for (line_reg, pos_reg) in [(R_T3_LINE, R_T3_POS), (R_T4_LINE, R_T4_POS)] {
        let line = r.word(line_reg);
        // The origin is the edge into vertical blank, which is line 384, so the
        // interrupt's line is BLANK_LINES plus its own number past the wrap.
        let expected = f64::from(BLANK_LINES) + f64::from(line);
        let got = r.lines(pos_reg);
        assert!(
            (got - expected).abs() < 1.5,
            "the interrupt programmed for line {line} arrived {got:.2} lines \
             after the vblank edge, against {expected:.2} derived ({BLANK_LINES} \
             blanked lines plus line {line}). Calibration was {:.3} iterations \
             per line.",
            r.iters_per_line()
        );
    }
}

#[test]
fn moving_the_programmed_line_moves_the_interrupt_by_the_same_amount() {
    let r = run();
    r.assert_completed();
    // The assertion that matters: the two positions share neither the vblank
    // edge nor each other, so the span between them cannot be right by a
    // calibration error that moved both.
    let span = r.lines(R_T4_POS) - r.lines(R_T3_POS);
    let expected = f64::from(T4_LINE - T3_LINE);
    assert!(
        (span - expected).abs() < 1.5,
        "moving the programmed line from {T3_LINE} to {T4_LINE} moved the \
         interrupt by {span:.2} lines against the {expected} asked for."
    );
}

#[test]
fn the_scanline_latch_is_cleared_by_its_acknowledge() {
    let r = run();
    r.assert_completed();
    for reg in [R_T3_CNT, R_T4_CNT] {
        let n = r.word(reg);
        assert_eq!(
            n, 1,
            "the scanline interrupt was taken {n} times in one frame. The \
             handler writes SCANACK on every entry, so more than one entry means \
             the acknowledge is not clearing the latch and RTE drops straight \
             back into the handler."
        );
    }
}

// --- The synthetic graphics themselves --------------------------------------

/// The encoder and the board's decoder agree, checked through the board rather
/// than against a second copy of the arithmetic.
///
/// This is not the round trip proving itself: it installs the tile set through
/// `load_*_gfx`, the same entry point the real ROM loader uses, and then reads
/// pens back out of the decoded cache. What it catches is a tile set that lands
/// at the wrong code, a region sized too small for a layout's plane offsets, or
/// a pen that does not survive the plane split. Those are exactly the mistakes
/// that would make a picture phase measure nothing while looking fine.
#[test]
fn the_synthetic_tiles_decode_to_the_pens_they_were_built_from() {
    let mut sys = ToobinSystem::new();
    sys.board.load_playfield_gfx(&playfield_rom());
    sys.board.load_mo_gfx(&mo_rom());
    sys.board.load_alpha_gfx(&alpha_rom());

    let check = |layer: &str, cache: &phosphor_core::gfx::GfxCache, code: u16, pen: u16| {
        let size = cache.row_slice(code as usize, 0).len();
        for y in 0..size {
            for x in 0..size {
                let got = u16::from(cache.row_slice(code as usize, y)[x]);
                assert_eq!(
                    got, pen,
                    "{layer} tile {code} pixel ({x},{y}) decoded to pen {got}, \
                     not the {pen} it was encoded as"
                );
            }
        }
    };

    let pf = sys.board.playfield_gfx();
    check("playfield", pf, PF_TILE_BLANK, 0);
    check("playfield", pf, PF_TILE_LO, PF_PEN_LO);
    check("playfield", pf, PF_TILE_HI, PF_PEN_HI);

    let mo = sys.board.mo_gfx();
    check("object", mo, MO_TILE_BLANK, 0);
    check("object", mo, MO_TILE_LO, MO_PEN_LO);
    check("object", mo, MO_TILE_HI, MO_PEN_HI);

    let al = sys.board.alpha_gfx();
    for pen in 0..4u16 {
        check("alpha", al, pen, pen);
    }
}

/// Pens differing only in bit 3 must survive as distinct pens.
///
/// The whole point of the object tile pair is to drive `LBPIX3`, the PAL input
/// the shipped merge ignores. If the two pens collapsed to the same value
/// through a plane-order mistake, a sweep over them would report "no difference"
/// for a reason that has nothing to do with the board.
#[test]
fn the_tile_pairs_differ_in_exactly_the_bit_the_sweep_drives() {
    assert_eq!(
        PF_PEN_LO ^ PF_PEN_HI,
        0x08,
        "the playfield pens must differ in bit 3 and nothing else"
    );
    assert_eq!(
        MO_PEN_LO ^ MO_PEN_HI,
        0x08,
        "the object pens must differ in bit 3 and nothing else"
    );
    assert_eq!(PF_PEN_LO & 0x08, 0, "PF_PEN_LO must have bit 3 clear");
    assert_eq!(MO_PEN_LO & 0x08, 0, "MO_PEN_LO must have bit 3 clear");
}

// --- The readout channel ----------------------------------------------------
//
// The picture phases have to report WHICH PALETTE ENTRY the compositor chose,
// because that is exactly what the priority PAL selects: its outputs drive the
// multiplexers that pick one layer's color and pen as the color RAM address.
// A test that only checked "the object is on top" would be reading back less
// than the hardware decides.
//
// So the palette is loaded with an identity code rather than with colors: entry
// `i` gets the low five bits of `i` in red and the next five in green, with bit
// 15 set so the global intensity control cannot scale them. Rendering then
// hands back the index itself, and the layer is the range it falls in:
// playfield below 0x100, object 0x100-0x1FF, alpha 0x200 and up.

/// The palette word that encodes `index` as a readable color.
fn index_color(index: u16) -> u16 {
    0x8000 | (index & 0x1F) << 10 | ((index >> 5) & 0x1F) << 5
}

/// `ToobinBoard::palette_rgb`'s component scaling, which the readout inverts.
///
/// Five bits scale to eight by `(c * 224) >> 5` with a 38-count pedestal on
/// everything but zero, so the 32 steps land on 0 and then 45 to 255 in sevens.
/// Injective, which is what makes the inverse exact rather than a nearest match.
fn component(c: u16) -> u8 {
    let v = (u32::from(c & 0x1F) * 224) >> 5;
    if v != 0 { (v + 38) as u8 } else { 0 }
}

/// Recover the palette index from a rendered pixel.
fn decode_index(r: u8, g: u8) -> u16 {
    let find = |v: u8| -> u16 {
        (0..32u16)
            .find(|&c| component(c) == v)
            .unwrap_or_else(|| panic!("{v} is not one of the 32 component steps"))
    };
    find(r) | find(g) << 5
}

#[test]
fn the_palette_index_survives_the_round_trip_through_a_rendered_pixel() {
    // Every index the sweep can produce, through the same scaling the renderer
    // applies. Checked before any of it is used to read a picture, because an
    // index that aliased would make two layers indistinguishable in the result
    // rather than failing.
    for i in 0..1024u16 {
        let w = index_color(i);
        let (r, g) = (component(w >> 10), component(w >> 5));
        assert_eq!(
            decode_index(r, g),
            i,
            "index {i} encodes to {w:#06X} and reads back wrong"
        );
    }
}

#[test]
fn a_rendered_pixel_reports_which_layer_won() {
    let mut sys = ToobinSystem::new();
    sys.board.load_playfield_gfx(&playfield_rom());
    sys.board.load_mo_gfx(&mo_rom());
    sys.board.load_alpha_gfx(&alpha_rom());
    let mut m: Box<dyn FrontendMachine> = Box::new(sys);

    // Region bases as the debug bus addresses them, which is the masked space.
    const PF: u32 = 0xC0_0000;
    const ALPHA: u32 = 0xC0_8000;
    const PAL: u32 = 0xC1_0000;

    {
        let bus = m.debug_bus_mut().expect("debug bus");
        let mut poke = |addr: u32, w: u16| {
            bus.write(0, addr, (w >> 8) as u8);
            bus.write(0, addr + 1, w as u8);
        };
        for i in 0..1024u16 {
            poke(PAL + u32::from(i) * 2, index_color(i));
        }
        // Playfield cell 0: color 0, priority 0, the bit-3-clear tile.
        poke(PF, 0x0000);
        poke(PF + 2, PF_TILE_LO);
        // Alpha cell 0 transparent for the first look.
        poke(ALPHA, 0x0000);
    }
    m.run_frame();
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    assert_eq!(
        decode_index(rgb[0], rgb[1]),
        PF_PEN_LO,
        "with nothing above it the playfield's own pen should reach the screen"
    );

    // Now put an opaque alpha cell over the same pixel.
    {
        let bus = m.debug_bus_mut().expect("debug bus");
        bus.write(0, ALPHA, 0x00);
        bus.write(0, ALPHA + 1, 0x01); // tile 1 = solid pen 1
    }
    m.run_frame();
    m.render_frame(&mut rgb);
    assert_eq!(
        decode_index(rgb[0], rgb[1]),
        0x200 + 1,
        "an opaque alpha pen should win, and should report in the alpha's own \
         range of the color RAM"
    );
}

// --- The layer-priority sweep -----------------------------------------------
//
// Phase 9 paints [`SWEEP_CELLS`] test cells, one per combination of the four
// live inputs to the priority PAL at 7E, and the palette is loaded so that a
// rendered pixel reports which color RAM address the compositor chose. This
// reads one pixel per cell back and tabulates it.
//
// **What this is and is not.** Every expectation below is derived from our own
// merge rule, so as it stands this is a regression guard: it pins what the
// compositor does, in a form a reader can check cell by cell. It becomes a
// correctness guard only against an oracle that is not us, and for this board
// there is no good one. The 16L8A at 7E was never dumped, in any of the three
// ROM sets, so MAME's priority is somebody's reverse engineering of the same
// sheet we have rather than the part's own contents.
//
// **What it adds over watching the game.** Measured over 3000 frames of
// recorded play, the whole playfield-priority mechanism touches 0.016% of
// pixels and `LBPIX3` 0.004%, and `PFPRI` never leaves 0 and 2. A sweep drives
// all four priority values and both object pens deliberately, so the behavior
// is documented at a resolution the game cannot reach.

const SWEEP_CELLS: usize = 96;
const SWEEP_COLS: usize = 12;
const SWEEP_PITCH: usize = 16;

/// One cell's inputs, unpacked from its index the way the assembly packs them.
struct Cell {
    /// 0 transparent, 1 opaque with pen bit 3 clear, 2 with it set.
    object: usize,
    pfpri: usize,
    pfpix3: usize,
    anpix: usize,
}

impl Cell {
    fn at(index: usize) -> Self {
        Self {
            anpix: index & 3,
            pfpix3: index >> 2 & 1,
            pfpri: index >> 3 & 3,
            object: index >> 5,
        }
    }

    /// The palette index the shipped merge rule predicts for this cell.
    ///
    /// Written out as the rule reads rather than as a table, so the two cannot
    /// drift: object over playfield unless the playfield claims priority and
    /// its pen has bit 3 set, then alpha over everything it is not transparent
    /// on. `LBPIX3` does not appear, which is the point.
    fn predicted(&self) -> u16 {
        let pf_pen = if self.pfpix3 == 1 {
            PF_PEN_HI
        } else {
            PF_PEN_LO
        };
        let mo_pen = if self.object == 2 {
            MO_PEN_HI
        } else {
            MO_PEN_LO
        };
        let pf_blocks = self.pfpri != 0 && pf_pen & 0x08 != 0;
        let mut index = pf_pen;
        if self.object != 0 && !pf_blocks {
            index = 0x100 + mo_pen;
        }
        if self.anpix != 0 {
            index = 0x200 + self.anpix as u16;
        }
        index
    }

    /// Which layer that index belongs to, by the range it falls in.
    fn layer(index: u16) -> &'static str {
        match index {
            0x000..=0x0FF => "playfield",
            0x100..=0x1FF => "object",
            _ => "alpha",
        }
    }
}

/// Run to completion, then read one pixel from the middle of each sweep cell.
fn sweep() -> Vec<u16> {
    let (m, run) = run_machine();
    // A wedge must fail on the magic word rather than on 96 assertions about a
    // blank screen, exactly as the signal tests do.
    run.assert_completed();
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    (0..SWEEP_CELLS)
        .map(|c| {
            let (col, row) = (c % SWEEP_COLS, c / SWEEP_COLS);
            let x = col * SWEEP_PITCH + SWEEP_PITCH / 2;
            let y = row * SWEEP_PITCH + SWEEP_PITCH / 2;
            let o = (y * w as usize + x) * 3;
            decode_index(rgb[o], rgb[o + 1])
        })
        .collect()
}

#[test]
fn every_sweep_cell_selects_the_layer_the_merge_rule_says_it_should() {
    let got = sweep();
    let mut wrong = Vec::new();
    for (c, &index) in got.iter().enumerate() {
        let cell = Cell::at(c);
        let want = cell.predicted();
        if index != want {
            wrong.push(format!(
                "cell {c:2} object {} PFPRI {} PFPIX3 {} ANPIX {}: got {:#05X} \
                 ({}), expected {:#05X} ({})",
                cell.object,
                cell.pfpri,
                cell.pfpix3,
                cell.anpix,
                index,
                Cell::layer(index),
                want,
                Cell::layer(want)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {SWEEP_CELLS} sweep cells disagree with the merge rule:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn the_alpha_covers_every_cell_it_is_not_transparent_on() {
    let got = sweep();
    for (c, &index) in got.iter().enumerate() {
        let cell = Cell::at(c);
        if cell.anpix == 0 {
            continue;
        }
        assert_eq!(
            index,
            0x200 + cell.anpix as u16,
            "cell {c} has alpha pen {} over object {} and playfield priority {}, \
             and something other than the alpha reached the screen. The shipped \
             rule draws the alpha last with pen 0 transparent, so an opaque \
             alpha pen wins everywhere.",
            cell.anpix,
            cell.object,
            cell.pfpri
        );
    }
}

#[test]
fn the_object_pens_bit_3_changes_nothing_but_the_pen() {
    let got = sweep();
    // LBPIX3 is a PAL input the shipped merge ignores. Pairs of cells differing
    // only in it must therefore pick the same LAYER; the index differs, because
    // the two object tiles carry different pens, and that is not the question.
    for c in 0..SWEEP_CELLS {
        let cell = Cell::at(c);
        if cell.object != 1 {
            continue;
        }
        let paired = c + 32; // the same inputs with the bit-3-set object tile
        assert_eq!(
            Cell::layer(got[c]),
            Cell::layer(got[paired]),
            "cells {c} and {paired} differ only in the object pen's bit 3, \
             which is LBPIX3, and they selected different layers ({:#05X} \
             against {:#05X}). The shipped rule does not read that input, so \
             this is a real change in behavior rather than a different pen.",
            got[c],
            got[paired]
        );
    }
}

#[test]
fn the_playfield_only_beats_an_opaque_object_where_it_claims_priority() {
    let got = sweep();
    for (c, &index) in got.iter().enumerate() {
        let cell = Cell::at(c);
        if cell.object == 0 || cell.anpix != 0 {
            continue; // the object is not there, or the alpha hides the answer
        }
        let pf_wins = Cell::layer(index) == "playfield";
        let should = cell.pfpri != 0 && cell.pfpix3 == 1;
        assert_eq!(
            pf_wins,
            should,
            "cell {c}: an opaque object over playfield priority {} with PFPIX3 \
             {}. The playfield {} but the rule says it {}.",
            cell.pfpri,
            cell.pfpix3,
            if pf_wins { "won" } else { "lost" },
            if should { "should win" } else { "should lose" }
        );
    }
}

#[test]
fn the_sweep_actually_drove_all_four_priority_values() {
    // The guard against a vacuous pass. If the grid were not painted at all
    // every cell would read the same index, and every assertion above that
    // compares a cell to the rule would still have something to say about a
    // blank screen. This is what says the experiment happened.
    let got = sweep();
    let distinct: std::collections::BTreeSet<u16> = got.iter().copied().collect();
    assert!(
        distinct.len() >= 5,
        "the sweep produced only {} distinct palette indices ({:#05X?}). It \
         should reach both playfield pens, both object pens and three alpha \
         pens; this few means the grid was not painted.",
        distinct.len(),
        distinct
    );
    // And the playfield-wins case has to actually occur, or the priority half
    // of the sweep proved nothing.
    let pf_wins = got
        .iter()
        .enumerate()
        .filter(|&(c, &i)| {
            let cell = Cell::at(c);
            cell.object != 0 && cell.anpix == 0 && Cell::layer(i) == "playfield"
        })
        .count();
    assert!(
        pf_wins > 0,
        "no cell had the playfield beat an opaque object, so the priority \
         mechanism was never exercised"
    );
}

// --- The object sampling lead -----------------------------------------------
//
// Sheet 13 settles that this board has two line-buffer SRAMs gated against `1V`
// and `/1V`, so one is filled while the other is displayed and what the beam
// shows on a line was scanned during the line before it. What it does not settle
// is whether the vertical match constant on sheet 7 already absorbs that, and
// `machines/CLAUDE.md` warns that adding the delay twice moves every object
// pixel the wrong way.
//
// **So the probe is a latency, not a position.** "Is this object on the right
// row" can only be answered against an oracle, and our own answer is the thing
// under test. "How many rows after a write to the list does the change appear"
// is answerable here, and it is the same quantity: a path that scans a line
// ahead cannot show a change on the very next line, because that line was
// already scanned.
//
// The playfield takes the identical probe in the same interrupt as the control.
// It has no line buffer, so the difference between the two is the object path's
// lead; a shared answer is the handler's own latency turning up in both, which
// is exactly what a single probe could not tell apart.

/// The probe object's left edge and top line, mirroring the assembly.
const LEAD_MOX: usize = 300;
const LEAD_MOY: usize = 200;
/// The playfield probe's column in pixels.
const LEAD_PFX: usize = 400;
/// Rows the painted playfield band covers, from `LEAD_PFTOP * 8`.
const LEAD_PF_TOP: usize = 25 * 8;
const LEAD_PF_ROWS: usize = 11 * 8;

/// The first row at or below `from` where the sampled column stops reading
/// `before`, and what it reads there.
fn first_change(
    rgb: &[u8],
    w: usize,
    x: usize,
    from: usize,
    to: usize,
    before: u16,
) -> (usize, u16) {
    for y in from..to {
        let o = (y * w + x) * 3;
        let index = decode_index(rgb[o], rgb[o + 1]);
        if index != before {
            return (y, index);
        }
    }
    panic!(
        "column {x} never changed from {before:#05X} between rows {from} and \
         {to}. The probe writes once a frame and is undone at every vertical \
         blank, so a column that never changes means the interrupt never made \
         its writes."
    );
}

struct Lead {
    line: usize,
    mo_row: usize,
    mo_index: u16,
    pf_row: usize,
    pf_index: u16,
}

fn lead() -> Lead {
    let (m, run) = run_machine();
    run.assert_completed();
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    let w = w as usize;
    let line = usize::from(run.word(R_LEAD_LINE));
    assert!(
        line > LEAD_MOY && line < LEAD_MOY + 128,
        "the probe line {line} is not inside the probe object's 128 rows"
    );
    // Sampled in the middle of each probe, clear of every edge.
    let (mo_row, mo_index) = first_change(
        &rgb,
        w,
        LEAD_MOX + 8,
        LEAD_MOY,
        LEAD_MOY + 128,
        0x100 + MO_PEN_LO,
    );
    let (pf_row, pf_index) = first_change(
        &rgb,
        w,
        LEAD_PFX + 4,
        LEAD_PF_TOP,
        LEAD_PF_TOP + LEAD_PF_ROWS,
        PF_PEN_LO,
    );
    Lead {
        line,
        mo_row,
        mo_index,
        pf_row,
        pf_index,
    }
}

#[test]
fn a_mid_frame_write_reaches_the_very_next_row_on_both_paths() {
    let l = lead();
    // The interrupt latch is set at the start of its line and that line is
    // composited immediately, before the CPU runs, so the handler's writes
    // cannot reach the line they fired on. The next row is the earliest
    // possible, and anything later is a path that had already read ahead.
    assert_eq!(
        l.pf_row,
        l.line + 1,
        "the playfield changed at row {} for a write made during row {}. This \
         is the control: the playfield has no line buffer, so anything but the \
         next row is the handler being slower than a scanline rather than \
         anything about the object path.",
        l.pf_row,
        l.line
    );
    assert_eq!(
        l.mo_row,
        l.line + 1,
        "the object changed at row {} for a write made during row {}, against \
         the playfield's row {}.",
        l.mo_row,
        l.line,
        l.pf_row
    );
}

#[test]
fn the_object_path_reads_the_list_no_earlier_than_the_playfield_reads_its_map() {
    let l = lead();
    // THIS IS THE MEASUREMENT jg18.2 ASKED FOR, as a single number. Zero means
    // our renderer applies no lead: it reads the object list live at the row it
    // is drawing, exactly as it reads the playfield map. One would mean a lead
    // is already modeled and adding another would double it.
    //
    // What it does NOT say is what the board does. The sheet establishes the
    // line buffers are real; whether sheet 7's match constant absorbs them is
    // still open, and that needs an oracle this file does not have.
    let object_lead = l.mo_row as isize - l.pf_row as isize;
    assert_eq!(
        object_lead, 0,
        "the object path showed a mid-frame list change {} row(s) after the \
         playfield showed the same-instant map change (object row {}, playfield \
         row {}, both written during row {}). A nonzero lead here is a real \
         difference between the two paths in this renderer.",
        object_lead, l.mo_row, l.pf_row, l.line
    );
}

#[test]
fn both_probes_changed_to_the_pen_they_were_pointed_at() {
    let l = lead();
    // Guards against a change that is real but is not the one asked for: a
    // shifted object, a palette write landing somewhere unintended, or a column
    // that ran off its painted band and read the cleared map instead.
    assert_eq!(
        l.mo_index,
        0x100 + MO_PEN_HI,
        "the object column changed to {:#05X} rather than to the bit-3-set \
         object pen, so the row it changed at is not measuring the write the \
         handler made",
        l.mo_index
    );
    assert_eq!(
        l.pf_index, PF_PEN_HI,
        "the playfield column changed to {:#05X} rather than to the bit-3-set \
         playfield pen",
        l.pf_index
    );
}

// --- The drift guard --------------------------------------------------------

/// The committed binary is the one artifact no reviewer can check by reading, so
/// re-assemble the source and byte-compare.
///
/// **It must not be allowed to pass by doing nothing.** The first version of this
/// guard on Williams reported green for its entire life because no assembler
/// existed on `PATH` anywhere, including inside the dev shell. The dev shell now
/// exports `PHOSPHOR_ASM=1`; with it set, a missing assembler is a failure rather
/// than a skip. CI has no dev shell, sets nothing, and skips with a printed note.
#[test]
fn the_committed_binary_matches_its_source() {
    use std::path::Path;
    use std::process::Command;

    let roms = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/roms");
    let asm = roms.join("toobin_video.asm");
    let tmp = std::env::temp_dir();
    let code = tmp.join("phosphor_toobin_video_check.p");
    let out = tmp.join("phosphor_toobin_video_check.bin");
    // asl appends to an existing code file rather than truncating it, so a
    // leftover from an earlier run would be re-read by p2bin.
    let _ = std::fs::remove_file(&code);

    let expected = std::env::var_os("PHOSPHOR_ASM").is_some();

    let assembled = Command::new("asl")
        .arg("-q")
        .arg("-o")
        .arg(&code)
        .arg(&asm)
        .status();
    let assembled = match assembled {
        Ok(status) => status,
        Err(e) => {
            assert!(
                !expected,
                "PHOSPHOR_ASM is set, so `asl` is supposed to be on PATH here, \
                 but running it failed: {e}. The dev shell provides it; a skip \
                 at this point would report green while guarding nothing."
            );
            eprintln!("skipping: `asl` is not on PATH and PHOSPHOR_ASM is unset");
            return;
        }
    };
    assert!(assembled.success(), "asl failed on {}", asm.display());

    let range = format!("0x0000-0x{:04X}", IMAGE_LEN - 1);
    let converted = Command::new("p2bin")
        .arg(&code)
        .arg(&out)
        .args(["-r", &range, "-l", "0xA5"])
        .status()
        .expect("p2bin runs when asl did");
    assert!(converted.success(), "p2bin failed on {}", code.display());

    let built = std::fs::read(&out).expect("read re-assembled image");
    let _ = std::fs::remove_file(&code);
    let _ = std::fs::remove_file(&out);

    let stale = format!(
        "tests/roms/toobin_video.bin is stale. Rebuild it with\n  \
         asl -q -o out.p toobin_video.asm\n  \
         p2bin out.p toobin_video.bin -r {range} -l 0xA5"
    );
    assert_eq!(
        built.len(),
        PROGRAM.len(),
        "re-assembled image is {} bytes, committed is {}. {stale}",
        built.len(),
        PROGRAM.len()
    );
    let differs = built
        .iter()
        .zip(PROGRAM)
        .position(|(a, b)| a != b)
        .map(|i| {
            format!(
                "first difference at ${:06X}: built {:#04X}, committed {:#04X}",
                i, built[i], PROGRAM[i]
            )
        });
    assert!(
        differs.is_none(),
        "{}. {stale}",
        differs.unwrap_or_default()
    );
}
