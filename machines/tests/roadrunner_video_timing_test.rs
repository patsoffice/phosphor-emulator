//! Road Runner (Atari System 1) video timing conformance, from a synthetic ROM.
//!
//! Design: `docs/designs/roadrunner-video-conformance.md`.
//!
//! A machine with **no arcade ROMs at all** takes an assembled 68000 image poked
//! into its program-ROM window through `BusDebug::write`
//! (`AddressSpace32::debug_write` ignores `AccessKind`, so the `ReadOnly` region
//! takes the write), and `M68000::reset` fetches both reset vectors out of it
//! through the bus. The program then measures the board it is running on and
//! writes its verdict into work RAM. Because no arcade ROM is involved, this is
//! the only test in the tree that asks what a machine draws *and* runs in CI.
//!
//! Three groups of assertion:
//!
//! - **The loader.** The stack pointer the CPU was handed, a checksum of the
//!   whole image read back through the *real* bus, and the number of vblank
//!   edges the program survived. The last is the watchdog: this board reboots
//!   after 8 frames without a strobe to `880001` (`roadrunner.rs:773-775`), and
//!   a reboot clears the result block, so a program that did not feed it cannot
//!   reach the target count.
//! - **The video signals**, whose verdict is a word in RAM: the VBLANK level,
//!   IRQ4 and its ack, and the motion-object timer interrupt placed on a
//!   scanline the program picks. Every position is counted in iterations of one
//!   self-calibrating poll loop, so there is no cycle constant anywhere.
//! - **The picture.** The program draws all three layers and then performs two
//!   mid-frame writes at a scanline the timer interrupt names. **Both of those
//!   assertions describe behaviour that is wrong**, held as a ratchet against
//!   `phosphor-emulator-raster-sampling-6kae.3`; see [`KNOWN_DEFECTS`].

use phosphor_core::core::machine::FrontendMachine;
use phosphor_machines::registry;
use phosphor_machines::roadrunner::RoadRunnerSystem;

/// The assembled test program, a flat 8 KB image loaded at `0x000000`.
///
/// Built from `tests/roms/roadrunner_video.asm` with `asl` and `p2bin`, both in
/// the Nix dev shell; the exact commands are at the top of the source and in
/// [`the_committed_binary_matches_its_source`], which re-assembles and compares
/// so the two cannot drift.
const PROGRAM: &[u8] = include_bytes!("roms/roadrunner_video.bin");

/// Where the image is poked, and where the 68000 looks for its reset vectors.
const LOAD_ADDR: u32 = 0x00_0000;
/// The `p2bin -r` window, and therefore the range the program checksums.
const IMAGE_LEN: usize = 0x2000;

const MACHINE: &str = "roadrunner";

/// Frames to run before giving up. The program spends its first frame on entry
/// and the image checksum and then rides [`VB_TARGET`] vblank edges, so twice
/// that is a wide margin which still fails fast on a wedge.
const MAX_FRAMES: usize = 64;

// --- Result block, mirroring the equates in the assembly --------------------

const RES: u32 = 0x40_0000;
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
const R_T2_COUNT: u32 = RES + 22;
const R_T2_HELD: u32 = RES + 24;
const R_T2_VB: u32 = RES + 26;
const R_T3_POLL_A: u32 = RES + 28;
const R_T3_END_A: u32 = RES + 30;
const R_T3_POLL_B: u32 = RES + 32;
const R_T4_CNT: u32 = RES + 34;
const R_T4_FIRST: u32 = RES + 36;
const R_T5_POLL: u32 = RES + 38;
const R_TIMEOUT: u32 = RES + 40;
const RESLEN: u32 = 48;

const MAGIC: u16 = 0x5A5A;
const TRAPPED: u16 = 0xDEAD;
const IRQ3_STORM: u16 = 0xDEA3;
const FINAL_PHASE: u16 = 15;

// --- Board geometry the expectations are derived from -----------------------
//
// `atari_system1::TIMING`: 262 scanlines of 456 cycles, and `VBLANK_SCANLINE`
// is 240, so the display is active for lines 0-239 and blanked for 240-261.

/// Active scanlines per frame.
const ACTIVE_LINES: f64 = 240.0;
/// Blanked scanlines per frame (`262 - VBLANK_SCANLINE`).
const BLANK_LINES: f64 = 22.0;

/// The scanline the first motion-object timer entry targets.
const TIMER_LINE_A: f64 = 64.0;
/// ... and the second, 96 lines further down.
const TIMER_LINE_B: f64 = 160.0;
/// The scanline the picture phases perform their mid-frame writes on.
const TIMER_LINE_C: usize = 120;

// --- Synthetic graphics -----------------------------------------------------
//
// A board built with no ROM set has no tile or font graphics, so every playfield
// pixel and every motion-object pixel is pen 0 and nothing can be drawn. The
// harness therefore builds a font and a tile set of its own and installs them
// through the same `load_alpha`/`load_gfx` the ROM loader uses. They are defined
// here rather than captured, so the expected picture is derivable.
//
// The bit layouts below are the ones `atari_system1.rs` decodes with, restated:
// `ALPHA_LAYOUT` at `:86-93` and `tile_layout` at `:130-142`. Restating them is
// a coupling, and a deliberate one. Get either wrong, here or there, and the
// picture assertions fail rather than quietly drawing the wrong thing.

