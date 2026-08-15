//! Headless boot check for the Galaxian-family machines. Loads each ROM set,
//! runs a few seconds of frames, and reports framebuffer stats (lit pixels,
//! distinct colors) plus a coarse ASCII thumbnail so banked-GFX bring-up can be
//! eyeballed without a window.
//!
//! The pass/fail verdict now also lives as a ROM-gated test
//! (`the_galaxian_family_draws_a_populated_frame` in
//! `harness/tests/boot_check_test.rs`); this stays as the interactive view —
//! lit-pixel/color stats and an ASCII thumbnail per game.
//!
//!   cargo run -p phosphor-machines --example galaxian_boot_check -- <roms-root>
//! where <roms-root> holds extracted subdirs galaxian/ mooncrst/ pisces/ uniwars/.

use std::path::Path;

use phosphor_machines::registry;
use phosphor_machines::rom_loader::RomSet;

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/rk".to_string());
    for id in ["galaxian", "mooncrst", "pisces", "uniwars"] {
        check(&root, id);
    }
}

fn check(root: &str, id: &str) {
    println!("\n=== {id} ===");
    let dir = Path::new(root).join(id);
    let rom_set = match RomSet::from_directory(&dir) {
        Ok(rs) => rs,
        Err(e) => {
            println!("  RomSet load FAILED: {e:?}");
            return;
        }
    };
    let entry = match registry::find(id) {
        Some(e) => e,
        None => {
            println!("  not registered");
            return;
        }
    };
    let mut machine = match (entry.create)(&rom_set) {
        Ok(m) => m,
        Err(e) => {
            println!("  create FAILED: {e:?}");
            return;
        }
    };

    let (w, h) = machine.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    // Run ~3 seconds so the game boots past its RAM check / attract intro.
    for _ in 0..180 {
        machine.run_frame();
    }
    machine.render_frame(&mut buf);

    let lit = buf.chunks(3).filter(|p| p != &[0, 0, 0]).count();
    let total = (w * h) as usize;
    let mut colors = std::collections::HashSet::new();
    for p in buf.chunks(3) {
        colors.insert((p[0], p[1], p[2]));
    }
    println!(
        "  {w}x{h}  lit {lit}/{total} ({:.1}%)  distinct colors {}",
        100.0 * lit as f64 / total as f64,
        colors.len()
    );
    thumbnail(&buf, w as usize, h as usize);
}

/// 56-wide ASCII brightness thumbnail.
fn thumbnail(buf: &[u8], w: usize, h: usize) {
    const CW: usize = 56;
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
