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

/// A difference that is understood, held the way `audio/expectations.toml` and
/// the two mid-frame assertions in the CI-safe suite are held: the list can
/// only shrink. The comparison passes while exactly these are present and fails
/// if one grows, moves, disappears, or a new one appears, so it cannot quietly
/// absorb a regression while a known defect is outstanding.
struct Known {
    phase: u16,
    /// Pixels differing in that phase's frame.
    pixels: usize,
    /// Where the first of them is, in the raster order the comparison walks.
    at: (usize, usize),
    /// Why it is here and what makes it go away. Never "we could not work it
    /// out"; an entry nobody can explain is a bug being pinned.
    #[allow(dead_code)]
    why: &'static str,
}

const KNOWN: &[Known] = &[Known {
    phase: 13,
    pixels: 64,
    at: (32, 48),
    why: "T7 writes two playfield cells at scanline 120. MAME leaves the upper \
          one (rows 48-55) red because the beam had already passed it; we turn \
          it green because this board composites the whole frame at the frame \
          boundary. That is the defect raster-sampling-fidelity.md W3 exists to \
          fix (phosphor-emulator-raster-sampling-6kae.3), and the CI-safe suite \
          holds the same behaviour as a ratchet. When W3 lands this entry goes.",
}];

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
            .as_chunks::<4>()
            .0
            .iter()
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
/// It passes, and it is still gated on `PHOSPHOR_MAME_PICTURE` on top of the
/// ROM directory, because it needs `mame` on `PATH` and the arcade ROMs; CI has
/// neither. `PHOSPHOR_MAME=1` alone runs the result-block comparison, which is
/// a different test in `phosphor-machines`.
///
/// One difference remains and it is in `KNOWN`: T7's mid-frame playfield write
/// in phase 13, where MAME leaves the upper cell red because the beam had
/// passed it and we turn it green because this board composites at the frame
/// boundary. Every other pixel of every other phase matches exactly.
///
/// Getting there took three fixes, and all three are worth recording because
/// each produced a picture that looked entirely plausible:
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
/// - The last 64 were ours: this board skipped drawing a motion-object entry
///   flagged `0xFFFF` in word 1. That flag belongs to the scanline-interrupt
///   comparator, which is a cartridge option, while the renderer is on the
///   motherboard and serves cartridges that have no such comparator. See
///   `render_motion_objects` and `phosphor-emulator-h52k`.
#[test]
fn our_picture_matches_mames_on_the_real_graphics() {
    if std::env::var_os("PHOSPHOR_MAME_PICTURE").is_none() {
        eprintln!(
            "skipping: the picture comparison needs mame on PATH and the arcade \
             ROMs; set PHOSPHOR_MAME_PICTURE=1 to run it"
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

    let mut wrong = Vec::new();
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
            .as_chunks::<3>()
            .0
            .iter()
            .zip(mame_rgb.as_chunks::<3>().0)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i % WIDTH, i / WIDTH, a.to_vec(), b.to_vec()))
            .collect::<Vec<_>>();

        let seen = bad.first().map(|(x, y, _, _)| (bad.len(), *x, *y));
        let want = KNOWN
            .iter()
            .find(|k| k.phase == phase)
            .map(|k| (k.pixels, k.at.0, k.at.1));
        if seen == want {
            continue;
        }
        match (seen, want) {
            (Some((n, x, y)), None) => {
                let (_, _, a, b) = &bad[0];
                wrong.push(format!(
                    "phase {phase}: {n} pixels differ with none expected, first at \
                     ({x}, {y}) where we draw ({}, {}, {}) and MAME draws ({}, {}, {})",
                    a[0], a[1], a[2], b[0], b[1], b[2],
                ));
            }
            (None, Some((n, x, y))) => wrong.push(format!(
                "phase {phase}: the {n} pixels expected at ({x}, {y}) are gone. If \
                 raster-sampling-6kae.3 has landed this is the win, and the entry \
                 in KNOWN should be deleted rather than the assertion relaxed"
            )),
            (Some((n, x, y)), Some((wn, wx, wy))) => wrong.push(format!(
                "phase {phase}: {n} pixels differ starting at ({x}, {y}), expected \
                 {wn} starting at ({wx}, {wy})"
            )),
            (None, None) => {}
        }
    }

    assert!(
        wrong.is_empty(),
        "the picture comparison moved. Both sides ran the same image on the same \
         graphics ROMs, so a change here is the two compositors disagreeing about \
         something new.\n  {}\nMAME's frames are in {}",
        wrong.join("\n  "),
        out.display()
    );
}
