//! Headless boot check for Atari The Empire Strikes Back (color vector game).
//!
//! ESB runs on the Star Wars board with two additions: a 137412-101 Slapstic
//! banking the $8000–$9FFF window and a second banked ROM at $A000–$FFFF. This
//! check loads the `esb` ROM set through the machine registry, runs a couple
//! thousand frames of the attract mode, and asserts that the AVG is producing a
//! non-empty, frame-to-frame *stable* vector display list — i.e. the dual-6809
//! boot cleared the slapstic protection, the Matrix Processor is feeding
//! geometry, and the vector generator is drawing it. Reports per-frame vector
//! counts over the tail window, the bounding box, distinct colors, and a coarse
//! ASCII thumbnail so bring-up can be eyeballed without a window.
//!
//! The pass/fail verdict now also lives as a ROM-gated test
//! (`empire_strikes_back_boots_past_its_slapstic` in
//! `harness/tests/boot_check_test.rs`); this stays as the interactive view —
//! per-frame counts, bounding box, colors, and an ASCII thumbnail.
//!
//!   cargo run -p phosphor-machines --example esb_boot_check -- <roms-dir> [frames]
//!
//! where <roms-dir> holds the extracted `esb` ROM files (unzip the MAME
//! `esb.zip` set into a directory first). Exits non-zero if the display list is
//! empty or collapses to nothing during the tail window.

use std::path::Path;

use phosphor_machines::registry;
use phosphor_machines::rom_loader::RomSet;

/// Number of trailing frames over which the display list must stay non-empty
/// for the boot to be considered stable.
const TAIL: usize = 60;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/esb".to_string());
    let frames: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    let rom_set = match RomSet::from_directory(Path::new(&dir)) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("RomSet load FAILED from {dir}: {e:?}");
            std::process::exit(1);
        }
    };

    // `create` loads the ROMs and resets both 6809s, so the machine is already
    // booted when it comes back.
    let entry = match registry::find("esb") {
        Some(e) => e,
        None => {
            eprintln!("esb not registered");
            std::process::exit(1);
        }
    };
    let mut machine = match (entry.create)(&rom_set) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("create FAILED: {e:?}");
            std::process::exit(1);
        }
    };

    // Run the attract mode, sampling the display-list length over the tail so we
    // can tell "booted and drawing" from "still churning through the RAM test".
    let mut tail_counts: Vec<usize> = Vec::with_capacity(TAIL);
    for f in 0..frames {
        machine.run_frame();
        if (frames - f) as usize <= TAIL {
            let n = machine.vector_display_list().map_or(0, |l| l.len());
            tail_counts.push(n);
        }
    }

    let vectors = machine.vector_display_list().unwrap_or(&[]);
    let min_tail = tail_counts.iter().copied().min().unwrap_or(0);
    let max_tail = tail_counts.iter().copied().max().unwrap_or(0);
    let stable = min_tail > 0;

    println!("frames run: {frames}");
    println!(
        "display list over last {TAIL} frames: min {min_tail}  max {max_tail}  (final {})",
        vectors.len()
    );
    println!(
        "vector display list non-empty and stable: {}",
        if stable { "yes" } else { "NO" }
    );

    // Bounding box + lit-segment stats over the final frame's list.
    let lit = vectors.iter().filter(|v| v.intensity > 0).count();
    let mut colors = std::collections::HashSet::new();
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for v in vectors {
        colors.insert((v.r, v.g, v.b));
        for (x, y) in [(v.x0, v.y0), (v.x1, v.y1)] {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
    }
    if vectors.is_empty() {
        println!("bounding box: (none — empty list)");
    } else {
        println!("lit segments (intensity > 0): {lit}/{}", vectors.len());
        println!("bounding box: x [{minx}, {maxx}]  y [{miny}, {maxy}]");
        println!("distinct colors: {}", colors.len());
    }

    // Rasterize the final frame and show a thumbnail.
    let (w, h) = machine.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    machine.render_frame(&mut buf);
    let lit_px = buf.chunks(3).filter(|p| p != &[0, 0, 0]).count();
    let total = (w * h) as usize;
    println!(
        "{w}x{h}  lit pixels {lit_px}/{total} ({:.2}%)",
        100.0 * lit_px as f64 / total as f64
    );
    thumbnail(&buf, w as usize, h as usize);

    if !stable {
        eprintln!("FAIL: vector display list was empty during the tail window");
        std::process::exit(1);
    }
}

/// 64-wide ASCII brightness thumbnail.
fn thumbnail(buf: &[u8], w: usize, h: usize) {
    const CW: usize = 64;
    let ch = (CW * h / w).max(1);
    let ramp = b" .:-=+*#%@";
    for cy in 0..ch {
        let mut line = String::new();
        for cx in 0..CW {
            let mut sum = 0u32;
            let mut n = 0u32;
            for dy in 0..(h / ch).max(1) {
                for dx in 0..(w / CW).max(1) {
                    let x = cx * w / CW + dx;
                    let y = cy * h / ch + dy;
                    if x < w && y < h {
                        let i = (y * w + x) * 3;
                        sum += buf[i] as u32 + buf[i + 1] as u32 + buf[i + 2] as u32;
                        n += 1;
                    }
                }
            }
            let b = if n > 0 { sum / (n * 3) } else { 0 };
            line.push(ramp[(b as usize * (ramp.len() - 1) / 255).min(ramp.len() - 1)] as char);
        }
        println!("  {line}");
    }
}