/// Alpha `x_offsets`, from `ALPHA_LAYOUT` (`atari_system1.rs:88`).
const ALPHA_X_OFFSETS: [usize; 8] = [0, 1, 2, 3, 8, 9, 10, 11];
/// Alpha rows are 16 bits apart and a glyph is 128 bits, i.e. 16 bytes.
const ALPHA_ROW_BITS: usize = 16;
const ALPHA_GLYPH_BYTES: usize = 16;
/// `ALPHA_TILE_COUNT` (`atari_system1.rs:94`).
const ALPHA_TILES: usize = 512;
/// `plane_offsets` is `{4, 0}`, so pen bit 0 sits 4 bits into each cell.
const ALPHA_PEN0_BIT: usize = 4;

/// Tile planes are `0x80000` **bits** apart, i.e. `0x10000` bytes
/// (`TILE_PLANES_4`, `atari_system1.rs:123`).
const TILE_PLANE_STRIDE: usize = 0x1_0000;
/// A tile is 64 bits per plane: eight rows of one byte, MSB at x 0.
const TILE_BYTES: usize = 8;
/// Enough for four planes of 4096 tiles. Reads past the end decode as pen 0
/// (`decode_gfx` bounds-checks per byte), so this only has to cover the planes
/// the pens below actually use.
const TILE_ROM_LEN: usize = 3 * TILE_PLANE_STRIDE + 4096 * TILE_BYTES;

/// 8x8 glyphs, `#` for pen 1 and anything else transparent. Index is the alpha
/// code the ROM writes; 0 is blank, and is also what the alpha layer is cleared
/// to.
const GLYPHS: [[&str; 8]; 8] = [
    [
        "........", "........", "........", "........", "........", "........", "........",
        "........",
    ],
    [
        ".####...", ".#...#..", ".#...#..", ".####...", ".#..#...", ".#...#..", ".#...#..",
        "........",
    ], // R
    [
        "..###...", ".#...#..", ".#...#..", ".#...#..", ".#...#..", ".#...#..", "..###...",
        "........",
    ], // O
    [
        "..###...", ".#...#..", ".#...#..", ".#####..", ".#...#..", ".#...#..", ".#...#..",
        "........",
    ], // A
    [
        ".####...", ".#...#..", ".#...#..", ".#...#..", ".#...#..", ".#...#..", ".####...",
        "........",
    ], // D
    [
        ".#...#..", ".#...#..", ".#...#..", ".#...#..", ".#...#..", ".#...#..", "..###...",
        "........",
    ], // U
    [
        ".#...#..", ".##..#..", ".#.#.#..", ".#.#.#..", ".#..##..", ".#...#..", ".#...#..",
        "........",
    ], // N
    [
        ".#####..", ".#......", ".#......", ".####...", ".#......", ".#......", ".#####..",
        "........",
    ], // E
];

/// The alpha font ROM: [`GLYPHS`] encoded into `ALPHA_LAYOUT`'s bit order.
fn font_rom() -> Vec<u8> {
    let mut rom = vec![0u8; ALPHA_TILES * ALPHA_GLYPH_BYTES];
    for (code, glyph) in GLYPHS.iter().enumerate() {
        for (y, row) in glyph.iter().enumerate() {
            for (x, c) in row.chars().take(8).enumerate() {
                if c != '#' {
                    continue;
                }
                let bit = code * ALPHA_GLYPH_BYTES * 8
                    + y * ALPHA_ROW_BITS
                    + ALPHA_X_OFFSETS[x]
                    + ALPHA_PEN0_BIT;
                rom[bit / 8] |= 0x80 >> (bit % 8);
            }
        }
    }
    rom
}

/// The playfield and motion-object tile ROM: tile N is a solid block of pen N,
/// for N in 1 to 4. Tile 0 is left blank.
fn tile_rom() -> Vec<u8> {
    let mut rom = vec![0u8; TILE_ROM_LEN];
    for tile in 1..=4usize {
        for plane in 0..4 {
            if tile & (1 << plane) == 0 {
                continue;
            }
            let base = plane * TILE_PLANE_STRIDE + tile * TILE_BYTES;
            rom[base..base + TILE_BYTES].fill(0xFF);
        }
    }
    rom
}

/// The two mapping PROMs, as one 1 KB image: playfield `prom1` at `0x000` and
/// `prom2` at `0x200`, motion objects at `0x100` and `0x300`
/// (`build_tile_gfx`, `atari_system1.rs:229-267`).
///
/// Every entry is the same: `prom1` `0x00` leaves the active-low bank-1 select
/// asserted and the tile offset at zero, and `prom2` `0x0F` leaves the
/// plane-4 enable clear (so 4bpp) with the negative-logic colour mask all ones,
/// which is colour 0. So both layers land in colour 0 and the palette indices
/// are `0x100 + pen` for a sprite and `0x200 + pen` for the playfield.
fn prom() -> Vec<u8> {
    let mut prom = vec![0u8; 0x400];
    prom[0x200..0x400].fill(0x0F);
    prom
}

