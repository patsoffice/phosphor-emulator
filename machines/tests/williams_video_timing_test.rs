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
const PROGRAM_D000: &[u8] = include_bytes!("roms/williams_video.bin");
/// The same program linked at `$E000`, for the board that shrinks program ROM to
/// 8 KB and puts work RAM at `$D000` instead.
const PROGRAM_E000: &[u8] = include_bytes!("roms/williams_video_e000.bin");

/// Every machine on `WilliamsBoard`, with the image its program-ROM window takes.
///
/// Sinistar differs only in where the ROM lives (`williams.rs:461-470`), so it
/// runs the same program from a second link address rather than a second source.
/// Its blitter also carries the window clip, which `$C900` bit 2 gates and the
/// program clears at startup along with the ROM bank.
const MACHINES: [&str; 3] = ["joust", "robotron", "sinistar"];

/// The image and load address for a machine's program-ROM window.
fn image_for(machine: &str) -> (&'static [u8], u32) {
    match machine {
        "sinistar" => (PROGRAM_E000, 0xE000),
        _ => (PROGRAM_D000, 0xD000),
    }
}

/// Whether the machine's `render_frame` rotates the board's raster.
///
/// Sinistar's cabinet stands the monitor on its side, so its `render_frame`
/// turns the landscape raster 270 degrees into a 240x292 portrait buffer
/// (`sinistar.rs:442`). Every assertion in this file is about the raster the
/// board draws, which is what the scanline and column arithmetic describes, so
/// the rotation is undone on read rather than being baked into the expected
/// coordinates. Getting this wrong is not subtle: it read row 0 as row 239 and
/// failed both picture tests in opposite directions.
fn is_rotated(machine: &str) -> bool {
    machine == "sinistar"
}

/// The board's raster width, before any cabinet rotation.
const RASTER_W: usize = 292;

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
    /// The frame came out of a `render_frame` that rotated the raster.
    rotated: bool,
}

impl Shot {
    fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.w + x) * 3;
        (self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }

    /// A pixel of the **board's raster**, whatever the cabinet does with it.
    ///
    /// Sinistar's rotation is `dst(dx, dy) = src(x = 291 - dy, y = dx)`, so
    /// reading raster `(x, y)` means asking for `dst(y, 291 - x)`. Every
    /// assertion here is written in raster coordinates because that is what the
    /// scanline and VRAM-column arithmetic is about; the cabinet's orientation
    /// is a separate concern with its own tests.
    fn raster(&self, x: usize, y: usize) -> (u8, u8, u8) {
        if self.rotated {
            self.pixel(y, RASTER_W - 1 - x)
        } else {
            self.pixel(x, y)
        }
    }
}

fn peek(m: &dyn FrontendMachine, addr: u32) -> u8 {
    m.debug_bus()
        .expect("machine exposes a debug bus")
        .read(0, addr)
        .unwrap_or_else(|| panic!("{addr:#06X} is not readable through the debug bus"))
}

fn render(m: &mut dyn FrontendMachine, rotated: bool) -> Shot {
    let (w, h) = m.display_size();
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    m.render_frame(&mut rgb);
    Shot {
        w: w as usize,
        h: h as usize,
        rgb,
        rotated,
    }
}

