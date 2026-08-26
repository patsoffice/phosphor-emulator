//! Williams gen-1 video timing conformance, driven by a synthetic program ROM.
//!
//! Design: `docs/designs/williams-video-conformance.md`.
//!
//! Unlike every other test that asks what a machine draws, this one needs **no
//! arcade ROMs** and therefore runs in CI. It builds a machine with
//! `MachineEntry::create_bare`, pokes an assembled M6809 test program into the
//! program-ROM region through `BusDebug::write` (`AddressSpace16::debug_write`
//! ignores `AccessKind`, so a `ReadOnly` region takes the write), resets, and
//! lets the CPU and the beam run against each other.
//!
//! What that buys over the existing tests: the picture a Williams board draws is
//! not a function of state, it is an artifact of *when* the CPU writes relative
//! to where the beam is. `begin_scanline` renders line N before the CPU runs
//! line N's cycles, and the ROM PIA's interrupts are driven off the scanline
//! counter. Nothing else in the suite observes either half.
//!
//! The program publishes its verdict as bytes in undisplayed video RAM. `$B000`
//! holds `$5A` only once every phase completed, so a zero-filled result block
//! cannot read as a pass.

use phosphor_core::core::machine::FrontendMachine;
use phosphor_machines::registry;

/// The assembled test program, `$D000-$FFFF` inclusive.
///
/// Built from `tests/roms/williams_video.asm` with `asl` and `p2bin`, both in
/// the Nix dev shell; the exact commands are at the top of the source and in
/// [`the_committed_binary_matches_its_source`], which re-assembles and compares
/// so the two cannot drift.
const PROGRAM: &[u8] = include_bytes!("roms/williams_video.bin");
const LOAD_ADDR: u32 = 0xD000;

/// Frames to run before giving up on the program reaching its end. The script
/// spends roughly one frame per phase plus three for the screen fill; 64 is a
/// wide margin that still fails fast when the CPU wedges.
const MAX_FRAMES: usize = 64;

// --- Result block, mirroring the equates in the assembly --------------------

const RES: u32 = 0xB000;
const R_MAGIC: u32 = RES;
const R_PHASE: u32 = RES + 1;
const R_T1TRN: u32 = RES + 2;
const R_T1WRP: u32 = RES + 3;
const R_T1MAX: u32 = RES + 4;
const R_T1DW0: u32 = RES + 5;
const R_T1DW4: u32 = RES + 6;
const R_T2CNT: u32 = RES + 7;
const R_T2LIN: u32 = RES + 8;
const R_T3RCNT: u32 = RES + 9;
const R_T3RLIN: u32 = RES + 10;
const R_T3FCNT: u32 = RES + 14;
const R_T3FLIN: u32 = RES + 15;
const R_T4FST: u32 = RES + 18;
/// The ROM measures the slow blit into this byte, but nothing asserts it yet:
/// the board charges one cycle a byte for `CTRL_SLOW` instead of two, so the
/// assertion lands with the fix rather than pinning the bug.
/// See `the_blitter_halts_the_cpu_for_the_cycles_it_charges`.
#[allow(dead_code)]
const R_T4SLW: u32 = RES + 19;
const R_T5A: u32 = RES + 20;
const R_T5B: u32 = RES + 21;

const MAGIC: u8 = 0x5A;
const FINAL_PHASE: u8 = 10;

// --- Screen geometry, derived from williams.rs ------------------------------

/// `CROP_Y` in `WilliamsBoard::render_scanline`: screen row 0 is scanline 7.
const CROP_Y: usize = 7;
/// `FIRST_COL` = `CROP_X / 2`: screen x 0 is the high nibble of VRAM column 3.
const FIRST_COL: usize = 3;

const RED: (u8, u8, u8) = (255, 0, 0);
const GREEN: (u8, u8, u8) = (0, 255, 0);

/// The VRAM column T7 writes into, and the two screen x positions its two
/// pixels land on.
const T7_COL: usize = 80;
/// VRAM row 60 → scanline 60 → screen row 53. Above the beam when written.
const T7_ROW_ABOVE: usize = 60;
/// VRAM row 200 → scanline 200 → screen row 193. Below the beam when written.
const T7_ROW_BELOW: usize = 200;

// --- Harness ----------------------------------------------------------------

struct Run {
    results: Vec<u8>,
    /// Frames captured at phases 7, 8 and 9.
    shots: [Option<Shot>; 3],
    frames: usize,
}