/// Vector 0 of the image: the supervisor stack pointer `cpu.reset` fetches
/// through the bus, parked in work RAM well clear of the result block.
const STACK_TOP: u32 = 0x40_1F00;

/// Vblank edges the program rides before declaring itself done. Double the
/// board's 8-frame watchdog timeout, on purpose.
const VB_TARGET: u16 = 16;

// --- Harness ----------------------------------------------------------------

/// The first picture phase; phases [`FIRST_SHOT_PHASE`] to 14 are captured.
const FIRST_SHOT_PHASE: u16 = 11;
const SHOT_COUNT: usize = 4;

struct Run {
    results: Vec<u8>,
    frames: usize,
    /// The frame index (1-based) in which `R_PHASE` first read 4, i.e. the frame
    /// the watchdog ride finished in. One vblank edge per frame makes this equal
    /// to `VB_TARGET`; an edge detector that retriggered inside one blank would
    /// finish sooner.
    watchdog_ride_frames: Option<usize>,
    /// One frame captured per picture phase, in phase order.
    shots: [Option<Shot>; SHOT_COUNT],
}

struct Shot {
    w: usize,
    rgb: Vec<u8>,
}

impl Shot {
    fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.w + x) * 3;
        (self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }
}

fn peek(m: &dyn FrontendMachine, addr: u32) -> u8 {
    m.debug_bus()
        .expect("machine exposes a debug bus")
        .read(0, addr)
        .unwrap_or_else(|| panic!("{addr:#08X} is not readable through the debug bus"))
}

fn render(m: &mut dyn FrontendMachine) -> Shot {
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    Shot { w: w as usize, rgb }
}

/// Build the machine, install the synthetic graphics, poke the program, run.
///
/// **Built directly rather than through `MachineEntry::create_bare`**, which is
/// what the signal-only version of this file used. The picture phases need tile
/// and font graphics, and a bare board has none: `PlayfieldGfx::empty` leaves one
/// blank placeholder bank, so every playfield and sprite pixel decodes to pen 0
/// and the compositor has nothing to draw. `load_alpha` and `load_gfx` are the
/// same entry points the real ROM loader uses, so the graphics go in the way a
/// cartridge's would. Still no arcade ROMs, still CI-safe.
fn run() -> Run {
    // The registry lookup is kept even though the machine is built by hand: it
    // is what fails if the name this file is about stops being registered.
    registry::find(MACHINE).unwrap_or_else(|| panic!("{MACHINE} is not registered"));

    let mut sys = RoadRunnerSystem::new();
    sys.board.load_alpha(&font_rom());
    sys.board.load_gfx(&prom(), &tile_rom());
    let mut m: Box<dyn FrontendMachine> = Box::new(sys);

    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{MACHINE} exposes no debug bus"));
        for (i, b) in PROGRAM.iter().enumerate() {
            bus.write(0, LOAD_ADDR + i as u32, *b);
        }
    }
    // The 68000 fetches the supervisor stack pointer from 0 and the program
    // counter from 4 through the bus, so this picks up the vectors the image
    // just installed.
    m.reset();

    let mut frames = 0;
    let mut watchdog_ride_frames = None;
    let mut shots: [Option<Shot>; SHOT_COUNT] = [None, None, None, None];
    for _ in 0..MAX_FRAMES {
        m.run_frame();
        frames += 1;
        let phase = word(&*m, R_PHASE);
        if watchdog_ride_frames.is_none() && phase >= 4 {
            watchdog_ride_frames = Some(frames);
        }
        // Each picture phase is published at the vblank edge of the frame it
        // describes and is the last thing written in that frame, so the frame
        // that just ended is the one to capture. The program holds phase 14 for
        // a whole frame before publishing 15 for exactly this reason.
        if (FIRST_SHOT_PHASE..FIRST_SHOT_PHASE + SHOT_COUNT as u16).contains(&phase) {
            let slot = (phase - FIRST_SHOT_PHASE) as usize;
            if shots[slot].is_none() {
                shots[slot] = Some(render(&mut *m));
            }
        }
        if word(&*m, R_MAGIC) == MAGIC {
            break;
        }
    }

    let results = (0..RESLEN).map(|i| peek(&*m, RES + i)).collect();
    Run {
        results,
        frames,
        watchdog_ride_frames,
        shots,
    }
}

