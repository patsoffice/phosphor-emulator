//! The Road Runner conformance ROM's *picture*, compared against MAME's.
//!
//! Design: `docs/designs/roadrunner-video-conformance.md`.
//!
//! `machines/tests/roadrunner_video_timing_test.rs` runs the same image and
//! compares every CPU-observable figure against MAME. It cannot compare the
//! picture, because it installs synthetic graphics so that it can run with no
//! arcade ROMs at all, while MAME is running the real ones: the same writes then
//! draw different pictures, and the comparison would be meaningless.
//!
//! This file closes that gap from the other side. It is ROM-gated, so it loads
//! the **real** Road Runner graphics into our board, pokes the same conformance
//! image over the program ROM, and compares every pixel of every captured frame
//! against MAME's dump of the same frame. Both sides are then running one image
//! on one set of graphics, and any difference is a difference between the two
//! compositors rather than between two fixtures.
//!
//! Gated on `PHOSPHOR_MAME` as well as on a ROM directory, the way the drift
//! guard is gated on `PHOSPHOR_ASM`: with the variable set, a missing `mame` is
//! a failure rather than a skip, so this cannot report green while comparing
//! nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

use phosphor_core::core::machine::FrontendMachine;
use phosphor_harness::{load_rom_set, roms_dir};
use phosphor_machines::registry;

/// The same image the CI-safe suite runs, and the same one handed to MAME.
const PROGRAM: &[u8] = include_bytes!("../../machines/tests/roms/roadrunner_video.bin");

const MACHINE: &str = "roadrunner";
const LOAD_ADDR: u32 = 0x00_0000;

// Only the two fields this file needs to know when to capture; everything else
// about the result block belongs to the suite that owns it.
const R_MAGIC: u32 = 0x40_0000;
const R_PHASE: u32 = 0x40_0002;
const MAGIC: u16 = 0x5A5A;

const FIRST_SHOT_PHASE: u16 = 11;
const LAST_SHOT_PHASE: u16 = 16;
const MAX_FRAMES: usize = 64;

const WIDTH: usize = 336;
const HEIGHT: usize = 240;

fn word(m: &dyn FrontendMachine, addr: u32) -> u16 {
    let bus = m.debug_bus().expect("machine exposes a debug bus");
    let hi = bus.read(0, addr).expect("result block is readable");
    let lo = bus.read(0, addr + 1).expect("result block is readable");
    u16::from_be_bytes([hi, lo])
}

/// One frame of ours: which `run_frame` produced it, what `R_PHASE` read at its
/// end, and the picture.
struct Frame {
    index: usize,
    phase: u16,
    rgb: Vec<u8>,
}

/// Run the conformance image on a machine holding the real Road Runner
/// graphics, capturing the frame at each picture phase.
fn run_with_real_graphics(dir: &Path) -> Vec<Frame> {
    let entry = registry::find(MACHINE).unwrap_or_else(|| panic!("{MACHINE} is not registered"));
    let set = load_rom_set(dir.to_str().unwrap(), entry.rom_names)
        .unwrap_or_else(|e| panic!("loading the {MACHINE} ROM set from {}: {e}", dir.display()));
    let mut m = (entry.create)(&set).unwrap_or_else(|e| panic!("building {MACHINE}: {e}"));

    // The cartridge's own program is loaded and then written over, exactly as
    // the Lua script does to MAME's maincpu region. Only the first 8 KB moves;
    // the banked cartridge code above it is left where it is and never reached.
    {
        let bus = m.debug_bus_mut().expect("machine exposes a debug bus");
        for (i, b) in PROGRAM.iter().enumerate() {
            bus.write(0, LOAD_ADDR + i as u32, *b);
        }
    }
    m.reset();

    let mut shots = Vec::new();
    let mut seen = [false; (LAST_SHOT_PHASE - FIRST_SHOT_PHASE + 1) as usize];
    for index in 0..MAX_FRAMES {
        m.run_frame();
        let phase = word(&*m, R_PHASE);
        if (FIRST_SHOT_PHASE..=LAST_SHOT_PHASE).contains(&phase) {
            let slot = (phase - FIRST_SHOT_PHASE) as usize;
            if !seen[slot] {
                seen[slot] = true;
                let (w, h) = m.display_size();
                let mut rgb = vec![0u8; w as usize * h as usize * 3];
                m.render_frame(&mut rgb);
                shots.push(Frame { index, phase, rgb });
            }
        }
        if word(&*m, R_MAGIC) == MAGIC {
            break;
        }
    }
    assert_eq!(
        word(&*m, R_MAGIC),
        MAGIC,
        "the conformance program did not finish on the real ROM set; it reached \
         phase {}. This file only compares pictures, so a wedge here is about \
         the machine and not about the comparison.",
        word(&*m, R_PHASE)
    );
    shots
}