struct Shot {
    w: usize,
    #[allow(dead_code)]
    h: usize,
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
        .unwrap_or_else(|| panic!("{addr:#06X} is not readable through the debug bus"))
}

fn render(m: &mut dyn FrontendMachine) -> Shot {
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    Shot {
        w: w as usize,
        h: h as usize,
        rgb,
    }
}

fn run(machine: &str) -> Run {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    let mut m = (entry.create_bare)();

    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{machine} exposes no debug bus"));
        for (i, b) in PROGRAM.iter().enumerate() {
            bus.write(0, LOAD_ADDR + i as u32, *b);
        }
    }
    // The M6809 fetches its reset vector through the bus, so this picks up the
    // vector the program just installed at $FFFE.
    m.reset();

    let mut shots: [Option<Shot>; 3] = [None, None, None];
    let mut frames = 0;
    for _ in 0..MAX_FRAMES {
        m.run_frame();
        frames += 1;
        let phase = peek(&*m, R_PHASE);
        // Each picture phase is published at line 240 of the frame it describes
        // and is the last thing written in that frame, so the frame that just
        // ended is the one to capture. The program holds phase 9 for a whole
        // frame before publishing 10 for exactly this reason; without that wait
        // the idle frame is only ever observed as phase 10 and capture C is
        // never taken.
        if (7..=9).contains(&phase) {
            let slot = (phase - 7) as usize;
            if shots[slot].is_none() {
                shots[slot] = Some(render(&mut *m));
            }
        }
        if peek(&*m, R_MAGIC) == MAGIC {
            break;
        }
    }

    let results = (0..32u32).map(|i| peek(&*m, RES + i)).collect();
    Run {
        results,
        shots,
        frames,
    }
}

impl Run {
    fn at(&self, addr: u32) -> u8 {
        self.results[(addr - RES) as usize]
    }
    fn slice(&self, addr: u32, len: usize) -> &[u8] {
        let o = (addr - RES) as usize;
        &self.results[o..o + len]
    }
    /// Fail loudly and early if the program never finished — otherwise every
    /// assertion below would be reading zeros and reporting them as answers.
    fn assert_completed(&self, machine: &str) {
        assert_eq!(
            self.at(R_MAGIC),
            MAGIC,
            "{machine}: the conformance program did not finish in {} frames \
             (reached phase {}, expected {FINAL_PHASE}). A zero result block is \
             a wedge, not a pass.",
            self.frames,
            self.at(R_PHASE)
        );
        assert_eq!(self.at(R_PHASE), FINAL_PHASE, "{machine}: phase mismatch");
    }
}

// ---------------------------------------------------------------------------
// The signals the CPU can observe
// ---------------------------------------------------------------------------

/// The program runs to completion on a machine with no arcade ROMs at all.
///
/// The precondition for everything below, worth failing on its own: a program
/// that wedges would otherwise surface as a dozen unrelated assertions about
/// zero bytes.
#[test]
fn the_conformance_program_runs_to_completion() {
    for machine in ["joust", "robotron"] {
        run(machine).assert_completed(machine);
    }
}

/// The video counter at `$CB00` steps by four, wraps once a frame, and tops out
/// at 252.
///
/// `T1_DWELL0` against `T1_DWELL4` measures the `u8` aliasing:
/// `current_scanline()` casts to `u8`, so scanlines 256-259 read back as 0-3 and
/// the value 0 occupies eight lines against every other value's four. Both poll
/// loops are 16 cycles, so the ratio is meaningful.
#[test]
fn the_video_counter_steps_by_four_and_wraps_once_a_frame() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(
            r.at(R_T1TRN),
            64,
            "{machine}: counter transitions per frame"
        );
        assert_eq!(r.at(R_T1WRP), 1, "{machine}: counter wraps per frame");
        assert_eq!(r.at(R_T1MAX), 0xFC, "{machine}: highest counter value");

        let (dwell0, dwell4) = (r.at(R_T1DW0) as u32, r.at(R_T1DW4) as u32);
        assert!(
            dwell4 > 4,
            "{machine}: dwell reference too small to compare"
        );
        assert!(
            dwell0 * 10 > dwell4 * 17 && dwell0 * 10 < dwell4 * 23,
            "{machine}: the counter dwells {dwell0} at value 0 against {dwell4} \
             at value 4, a ratio of {:.2}. Expected about 2: current_scanline() \
             is a u8, so scanlines 256-259 alias onto 0-3.",
            dwell0 as f32 / dwell4 as f32
        );
    }
}