fn word(m: &dyn FrontendMachine, addr: u32) -> u16 {
    u16::from_be_bytes([peek(m, addr), peek(m, addr + 1)])
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
        // word's absence would otherwise be blamed on. The program bounds every
        // wait and parks itself here rather than spinning until the watchdog
        // reboots it, which is what makes R_PHASE name the stage that stalled
        // instead of whatever the restarted run reached.
        match self.word(R_TIMEOUT) {
            0 => {}
            TRAPPED => panic!(
                "a wait gave up in phase {}: the signal it was polling for never \
                 arrived, or never went away again. This is a stalled stage, not \
                 a wrong number, so look at what phase {} waits on.",
                self.word(R_PHASE),
                self.word(R_PHASE)
            ),
            IRQ3_STORM => panic!(
                "IRQ3 fired more times in phase {} than a one-scanline pulse can: \
                 nothing acks it, so a level held longer than its line traps the \
                 CPU in its own handler. The interrupt is not being released at \
                 the line boundary.",
                self.word(R_PHASE)
            ),
            other => panic!("unknown stall marker {other:#06X} in the result block"),
        }
        if self.word(R_TRAP) == TRAPPED {
            panic!(
                "the conformance program took a stray exception at phase {}: \
                 the 68010 frame's vector-offset word was {:#06X} (vector {}). \
                 Every vector but reset points at the handler that recorded this.",
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
        assert_eq!(self.word(R_PHASE), FINAL_PHASE, "phase mismatch");
    }
}

// ---------------------------------------------------------------------------
// The loader
// ---------------------------------------------------------------------------

/// A 68000 program executes out of poked ROM on a machine with no arcade ROMs.
///
/// The precondition for everything below and the whole point of this step: the
/// Williams loading mechanism rests on `AddressSpace16` and an M6809, and until
/// this passed, the rest of the epic was speculation.
#[test]
fn the_conformance_program_runs_to_completion() {
    run().assert_completed();
}

/// `cpu.reset` fetched the supervisor stack pointer out of the poked image.
///
/// The program records `A7` before touching it. The program counter half of the
/// reset fetch is self-evident from the program running at all; this is the
/// other half, and it fails rather than wanders if vector 0 did not arrive.
#[test]
fn the_reset_vectors_come_from_the_poked_image() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.long(R_SSP),
        STACK_TOP,
        "the stack pointer the CPU started with should be vector 0 of the image"
    );
}

/// The CPU reads the whole 8 KB image back through the bus it actually drives.
///
/// The poke went in through the debug bus, which is not the bus the program
/// runs on. This is the assertion that every byte arrived at the right address:
/// the program sums 4096 big-endian words with 16-bit wraparound and the test
/// sums the committed file the same way. A load at the wrong offset, a short
/// image, or a poke that silently dropped `ReadOnly` writes all move it.
///
/// The image is padded with `$A5` rather than zero precisely so this stays a
/// real check: a ROM-less board's program ROM is already zero-filled, so with a
/// zero pad a truncated load would sum to the same value as a complete one. It
/// did, when this was first written with `-l 0x00`.
#[test]
fn the_whole_image_reads_back_through_the_cpu_bus() {
    let expected = PROGRAM.chunks_exact(2).fold(0u16, |acc, w| {
        acc.wrapping_add(u16::from_be_bytes([w[0], w[1]]))
    });
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_CKSUM),
        expected,
        "the CPU's checksum of 000000-001FFF should match the committed image"
    );
}

/// The program rides twice the watchdog's timeout without being rebooted.
///
/// `RoadRunnerSystem::run_frame` resets the machine after 8 frames with no write
/// to `880001`. A reboot re-enters at the reset vector, which clears the result
/// block, so the count cannot creep past 8 unless every strobe landed: delete
/// `PetDog` from the vblank loop and this reads back somewhere below 8 with the
/// magic word absent, which is the failure the design doc records.
///
/// The frame count is asserted alongside it because the count alone would also
/// be satisfied by a poll loop that saw 16 edges inside one frame. One edge per
/// frame is the property; the total is only the symptom.
#[test]
fn the_program_outlives_the_watchdog() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_VBCOUNT),
        VB_TARGET,
        "vblank edges observed; the watchdog reboots at 8 frames, so anything \
         short of this means a strobe was missed"
    );
    assert_eq!(
        r.watchdog_ride_frames,
        Some(VB_TARGET as usize),
        "one vblank edge per frame: the program takes its first edge in the \
         frame it boots in and publishes phase 4 on the {VB_TARGET}th, so the \
         ride should cover exactly {VB_TARGET} frames. More means an edge was \
         missed, fewer means the edge detector is retriggering inside one blank."
    );
}

// ---------------------------------------------------------------------------
// The video signals the CPU can observe
//
// Every position below is a count of iterations of one shared poll loop, and
// every expectation is that count divided by iterations-per-line, which the
// program measures in the same run (T1's active dwell over 240 lines). There is
// no cycle count and no fitted constant anywhere in this section: a machine
// running at a different rate, or a poll loop of a different length, cancels.
// ---------------------------------------------------------------------------

/// Iterations of the shared poll loop per scanline, from T1's own calibration.
fn iters_per_line(r: &Run) -> f64 {
    let active = r.word(R_T1_ACTIVE) as f64;
    assert!(
        active > 100.0,
        "the calibration dwell is {active} iterations, far too short to divide by"
    );
    active / ACTIVE_LINES
}

