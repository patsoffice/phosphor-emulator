//! End-to-end: the scripted render must match `disasm frameshot` byte-for-byte.
//!
//! Both paths boot the same machine via the shared `Harness`, advance the same
//! number of frames, and call `render_frame` — so their pixels must be
//! identical. Any difference is a real bug in the `DebugSession`/screenshot
//! wiring, not a rendering nondeterminism (there is no wall-clock or RNG in the
//! tick path).
//!
//! ROM-gated: skips cleanly (with an eprintln) when no ROM dir is present, so
//! CI without ROMs stays green. Run locally with a ROM dir on `PHOSPHOR_ROMS`
//! (or the conventional `~/ws/mame-runtime/roms`).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use phosphor_core::core::machine::{FrontendMachine, Orientation};
use phosphor_core::gfx::apply_orientation;
use phosphor_harness::{Harness, roms_dir};
use phosphor_script::DebugSession;

/// mrdo is a raster machine present in the conventional ROM set; 3100 frames is
/// well past its power-on self-test, matching `examples/capture.rhai`.
const MACHINE: &str = "mrdo";
const FRAMES: u64 = 3100;

/// Render the booted machine the way `disasm frameshot` does: native buffer,
/// then the machine's declared orientation applied centrally.
fn render_oriented(m: &mut dyn FrontendMachine) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = m.display_size();
    let mut native = vec![0u8; nw as usize * nh as usize * 3];
    m.render_frame(&mut native);
    let orient = m.orientation();
    if orient == Orientation::NORMAL {
        (nw, nh, native)
    } else {
        let (dw, dh) = if orient.swaps_axes() {
            (nh, nw)
        } else {
            (nw, nh)
        };
        let mut oriented = vec![0u8; dw as usize * dh as usize * 3];
        apply_orientation(&native, &mut oriented, nw as usize, nh as usize, orient);
        (dw, dh, oriented)
    }
}

/// Decode the RGB8 PNG that `screenshot` writes back into a tight RGB buffer.
fn decode_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(BufReader::new(File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![
        0u8;
        reader
            .output_buffer_size()
            .expect("decoded size fits in memory")
    ];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(
        info.color_type,
        png::ColorType::Rgb,
        "screenshot writes RGB8"
    );
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn scripted_render_matches_frameshot() {
    let Some(roms) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let path = roms.to_str().unwrap();

    // Reference: the frameshot path — boot via the harness, advance FRAMES, and
    // render with the same central-orientation step disasm uses.
    let (rw, rh, reference) = {
        let mut harness =
            Harness::build(MACHINE, path, None, None, &[], &[]).expect("harness boot");
        for _ in 0..FRAMES {
            harness.run_frame();
        }
        render_oriented(harness.machine_mut())
    };

    // Guard the guard: a uniform frame would let a broken render pass trivially.
    assert!(
        reference.iter().any(|&b| b != reference[0]),
        "reference frame is uniform; pick a machine/frame with real content"
    );

    // Scripted: open a DebugSession, advance, and screenshot to a temp PNG —
    // exactly what examples/capture.rhai does — then decode it back to RGB.
    let out = std::env::temp_dir().join("phosphor_script_frameshot_parity.png");
    let _ = std::fs::remove_file(&out);
    {
        let mut session = DebugSession::open(MACHINE, path).expect("session open");
        session.run_frames(FRAMES);
        session
            .screenshot(out.to_str().unwrap())
            .expect("screenshot");
    }
    let (sw, sh, scripted) = decode_png(&out);
    let _ = std::fs::remove_file(&out);

    assert_eq!((sw, sh), (rw, rh), "display dimensions differ");

    // Byte-exact RGB equality is imgdiff 0%.
    let diffs = scripted
        .iter()
        .zip(&reference)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        diffs,
        0,
        "scripted render differs from frameshot at frame {FRAMES}: {diffs} of {} bytes differ",
        reference.len()
    );
}