/// count240 reaches the ROM PIA's CA1 once a frame, at scanline 240.
#[test]
fn count240_raises_one_interrupt_a_frame_at_scanline_240() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(r.at(R_T2CNT), 1, "{machine}: CA1 interrupts per frame");
        assert_eq!(
            r.at(R_T2LIN),
            0xF0,
            "{machine}: counter read inside the CA1 handler"
        );
    }
}

/// VA11 (scanline bit 5) reaches the ROM PIA's CB1 at every 32-line boundary.
///
/// Measured over [line 16, line 240): four rising edges at 32, 96, 160 and 224,
/// three falling at 64, 128 and 192.
#[test]
fn va11_toggles_cb1_every_thirty_two_scanlines() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(r.at(R_T3RCNT), 4, "{machine}: CB1 rising edges");
        assert_eq!(
            r.slice(R_T3RLIN, 4),
            &[0x20, 0x60, 0xA0, 0xE0],
            "{machine}: CB1 rising edge scanlines"
        );
        assert_eq!(r.at(R_T3FCNT), 3, "{machine}: CB1 falling edges");
        assert_eq!(
            r.slice(R_T3FLIN, 3),
            &[0x40, 0x80, 0xC0],
            "{machine}: CB1 falling edge scanlines"
        );
    }
}

/// A blit halts the CPU for one cycle per byte.
///
/// 8 × 128 = 1024 bytes, so 1024 cycles, which is 16 scanlines and a counter
/// delta of `$10`.
///
/// The slow half of T4 is deliberately not asserted here. The ROM measures it
/// into `R_T4SLW` and the board gets it wrong: `CTRL_SLOW` should cost two
/// cycles a byte and costs one, because `do_dma_cycle` returns the cost and
/// `williams.rs` discards it. Asserting `$20` would land a failing test and
/// asserting `$10` would pin the bug, so the assertion arrives with the fix.
/// See `phosphor-emulator-williams-video-conformance-itvk.4`.
#[test]
fn the_blitter_halts_the_cpu_for_the_cycles_it_charges() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(
            r.at(R_T4FST),
            0x10,
            "{machine}: a 1024-byte fast blit should halt the CPU for 1024 \
             cycles, i.e. 16 scanlines"
        );
    }
}

/// The SC1's XOR-4 size bug: width 4 and height 4 become 0, clamp to 1, and
/// blit a single byte.
#[test]
fn the_sc1_xors_four_into_the_blit_size() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(
            r.at(R_T5A),
            0xEE,
            "{machine}: the 4x4 blit wrote nothing at all"
        );
        assert_eq!(
            r.at(R_T5B),
            0x00,
            "{machine}: the 4x4 blit covered more than one byte, so the SC1 \
             size XOR was not applied"
        );
    }
}

// ---------------------------------------------------------------------------
// The interleave, against the framebuffer
// ---------------------------------------------------------------------------

fn shot<'a>(r: &'a Run, phase: u8, machine: &str) -> &'a Shot {
    r.shots[(phase - 7) as usize]
        .as_ref()
        .unwrap_or_else(|| panic!("{machine}: no frame captured at phase {phase}"))
}

/// Screen x positions of the two pixels in VRAM column `col`.
fn column_x(col: usize) -> (usize, usize) {
    let x = (col - FIRST_COL) * 2;
    (x, x + 1)
}

/// A palette write part way down the frame splits the picture at the beam.
///
/// The whole displayed area is pen 1. At scanline ~120 a single store changes
/// pen 1 from red to green. Rows already scanned out keep the old colour; rows
/// below take the new one. The counter's four-line resolution puts the store in
/// lines 120-123 and the boundary, one line later, in screen rows 114-117.
#[test]
fn a_mid_frame_palette_write_splits_the_picture_at_the_beam() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        let s = shot(&r, 7, machine);
        let (x, _) = column_x(T7_COL);

        for y in 0..=(120 - CROP_Y - 1) {
            assert_eq!(
                s.pixel(x, y),
                RED,
                "{machine}: screen row {y} is above the mid-frame palette write \
                 and should still be red"
            );
        }
        for y in (124 - CROP_Y + 1)..240 {
            assert_eq!(
                s.pixel(x, y),
                GREEN,
                "{machine}: screen row {y} is below the mid-frame palette write \
                 and should be green"
            );
        }
    }
}