/// The VBLANK level at `F60000` bit 4 blanks 22 of the frame's 262 lines.
///
/// `TIMING` gives 262 scanlines and `VBLANK_SCANLINE` is 240, so the display is
/// active for lines 0-239 and blanked for 240-261. The active dwell defines
/// iterations-per-line, so the *assertion* is on the blank: 22 lines of it.
///
/// The blank is measured twice, a frame apart, because a ratio that comes out
/// right once can come out right by accident. Both readings must agree.
#[test]
fn vblank_blanks_twenty_two_of_the_frames_lines() {
    let r = run();
    r.assert_completed();
    let ipl = iters_per_line(&r);

    let blank = r.word(R_T1_BLANK) as f64 / ipl;
    assert!(
        (blank - BLANK_LINES).abs() < 1.0,
        "VBLANK is asserted for {blank:.2} scanlines; 262 total less \
         VBLANK_SCANLINE 240 derives {BLANK_LINES}"
    );
    // The loop samples the beam asynchronously, so two readings of the same
    // interval can differ by the one sample that straddles its edge, and no
    // more. Requiring them to be identical was tried first and held only by
    // coincidence of the loop period at the time.
    let drift = r.word(R_T1_BLANK) as i32 - r.word(R_T1_BLANK2) as i32;
    assert!(
        drift.abs() <= 1,
        "the same blank measured a frame later read {} against {}, {drift} \
         samples apart. One sample of slack is the polling; more is the signal \
         moving.",
        r.word(R_T1_BLANK),
        r.word(R_T1_BLANK2)
    );
}

/// IRQ4 fires once a frame, during the blank, and is held until `8A0001` acks it.
///
/// Two frames that differ only in whether the handler acks on its first entry.
/// With an immediate ack the level drops and the handler runs once. With the ack
/// deferred by one entry the level is still asserted when RTE lowers the mask,
/// so the handler is re-entered at once and runs twice.
///
/// Both halves are needed. A count of 1 alone is also produced by an
/// edge-triggered interrupt that no ack could hold, and a count of 2 alone says
/// nothing about the ack working; it is the pair that pins `8A0001` as the thing
/// that clears the latch.
#[test]
fn the_vblank_interrupt_is_held_until_it_is_acked() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_T2_COUNT),
        1,
        "IRQ4 entries in one frame with an immediate ack"
    );
    assert_eq!(
        r.word(R_T2_HELD),
        2,
        "IRQ4 entries in one frame with the ack deferred by one entry. A count \
         of 1 here means the level is not held, so nothing needs 8A0001."
    );
    assert_eq!(
        r.word(R_T2_VB),
        1,
        "the VBLANK line should still be asserted inside the IRQ4 handler: the \
         interrupt is raised at scanline 240, which is the first blanked line"
    );
}

/// A motion-object timer entry raises IRQ3 at the scanline the program picks.
///
/// Entry 0 of the sprite list is flagged `$FFFF` in word 1, which makes it a
/// timer rather than a sprite. The band is
/// `(256 - (word0 >> 5) - vsize * 8 - 1) & 0x1FF`, so with `vsize` 1 the program
/// writes `(247 - line) << 5` and names the line directly. The assertion is
/// against that line, not against what the board computes.
///
/// Measured from the vblank edge at scanline 240, so a timer at line L is
/// `22 + L` lines away.
///
/// **Two placements, and that is the point.** One placement is satisfied by an
/// interrupt hard-wired to a fixed scanline that never reads the list at all,
/// which is exactly the wrong thing to pin. The second is 96 lines away and the
/// interrupt has to move the whole way with it.
#[test]
fn a_motion_object_timer_places_irq3_on_the_scanline_it_names() {
    let r = run();
    r.assert_completed();
    let ipl = iters_per_line(&r);

    let a = r.word(R_T3_POLL_A) as f64 / ipl;
    let b = r.word(R_T3_POLL_B) as f64 / ipl;
    assert!(
        (a - (BLANK_LINES + TIMER_LINE_A)).abs() < 1.0,
        "the timer named line {TIMER_LINE_A}, which is {} lines past the vblank \
         edge, but IRQ3 asserted {a:.2} lines past it",
        BLANK_LINES + TIMER_LINE_A
    );
    assert!(
        (b - (BLANK_LINES + TIMER_LINE_B)).abs() < 1.0,
        "the timer named line {TIMER_LINE_B}, which is {} lines past the vblank \
         edge, but IRQ3 asserted {b:.2} lines past it",
        BLANK_LINES + TIMER_LINE_B
    );
    assert!(
        (b - a - (TIMER_LINE_B - TIMER_LINE_A)).abs() < 1.0,
        "moving the timer entry {} lines down moved IRQ3 by {:.2} lines. An \
         interrupt anchored to a fixed scanline would not move at all.",
        TIMER_LINE_B - TIMER_LINE_A,
        b - a
    );
}