fn run(machine: &str) -> Run {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    let mut m = (entry.create_bare)();
    let rotated = is_rotated(machine);

    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{machine} exposes no debug bus"));
        let (program, load_addr) = image_for(machine);
        for (i, b) in program.iter().enumerate() {
            bus.write(0, load_addr + i as u32, *b);
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
                shots[slot] = Some(render(&mut *m, rotated));
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
    for machine in MACHINES {
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
    for machine in MACHINES {
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
    for machine in MACHINES {
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
    for machine in MACHINES {
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

/// A blit halts the CPU for one cycle per byte, or two with `CTRL_SLOW`.
///
/// 8 × 128 = 1024 bytes, so 1024 cycles fast (16 scanlines, counter delta
/// `$10`) and 2048 slow (32 scanlines, `$20`).
///
/// The slow half is the assertion this suite was worth writing for. It failed
/// on first run with `$10`, because `do_dma_cycle` returned the cost of the byte
/// it moved and `williams.rs` discarded it, so `CTRL_SLOW` cost nothing. The
/// device was right and the board was right; the join was wrong, which is the
/// shape of bug a device unit test cannot reach.
#[test]
fn the_blitter_halts_the_cpu_for_the_cycles_it_charges() {
    for machine in MACHINES {
        let r = run(machine);
        r.assert_completed(machine);
        assert_eq!(
            r.at(R_T4FST),
            0x10,
            "{machine}: a 1024-byte fast blit should halt the CPU for 1024 \
             cycles, i.e. 16 scanlines"
        );
        assert_eq!(
            r.at(R_T4SLW),
            0x20,
            "{machine}: a 1024-byte slow blit should halt the CPU for 2048 \
             cycles, i.e. 32 scanlines. Equal to the fast figure means the \
             stall clock is not being charged."
        );
    }
}

/// The SC1's XOR-4 size bug: width 4 and height 4 become 0, clamp to 1, and
/// blit a single byte.
#[test]
fn the_sc1_xors_four_into_the_blit_size() {
    for machine in MACHINES {
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
    for machine in MACHINES {
        let r = run(machine);
        r.assert_completed(machine);
        let s = shot(&r, 7, machine);
        let (x, _) = column_x(T7_COL);

        for y in 0..=(120 - CROP_Y - 1) {
            assert_eq!(
                s.raster(x, y),
                RED,
                "{machine}: screen row {y} is above the mid-frame palette write \
                 and should still be red"
            );
        }
        for y in (124 - CROP_Y + 1)..240 {
            assert_eq!(
                s.raster(x, y),
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
    for machine in MACHINES {
        let r = run(machine);
        r.assert_completed(machine);
        let (x0, x1) = column_x(T7_COL);
        let above = T7_ROW_ABOVE - CROP_Y;
        let below = T7_ROW_BELOW - CROP_Y;

        let during = shot(&r, 8, machine);
        for x in [x0, x1] {
            assert_eq!(
                during.raster(x, below),
                GREEN,
                "{machine}: VRAM row {T7_ROW_BELOW} was written at scanline ~120, \
                 before the beam reached it, so screen row {below} must show it \
                 on this frame"
            );
            assert_eq!(
                during.raster(x, above),
                RED,
                "{machine}: VRAM row {T7_ROW_ABOVE} was written at scanline ~120, \
                 long after the beam drew scanline {T7_ROW_ABOVE}, so screen row \
                 {above} must NOT change until the next frame"
            );
        }

        let after = shot(&r, 9, machine);
        for x in [x0, x1] {
            assert_eq!(
                after.raster(x, above),
                GREEN,
                "{machine}: on the frame after the write, screen row {above} must \
                 finally show it"
            );
            assert_eq!(
                after.raster(x, below),
                GREEN,
                "{machine}: row {below} again"
            );
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

    // Both link addresses, because a source edit that only breaks one of them is
    // exactly what a single-image guard would miss.
    for (image, base, name, define) in [
        (PROGRAM_D000, 0xD000u32, "williams_video.bin", None),
        (
            PROGRAM_E000,
            0xE000,
            "williams_video_e000.bin",
            Some("ROMBASE=0xE000"),
        ),
    ] {
        let _ = std::fs::remove_file(&code);
        let mut asl = Command::new("asl");
        asl.arg("-q");
        if let Some(d) = define {
            asl.args(["-D", d]);
        }
        let assembled = asl.arg("-o").arg(&code).arg(&asm).status();
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

        let range = format!("0x{base:04X}-0xFFFF");
        let converted = Command::new("p2bin")
            .arg(&code)
            .arg(&out)
            .args(["-r", &range, "-l", "0x00"])
            .status()
            .expect("p2bin runs when asl did");
        assert!(converted.success(), "p2bin failed on {}", code.display());

        let built = std::fs::read(&out).expect("read re-assembled image");
        let _ = std::fs::remove_file(&code);
        let _ = std::fs::remove_file(&out);

        let define_arg = define.map(|d| format!("-D {d} ")).unwrap_or_default();
        let stale = format!(
            "tests/roms/{name} is stale. Rebuild it with\n  \
             asl -q {define_arg}-o out.p williams_video.asm\n  \
             p2bin out.p {name} -r {range} -l 0x00"
        );
        assert_eq!(
            built.len(),
            image.len(),
            "re-assembled image is {} bytes, committed is {}. {stale}",
            built.len(),
            image.len()
        );
        let differs = built.iter().zip(image).position(|(a, b)| a != b).map(|i| {
            format!(
                "first difference at ${:04X}: built {:#04X}, committed {:#04X}",
                base as usize + i,
                built[i],
                image[i]
            )
        });
        assert!(
            differs.is_none(),
            "{}. {stale}",
            differs.unwrap_or_default()
        );
    }
}

/// The image is exactly the $D000-$FFFF program-ROM window, so loading it is a
/// flat copy and the vectors land where the CPU looks for them.
#[test]
fn the_image_fills_the_program_rom_window() {
    for (image, base, size) in [
        (PROGRAM_D000, 0xD000u32, 0x3000usize),
        (PROGRAM_E000, 0xE000, 0x2000),
    ] {
        assert_eq!(
            image.len(),
            size,
            "expected a {} KB ${base:04X}-$FFFF image",
            size / 1024
        );
        // A wrong p2bin window produces a short file rather than a wrong one,
        // and a wrong link address produces a vector pointing outside the image.
        let reset = u16::from_be_bytes([image[size - 2], image[size - 1]]);
        assert_eq!(
            u32::from(reset),
            base,
            "the reset vector should point at the start of the image"
        );
    }
}

// ---------------------------------------------------------------------------
// The second opinion: the same binary under MAME
// ---------------------------------------------------------------------------

/// Every figure the ROM measures, ours against MAME's, on all three machines.
///
/// This is the sharper of the two conformance cross-checks in the tree and it is
/// worth saying why. The Road Runner ROM has no line counter, so every position
/// it reports is in poll-loop iterations divided by a rate it measures in the
/// same run, and a constant cycle difference between two cores cancels by
/// design. Williams has a video counter at `$CB00`, so these figures are already
/// absolute: 64 transitions, one wrap, a maximum of `$FC`, count240 at `$F0`,
/// VA11 edges at `$20/$60/$A0/$E0` and `$40/$80/$C0`, blitter halts of `$10` and
/// `$20` scanlines. Nothing is calibrated and nothing cancels, so every one of
/// them is compared exactly and a disagreement is a disagreement.
///
/// **What it confirmed.** `T4_SLOW` is the assertion this ROM was written for:
/// `williams.rs` discarded the cycle count `do_dma_cycle` returned, so a slow
/// RAM-to-RAM blit halted the CPU for 16 scanlines where the datasheet's 2
/// microseconds a byte derives 32. The fix moved robotron's and sinistar's
/// golden frames, which can say a picture changed but not that it changed to the
/// right thing. MAME independently reports `$20`, which is the first outside
/// evidence that the fix was right rather than merely different.
///
/// **The one disagreement, and it is not settled here.** `T1_DWELL0` against
/// `T1_DWELL4` measures how long the counter reads 0. We alias: `current_scanline()`
/// is a `u8`, so lines 256-259 read back as 0-3 and value 0 spans eight lines
/// against every other value's four. MAME saturates instead
/// (`williams_v.cpp video_counter_r`: `vpos() < 0x100 ? vpos() & 0xfc : 0xfc`),
/// so its value 0 spans four lines and `$FC` spans eight. Both frames are 260
/// lines, so this is entirely about what `$CB00` reads on lines 256-259.
///
/// **The schematic says ours is right**, which is why the divergence is pinned
/// rather than resolved by matching MAME. On R-8731 CPU board sheet 1 the
/// `$CB00` readback is `3B`, an 8T97 buffer whose six inputs are VA8-VA13
/// straight off the video address counter (four cascaded 74163s at 5F, 5E, 5D,
/// 5C) and whose outputs are D2-D7, with D0 and D1 not driven at all. There is
/// no logic between the counter and the buffer, so the CPU sees whatever the
/// counter is counting, and a 74163 chain on a free-running clock does not stop
/// at its maximum: it rolls over. A saturating readback would need the counter
/// to hold, and nothing holds it. The design doc carries the full derivation.
///
/// So this asserts both models: ours because it is derived, MAME's because it is
/// what MAME does and a change of mind there should be noticed rather than
/// silently absorbed.
#[test]
fn mame_agrees_about_every_signal_the_rom_measures() {
    let required = std::env::var_os("PHOSPHOR_MAME").is_some();
    if !required {
        eprintln!("skipping: set PHOSPHOR_MAME=1 to cross-check against MAME");
        return;
    }
    // The same fallback the Road Runner cross-check uses; this crate does not
    // depend on phosphor-harness, so `roms_dir()` is not available here.
    let roms = std::env::var("PHOSPHOR_ROMS").unwrap_or_else(|_| {
        format!(
            "{}/ws/mame-runtime/roms",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let roms = std::path::PathBuf::from(roms);

    for machine in MACHINES {
        let mame = run_under_mame(machine, &roms);
        let m = |k: &str| -> i64 {
            *mame
                .get(k)
                .unwrap_or_else(|| panic!("{machine}: MAME's result block has no {k}"))
        };

        let r = run(machine);
        r.assert_completed(machine);

        assert_eq!(
            m("R_MAGIC"),
            i64::from(MAGIC),
            "{machine}: MAME did not reach the magic byte, stopping at phase {}",
            m("R_PHASE")
        );

        // Absolute figures: no calibration, no cancellation, so exact equality.
        for (name, ours) in [
            ("R_PHASE", r.at(R_PHASE)),
            ("R_T1TRN", r.at(R_T1TRN)),
            ("R_T1WRP", r.at(R_T1WRP)),
            ("R_T1MAX", r.at(R_T1MAX)),
            ("R_T2CNT", r.at(R_T2CNT)),
            ("R_T2LIN", r.at(R_T2LIN)),
            ("R_T3RCNT", r.at(R_T3RCNT)),
            ("R_T3RLIN0", r.slice(R_T3RLIN, 4)[0]),
            ("R_T3RLIN1", r.slice(R_T3RLIN, 4)[1]),
            ("R_T3RLIN2", r.slice(R_T3RLIN, 4)[2]),
            ("R_T3RLIN3", r.slice(R_T3RLIN, 4)[3]),
            ("R_T3FCNT", r.at(R_T3FCNT)),
            ("R_T3FLIN0", r.slice(R_T3FLIN, 3)[0]),
            ("R_T3FLIN1", r.slice(R_T3FLIN, 3)[1]),
            ("R_T3FLIN2", r.slice(R_T3FLIN, 3)[2]),
            ("R_T4FST", r.at(R_T4FST)),
            ("R_T4SLW", r.at(R_T4SLW)),
            ("R_T5A", r.at(R_T5A)),
            ("R_T5B", r.at(R_T5B)),
        ] {
            assert_eq!(
                m(name),
                i64::from(ours),
                "{machine} {name}: MAME reports {}, we report {ours}. This figure \
                 is read straight off the video counter with nothing calibrated \
                 out, so the two are supposed to be identical.",
                m(name)
            );
        }

        // The counter's behaviour above line 255, which the two model
        // differently and neither derives. Pinned on both sides so a change of
        // mind on either cannot pass silently.
        let (ours0, ours4) = (i64::from(r.at(R_T1DW0)), i64::from(r.at(R_T1DW4)));
        assert!(
            ours4 > 4 && m("R_T1DW4") > 4,
            "{machine}: the dwell reference is too small to form a ratio"
        );
        assert!(
            ours0 * 10 > ours4 * 17 && ours0 * 10 < ours4 * 23,
            "{machine}: we dwell {ours0} at counter 0 against {ours4} at 4, a \
             ratio of {:.2}, and about 2 is what aliasing means. If this has \
             become 1 our counter has stopped aliasing, which is a change to the \
             board and wants the schematic question answered rather than this \
             assertion relaxed.",
            ours0 as f32 / ours4 as f32
        );
        assert_eq!(
            m("R_T1DW0"),
            m("R_T1DW4"),
            "{machine}: MAME dwells {} at counter 0 against {} at 4. It has always \
             reported the same for both, because video_counter_r saturates at \
             $FC above line 255 instead of aliasing to 0. If that has changed, \
             MAME has taken a position on the open schematic question and the \
             design doc should be updated with whatever reasoning came with it.",
            m("R_T1DW0"),
            m("R_T1DW4")
        );
    }
}

/// Run one machine's conformance image under MAME and return its result block.
///
/// Gated the way the drift guard is gated on `PHOSPHOR_ASM`: the caller has
/// already established that `PHOSPHOR_MAME` is set, so a missing `mame` is a
/// failure rather than a skip. A skip here would report green having compared
/// nothing, which is the failure mode this repository keeps finding.
fn run_under_mame(machine: &str, roms: &std::path::Path) -> std::collections::HashMap<String, i64> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("../tools/mame_williams_conformance.lua");
    let (_, load_addr) = image_for(machine);
    let image = root.join(if load_addr == 0xE000 {
        "tests/roms/williams_video_e000.bin"
    } else {
        "tests/roms/williams_video.bin"
    });
    let out = std::env::temp_dir().join(format!("phosphor_williams_mame_{machine}"));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create the MAME output directory");

    let status = std::process::Command::new("mame")
        .args([machine, "-rompath", roms.to_str().unwrap()])
        .arg("-autoboot_script")
        .arg(&script)
        .args(["-autoboot_delay", "0"])
        // Headless and unthrottled; the script exits the machine when the
        // program finishes, and -str is only a backstop if it never does.
        .args([
            "-video",
            "none",
            "-sound",
            "none",
            "-nothrottle",
            "-str",
            "60",
        ])
        // Keep MAME's droppings out of the working tree.
        .arg("-cfg_directory")
        .arg(out.join("cfg"))
        .arg("-nvram_directory")
        .arg(out.join("nvram"))
        .arg("-snapshot_directory")
        .arg(&out)
        .env("PHOSPHOR_CONFORMANCE_BIN", &image)
        .env("PHOSPHOR_CONFORMANCE_ADDR", format!("{load_addr:#X}"))
        .env("PHOSPHOR_CONFORMANCE_OUT", &out)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "PHOSPHOR_MAME is set, so `mame` is supposed to be on PATH, but \
                 running it for {machine} failed: {e}. A skip here would report \
                 green while comparing nothing."
            )
        });
    assert!(status.success(), "{machine}: mame exited with {status}");

    let result = out.join("mame_result.txt");
    let text = std::fs::read_to_string(&result).unwrap_or_else(|e| {
        panic!(
            "{machine}: MAME ran but wrote no result block at {}: {e}. Check its \
             output for a Lua error in tools/mame_williams_conformance.lua.",
            result.display()
        )
    });
    text.lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(k, v)| v.trim().parse().ok().map(|v| (k.to_string(), v)))
        .collect()
}