/// The load-bearing test: the beam has already drawn the rows above it.
///
/// At scanline ~120 the program writes pen 2 into VRAM at rows 60 and 200 of one
/// column. Row 200 has not been scanned out yet, so it must change on this
/// frame; row 60 was drawn at scanline 60 and must not. On the following frame,
/// with no further writes, both must show pen 2.
///
/// Render the whole frame at end-of-frame and the first capture shows both rows
/// green. Render it at start-of-frame and it shows neither. Only a per-scanline
/// renderer that samples VRAM as the beam reaches each line produces this pair.
#[test]
fn a_mid_frame_vram_write_only_affects_rows_the_beam_has_not_reached() {
    for machine in ["joust", "robotron"] {
        let r = run(machine);
        r.assert_completed(machine);
        let (x0, x1) = column_x(T7_COL);
        let above = T7_ROW_ABOVE - CROP_Y;
        let below = T7_ROW_BELOW - CROP_Y;

        let during = shot(&r, 8, machine);
        for x in [x0, x1] {
            assert_eq!(
                during.pixel(x, below),
                GREEN,
                "{machine}: VRAM row {T7_ROW_BELOW} was written at scanline ~120, \
                 before the beam reached it, so screen row {below} must show it \
                 on this frame"
            );
            assert_eq!(
                during.pixel(x, above),
                RED,
                "{machine}: VRAM row {T7_ROW_ABOVE} was written at scanline ~120, \
                 long after the beam drew scanline {T7_ROW_ABOVE}, so screen row \
                 {above} must NOT change until the next frame"
            );
        }

        let after = shot(&r, 9, machine);
        for x in [x0, x1] {
            assert_eq!(
                after.pixel(x, above),
                GREEN,
                "{machine}: on the frame after the write, screen row {above} must \
                 finally show it"
            );
            assert_eq!(after.pixel(x, below), GREEN, "{machine}: row {below} again");
        }
    }
}

// ---------------------------------------------------------------------------
// The committed binary cannot drift from its source
// ---------------------------------------------------------------------------

/// Re-assemble `williams_video.asm` and compare against the committed binary.
///
/// The committed image is the one artifact here that no reviewer can check by
/// reading, so this is the only thing standing between an edited source and a
/// stale binary. It therefore must not be allowed to pass by doing nothing:
/// `PHOSPHOR_ASM` is exported by the Nix dev shell, and when it is set a missing
/// assembler is a failure rather than a skip. CI has no dev shell, sets nothing,
/// and skips with a printed note.
#[test]
fn the_committed_binary_matches_its_source() {
    use std::path::Path;
    use std::process::Command;

    let roms = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/roms");
    let asm = roms.join("williams_video.asm");
    let tmp = std::env::temp_dir();
    let code = tmp.join("phosphor_williams_video_check.p");
    let out = tmp.join("phosphor_williams_video_check.bin");
    // asl appends to an existing code file rather than truncating it, so a
    // leftover from an earlier run would be re-read by p2bin.
    let _ = std::fs::remove_file(&code);

    let expected = std::env::var_os("PHOSPHOR_ASM").is_some();
    let assembled = Command::new("asl")
        .args(["-q", "-o"])
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

    let converted = Command::new("p2bin")
        .arg(&code)
        .arg(&out)
        .args(["-r", "0xD000-0xFFFF", "-l", "0x00"])
        .status()
        .expect("p2bin runs when asl did");
    assert!(converted.success(), "p2bin failed on {}", code.display());

    let built = std::fs::read(&out).expect("read re-assembled image");
    let _ = std::fs::remove_file(&code);
    let _ = std::fs::remove_file(&out);

    let stale = "tests/roms/williams_video.bin is stale. Rebuild it with\n  \
                 asl -q -o williams_video.p williams_video.asm\n  \
                 p2bin williams_video.p williams_video.bin -r 0xD000-0xFFFF -l 0x00";
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
                "first difference at ${:04X}: built {:#04X}, committed {:#04X}",
                LOAD_ADDR as usize + i,
                built[i],
                PROGRAM[i]
            )
        });
    assert!(
        differs.is_none(),
        "{}. {stale}",
        differs.unwrap_or_default()
    );
}

/// The image is exactly the $D000-$FFFF program-ROM window, so loading it is a
/// flat copy and the vectors land where the CPU looks for them.
#[test]
fn the_image_fills_the_program_rom_window() {
    assert_eq!(PROGRAM.len(), 0x3000, "expected a 12 KB $D000-$FFFF image");
    let reset = u16::from_be_bytes([PROGRAM[0x2FFE], PROGRAM[0x2FFF]]);
    assert_eq!(
        reset, 0xD000,
        "the reset vector should point at the start of the image"
    );
}