/// IRQ3 is a pulse one scanline wide, not a level that latches.
///
/// Measured as the span between the poll loop first seeing `2E0000` bit 7 set
/// and first seeing it clear again. It reads a little **under** one line, and
/// systematically so: the `rts`/`bsr` between the two loops is time that passes
/// without the counter advancing, worth about one and a half iterations out of
/// the eleven a line contains. The band is wide enough to absorb that and still
/// separate one line from two, and from the 240 a latched level would give.
#[test]
fn the_scanline_interrupt_lasts_one_scanline() {
    let r = run();
    r.assert_completed();
    let ipl = iters_per_line(&r);
    let width = (r.word(R_T3_END_A) - r.word(R_T3_POLL_A)) as f64 / ipl;
    assert!(
        (0.5..1.5).contains(&width),
        "IRQ3 stayed asserted for {width:.2} scanlines. A latched level would \
         read as the rest of the frame; a width near zero would mean the poll \
         loop is outrunning the pulse."
    );
}

/// The interrupt path and the poll path see the same pulse in the same place.
///
/// The two cannot be measured in one frame. IRQ3 is a level held for a scanline
/// that nothing acknowledges, so RTE lowers the mask back into a still-asserted
/// interrupt and the handler is re-entered before the interrupted loop advances
/// a single iteration; with the mask down the poll loop gets **no** iterations
/// during the one line it is trying to observe. So the program takes a frame
/// each, and agreeing across the two is the check that the level at `2E0000`
/// bit 7 and the autovector at level 3 are one signal rather than two.
///
/// The entry count is asserted only as "more than one", which is what a level
/// with no ack and a handler far shorter than a scanline has to produce, and is
/// the same distinction IRQ4's count of exactly 1 makes from the other side. Its
/// exact value is a function of this handler's length and is not a property of
/// the board, so it is not pinned.
#[test]
fn irq3_arrives_where_the_status_bit_says_it_does() {
    let r = run();
    r.assert_completed();
    let ipl = iters_per_line(&r);

    assert!(
        r.word(R_T4_CNT) > 1,
        "the IRQ3 handler was entered {} times in a frame. Nothing acks IRQ3, so \
         a level held for a whole scanline must re-enter a handler this short.",
        r.word(R_T4_CNT)
    );
    let irq = r.word(R_T4_FIRST) as f64 / ipl;
    let poll = r.word(R_T3_POLL_A) as f64 / ipl;
    assert!(
        (irq - poll).abs() < 1.0,
        "the level at 2E0000 asserted {poll:.2} lines past the vblank edge and \
         the level-3 autovector was taken at {irq:.2}. They are supposed to be \
         the same signal."
    );
}

/// The display list is read live, so a timer written mid-frame fires that frame.
///
/// `timer_irq_at_scanline` walks the live sprite RAM while the compositor
/// renders from `mo_shadow`, a copy taken at the start of vblank. So a timer
/// entry written part way down a frame moves the interrupt in that frame and
/// the picture only in the next one. This is the interrupt half of that
/// asymmetry; the picture half needs a framebuffer.
///
/// No timing constant is involved in placing the write: the line-64 interrupt is
/// itself the trigger. When it arrives, entry 1 becomes a timer at line 160, and
/// the second assertion has to appear 96 lines later in the same frame, at the
/// same place a timer installed before the frame started would have put it.
#[test]
fn a_timer_written_mid_frame_moves_the_interrupt_in_that_frame() {
    let r = run();
    r.assert_completed();
    let ipl = iters_per_line(&r);
    let live = r.word(R_T5_POLL) as f64 / ipl;
    assert!(
        (live - (BLANK_LINES + TIMER_LINE_B)).abs() < 1.0,
        "a timer entry installed at line {TIMER_LINE_A} of this frame, naming \
         line {TIMER_LINE_B}, fired {live:.2} lines past the vblank edge; the \
         list is read live, so it should fire at {}",
        BLANK_LINES + TIMER_LINE_B
    );
    let pre = r.word(R_T3_POLL_B) as f64 / ipl;
    assert!(
        (live - pre).abs() < 1.0,
        "the same line reached {pre:.2} lines in when the timer was installed \
         before the frame and {live:.2} when installed during it. A list latched \
         at the frame boundary would put the mid-frame one a whole frame later."
    );
}

// ---------------------------------------------------------------------------
// The picture
//
// Colours are IRGB-4444 at full intensity, which `irgb4444_to_rgb` resolves as
// (15 * 255) >> 8 = 254 rather than 255.
// ---------------------------------------------------------------------------

const RED: (u8, u8, u8) = (254, 0, 0);
const GREEN: (u8, u8, u8) = (0, 254, 0);
const BLUE: (u8, u8, u8) = (0, 0, 254);
const WHITE: (u8, u8, u8) = (254, 254, 254);

/// A screen column clear of the text and the sprite, inside the playfield cell
/// column the T7 writes land in.
const PROBE_X: usize = 34;
/// Screen row of the T7 cell that is above the beam at the moment of the write.
const T7_ABOVE_Y: usize = 50;
/// ... and of the one below it.
const T7_BELOW_Y: usize = 202;