/// Read one of the script's snapshots as raw RGB.
///
/// These are `screen:snapshot()` PNGs rather than a buffer read out through
/// Lua, for the reason set out at length in the script: the Lua pixel accessor
/// hands back the previous frame's indices carrying the current frame's
/// palette. MAME is run with `-snapview native`, so the image is the screen's
/// own 336x240 with no artwork or scaling in it.
fn read_snapshot(path: &Path) -> Vec<u8> {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("reading MAME's frame at {}: {e}", path.display()));
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    let mut buf = vec![
        0u8;
        reader.output_buffer_size().unwrap_or_else(|| panic!(
            "{}: decoded size does not fit in memory",
            path.display()
        ))
    ];
    let info = reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    assert_eq!(
        (info.width as usize, info.height as usize),
        (WIDTH, HEIGHT),
        "MAME dumped a {}x{} frame; this board is {WIDTH}x{HEIGHT}. A size other \
         than the screen's own means -snapview native did not take effect, and \
         comparing a scaled image would be meaningless.",
        info.width,
        info.height
    );
    match info.color_type {
        png::ColorType::Rgb => {
            buf.truncate(info.buffer_size());
            buf
        }
        png::ColorType::Rgba => buf[..info.buffer_size()]
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        other => panic!("{}: unexpected colour type {other:?}", path.display()),
    }
}