fn shot(r: &Run, phase: u16) -> &Shot {
    r.shots[(phase - FIRST_SHOT_PHASE) as usize]
        .as_ref()
        .unwrap_or_else(|| panic!("no frame was captured at phase {phase}"))
}

/// All three layers composite, on a board with no arcade ROMs.
///
/// This is the assertion that the picture the two tests below are about is
/// actually being drawn, rather than the two of them agreeing about a black
/// screen. It also pins one thing from each layer's own path: the playfield's
/// PROM lookup and palette bank, the motion object's link walk and its per-row
/// tile stepping, and the alpha layer's glyph decode and transparent pen 0.
#[test]
fn the_program_draws_a_picture_through_all_three_layers() {
    let r = run();
    r.assert_completed();
    let s = shot(&r, 11);

    assert_eq!(
        s.pixel(PROBE_X, 100),
        RED,
        "the playfield is filled with tile 1, which is solid pen 1, and pen 1 is \
         red. Black here means the synthetic tile ROM decoded to pen 0."
    );

    // The sprite is four tiles tall at (160, 80), stepping tile codes 1 to 4, so
    // each 8-row band is a different pen and a different palette entry.
    for (y, expected, band) in [(84, WHITE, 1), (92, BLUE, 2), (100, GREEN, 3)] {
        assert_eq!(
            s.pixel(163, y),
            expected,
            "screen row {y} is the sprite's tile-{band} band"
        );
    }

    // 'R' of the text at alpha cell (2, 8): screen row 16 is its top bar,
    // ".####...", so x 64 is background showing through and 65 is the glyph.
    assert_eq!(
        s.pixel(65, 16),
        WHITE,
        "the top bar of the first glyph should be alpha pen 1"
    );
    assert_eq!(
        s.pixel(64, 16),
        RED,
        "pen 0 of a glyph is transparent, so the playfield shows through it"
    );
}

// ---------------------------------------------------------------------------
// The two assertions that are supposed to fail, and the ratchet holding them
// ---------------------------------------------------------------------------

/// **These two tests assert behaviour that is WRONG, on purpose.**
///
/// Road Runner is one of the nine render-once machines in
/// `docs/designs/raster-sampling-fidelity.md`: `AtariSystem1Board::render`
/// composites the whole frame at the frame boundary out of whatever state the
/// board holds at that moment. So a palette entry changed part way down a frame
/// recolours the entire picture, and a playfield cell written part way down a
/// frame appears everywhere at once, including in rows the beam drew long before
/// the write.
///
/// Hardware does neither. The rows already scanned out keep the old colour and
/// the old contents; only the rows below the beam take the change. That is the
/// whole point of `raster-sampling-fidelity.md` W3
/// (`phosphor-emulator-raster-sampling-6kae.3`), which has no acceptance test
/// today beyond "golden frames unchanged" -- and a golden frame cannot tell a
/// timing fix from an attract-loop shift.
///
/// **This is that acceptance test, written in advance, in the machine's own
/// instruction stream.** It is held as a ratchet rather than `#[ignore]`d: each
/// test asserts the *defect* is still present, and states in its failure message
/// what the correct answer is and that the fix is to delete the entry here. So
/// the suite is green today, W3 turns it red the day it lands, and the only way
/// to make it green again is to write the correct expectation.
///
/// The alternative shapes were considered and rejected. `#[ignore]` makes a test
/// invisible, and `phosphor-emulator-gn5w` is what happens when nobody comes back
/// for one. Asserting the current behaviour with only a comment saying it is
/// wrong leaves nothing that fires when it stops being wrong.
///
/// **Do not delete a failing assertion here to make the suite green.** If one of
/// these fails, either W3 landed, in which case follow the message, or something
/// else changed what this board draws, in which case that is the finding.
const KNOWN_DEFECTS: &str = "phosphor-emulator-raster-sampling-6kae.3";

/// T6, the palette split. **Held as a known defect**, see [`KNOWN_DEFECTS`].
///
/// The playfield is uniformly pen 1 and pen 1 is red. At scanline 120, named by
/// the motion-object timer interrupt rather than by counting cycles, pen 1
/// becomes green.
///
/// Correct: rows above the write stay red, rows below come out green, one
/// transition, within a row or two of the line the interrupt named.
/// What this board does: the palette is read once at the frame boundary, so
/// every row is green and no red survives anywhere.
#[test]
fn a_mid_frame_palette_write_recolours_the_whole_frame() {
    let r = run();
    r.assert_completed();
    let s = shot(&r, 12);

    let reds = (0..240).filter(|&y| s.pixel(PROBE_X, y) == RED).count();
    let greens = (0..240).filter(|&y| s.pixel(PROBE_X, y) == GREEN).count();

    // Ordered so the diagnosis is right in both directions. No green at all
    // means the write never happened, which is a broken fixture and not a
    // rendering model; some green and some red means the beam is being tracked,
    // which is the ratchet firing.
    assert!(
        greens > 0,
        "no row is green, so the mid-frame palette write never landed at all. \
         That is a broken fixture rather than a rendering-model question."
    );
    assert_eq!(
        reds,
        0,
        "KNOWN DEFECT ({KNOWN_DEFECTS}): {reds} rows kept the red they were drawn \
         with and {greens} came out green, so this board is now sampling the \
         palette as the beam passes. THAT IS THE CORRECT BEHAVIOUR and this \
         assertion is the acceptance test for it. Replace this test with the real \
         expectation: rows 0 to about {} red, rows about {} to 239 green, exactly \
         one transition. Do not delete it.",
        TIMER_LINE_C - 1,
        TIMER_LINE_C + 1
    );
    assert_eq!(
        greens, 240,
        "under whole-frame rendering every row takes the new colour; {greens} did"
    );
}

/// T7, the beam has already passed. **Held as a known defect**, see
/// [`KNOWN_DEFECTS`].
///
/// At scanline 120, again named by the timer interrupt, two playfield cells
/// become pen 2, which is green: one covering screen rows 48-55, drawn long
/// before, and one covering 200-207, not yet reached.
///
/// Correct: in the frame of the write only the lower row changes; on the next
/// frame both have. Get the render order wrong in either direction and one of
/// the two captures is wrong, which is what makes this discriminate rather than
/// merely observe.
/// What this board does: both change in the frame of the write.
#[test]
fn a_mid_frame_playfield_write_changes_rows_the_beam_already_drew() {
    let r = run();
    r.assert_completed();

    let during = shot(&r, 13);
    assert_eq!(
        during.pixel(PROBE_X, T7_BELOW_Y),
        GREEN,
        "screen row {T7_BELOW_Y} was written before the beam reached it, so it \
         must show the change on this frame under any rendering model. Red here \
         means the write never landed."
    );
    assert_eq!(
        during.pixel(PROBE_X, T7_ABOVE_Y),
        GREEN,
        "KNOWN DEFECT ({KNOWN_DEFECTS}): screen row {T7_ABOVE_Y} is RED, which \
         means the beam drew it before the write and the frame kept what it drew. \
         THAT IS THE CORRECT BEHAVIOUR and this assertion is the acceptance test \
         for it. Replace this test with the real expectation: row {T7_ABOVE_Y} \
         stays red in this capture and is green in the next one. Do not delete it."
    );

    // True under either model, and the reason it is here: it is what separates
    // "the write landed late" from "the write never landed at all".
    let after = shot(&r, 14);
    for y in [T7_ABOVE_Y, T7_BELOW_Y] {
        assert_eq!(
            after.pixel(PROBE_X, y),
            GREEN,
            "on the frame after the write, screen row {y} must show it"
        );
    }
}

// ---------------------------------------------------------------------------
// The committed binary cannot drift from its source
// ---------------------------------------------------------------------------

/// Re-assemble `roadrunner_video.asm` and compare against the committed binary.
///
/// The committed image is the one artifact here that no reviewer can check by
/// reading, so this is the only thing standing between an edited source and a
/// stale binary. It therefore must not be allowed to pass by doing nothing:
/// `PHOSPHOR_ASM` is exported by the Nix dev shell, and when it is set a missing
/// assembler is a failure rather than a skip. CI has no dev shell, sets nothing,
/// and skips with a printed note.
///
/// The Williams guard this is copied from reported green for its entire life
/// because `asl` was on `PATH` nowhere, which is why the trap exists and why it
/// is re-demonstrated rather than inherited on faith.
#[test]
fn the_committed_binary_matches_its_source() {
    use std::path::Path;
    use std::process::Command;

    let roms = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/roms");
    let asm = roms.join("roadrunner_video.asm");
    let tmp = std::env::temp_dir();
    let code = tmp.join("phosphor_roadrunner_video_check.p");
    let out = tmp.join("phosphor_roadrunner_video_check.bin");
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
        "tests/roms/roadrunner_video.bin is stale. Rebuild it with\n  \
         asl -q -o out.p roadrunner_video.asm\n  \
         p2bin out.p roadrunner_video.bin -r {range} -l 0xA5"
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

/// The image is exactly the `p2bin -r` window, with both reset vectors in it.
///
/// A wrong window produces a short file rather than a wrong one, and a wrong
/// origin produces a vector pointing outside the image; neither is visible by
/// reading the binary.
#[test]
fn the_image_carries_its_reset_vectors() {
    assert_eq!(
        PROGRAM.len(),
        IMAGE_LEN,
        "expected an 8 KB 000000-001FFF image"
    );
    let long = |o: usize| u32::from_be_bytes(PROGRAM[o..o + 4].try_into().unwrap());
    assert_eq!(
        long(0),
        STACK_TOP,
        "vector 0 is the supervisor stack pointer"
    );
    let entry = long(4);
    assert!(
        (0x400..IMAGE_LEN as u32).contains(&entry),
        "vector 1 should point at code inside the image, past the 1 KB vector \
         table, but it is {entry:#08X}"
    );
}