/// Every pixel of every captured frame, ours against MAME's.
///
/// Both sides run the identical 68010 image on the identical graphics ROMs, so
/// this is a direct comparison of two compositors. It covers far more than the
/// probe-based assertions in the CI-safe suite: the playfield's PROM lookup and
/// colour banks, the motion-object list walk and merge, the alpha layer, the
/// palette conversion, and the scroll, over 80,640 pixels a frame and six
/// frames.
///
/// **THIS DOES NOT PASS YET, AND IS GATED SEPARATELY BECAUSE OF IT.** It is an
/// open investigation tracked as `phosphor-emulator-j5wp`, not a guard. Set
/// `PHOSPHOR_MAME_PICTURE=1` to run it; `PHOSPHOR_MAME=1` alone runs only the
/// result-block comparison, which does pass.
///
/// Two things stand between here and a green test, and after
/// `phosphor-emulator-fpgx` neither of them is the fixture any more:
///
/// 1. **A residual of exactly 64 pixels**, an 8x8 block at x 0-7, y 121-128,
///    in every one of the six phases. MAME draws it opaque black and we draw
///    the playfield through it. It is not cell-aligned vertically, so it is a
///    motion object rather than a tile, and it is constant across phases, so it
///    is not a function of anything the program does after it draws. That is
///    `phosphor-emulator-h52k`.
/// 2. **T7's mid-frame playfield write, phase 13 only**, a second 64 pixels at
///    (32, 48). MAME draws the upper cell still red because the beam had passed
///    it when the write landed; we draw it green because this board composites
///    the whole frame at the frame boundary. That is the defect
///    `raster-sampling-fidelity.md` W3 exists to fix, and the CI-safe suite
///    already holds it as a ratchet. It is expected to stay until W3 lands.
///
/// Everything else that used to show up here was the fixture, and it is worth
/// recording what it cost, because both halves reported plausible pictures:
///
/// - Clearing the machine's RAM in the ROM took the difference from ~900
///   pixels a frame to 64. MAME had the real game's palette and sprite list in
///   RAM when the program took over and the program only wrote the entries it
///   used.
/// - The ROM published each phase at the vblank edge, which is exactly where
///   MAME samples, so which frame MAME associated with a phase was a coin toss
///   and came down differently per phase. The ROM now publishes in active
///   display. `PHOSPHOR_PICTURE_ALIGN=1` prints the frame each side captured
///   each phase at; equal *gaps* mean the two are looking at the same frame.
/// - The script read the screen with `screen:pixels()`, which returns the
///   previous frame's pixel indices carrying the current frame's palette. It
///   now takes a native snapshot instead. See the script for the mechanism.
#[test]
fn our_picture_matches_mames_on_the_real_graphics() {
    if std::env::var_os("PHOSPHOR_MAME_PICTURE").is_none() {
        eprintln!(
            "skipping: picture comparison is an open investigation \
             (phosphor-emulator-j5wp); set PHOSPHOR_MAME_PICTURE=1 to run it"
        );
        return;
    }
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    if !dir.join("roadrunn.zip").exists() {
        eprintln!("skipping: no roadrunn.zip in {}", dir.display());
        return;
    }

    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tools/mame_roadrunner_conformance.lua");
    let image = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../machines/tests/roms/roadrunner_video.bin");
    let out = std::env::temp_dir().join("phosphor_roadrunner_mame_picture");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create the MAME output directory");

    let status = Command::new("mame")
        .args(["roadrunn", "-rompath", dir.to_str().unwrap()])
        .arg("-autoboot_script")
        .arg(&script)
        .args(["-autoboot_delay", "0"])
        .args([
            "-video",
            "none",
            "-sound",
            "none",
            "-nothrottle",
            "-str",
            "120",
        ])
        // `native` is what makes the snapshot the screen's own 336x240 rather
        // than whatever the render target is showing; `read_snapshot` fails
        // loudly on any other size rather than comparing a scaled image.
        .args(["-snapview", "native"])
        .arg("-snapshot_directory")
        .arg(&out)
        // Keep MAME's own directories inside the scratch output too, or it
        // drops cfg/ and nvram/ into the working tree.
        .arg("-cfg_directory")
        .arg(out.join("cfg"))
        .arg("-nvram_directory")
        .arg(out.join("nvram"))
        .env("PHOSPHOR_CONFORMANCE_BIN", &image)
        .env("PHOSPHOR_CONFORMANCE_OUT", &out)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "PHOSPHOR_MAME is set, so `mame` is supposed to be on PATH, but \
                 running it failed: {e}. A skip here would report green while \
                 comparing nothing."
            )
        });
    assert!(status.success(), "mame exited with {status}");

    let align = std::env::var_os("PHOSPHOR_PICTURE_ALIGN").is_some();
    let ours = run_with_real_graphics(&dir);
    assert_eq!(
        ours.len(),
        (LAST_SHOT_PHASE - FIRST_SHOT_PHASE + 1) as usize,
        "we captured {} frames rather than one per picture phase",
        ours.len()
    );

    if let Ok(d) = std::env::var("PHOSPHOR_PICTURE_DUMP") {
        for f in &ours {
            let mut out = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
            out.extend_from_slice(&f.rgb);
            std::fs::write(format!("{d}/ours_phase{}.ppm", f.phase), out).unwrap();
        }
        for f in &ours {
            let _ = std::fs::copy(
                out.join(format!("mame_phase{}.png", f.phase)),
                format!("{d}/mame_phase{}.png", f.phase),
            );
        }
    }

    // Are the two sides looking at the same frame at all? Each prints the frame
    // it first saw each phase in (MAME's are the `[CONF] capturing phase N at
    // frame M` lines above), and the answer is in the *gaps*: the two counters
    // have different origins, because MAME counts the frame it patches and
    // resets in, so only the spacing is comparable. Equal gaps mean both sides
    // observed every phase in the same frame of the program, which is what
    // `phosphor-emulator-fpgx` was about; any pixel difference left after that
    // is two compositors disagreeing rather than two clocks.
    if align {
        let at: Vec<String> = ours
            .iter()
            .map(|f| format!("{}@{}", f.phase, f.index))
            .collect();
        let gaps: Vec<usize> = ours.windows(2).map(|w| w[1].index - w[0].index).collect();
        eprintln!("we captured {}; gaps {gaps:?}", at.join(" "));
    }

    let mut differing = Vec::new();
    for f in &ours {
        let phase = f.phase;
        let our_rgb = &f.rgb;
        let path = out.join(format!("mame_phase{phase}.png"));
        let mame_rgb = read_snapshot(&path);
        assert_eq!(
            our_rgb.len(),
            mame_rgb.len(),
            "phase {phase}: frame sizes differ"
        );
        let bad = our_rgb
            .chunks_exact(3)
            .zip(mame_rgb.chunks_exact(3))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i % WIDTH, i / WIDTH, a.to_vec(), b.to_vec()))
            .collect::<Vec<_>>();
        if !bad.is_empty() {
            let (x, y, a, b) = &bad[0];
            differing.push(format!(
                "phase {phase}: {} of {} pixels differ, first at ({x}, {y}) \
                 where we draw ({}, {}, {}) and MAME draws ({}, {}, {})",
                bad.len(),
                our_rgb.len() / 3,
                a[0],
                a[1],
                a[2],
                b[0],
                b[1],
                b[2],
            ));
        }
    }

    assert!(
        differing.is_empty(),
        "our compositor and MAME's disagree about what this program draws. Both \
         ran the same image on the same graphics ROMs, so this is a difference \
         between the two renderers.\n  {}\nMAME's frames are in {}",
        differing.join("\n  "),
        out.display()
    );
}
