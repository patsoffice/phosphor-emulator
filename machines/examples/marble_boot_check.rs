//! Headless boot check for Marble Madness (Atari System 1).
//!
//! Loads the ROM set, runs a few seconds of frames, and reports that the 68010
//! left the reset vector plus framebuffer stats (lit pixels, distinct colours)
//! and a coarse ASCII thumbnail. As of Phase 3a only the alpha (text/HUD) layer
//! renders — the playfield and motion objects land in Phases 3b/3c — so expect
//! the attract-mode HUD text on an otherwise black frame.
//!
//!   cargo run -p phosphor-machines --example marble_boot_check -- <roms-dir>
//! where <roms-dir> holds the extracted `marble` ROM files (136032.*, 136033.*).

use std::path::Path;

use phosphor_core::core::machine::{MachineCore, Renderable};
use phosphor_machines::marble::MarbleSystem;
use phosphor_machines::rom_loader::RomSet;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/marble".to_string());
    let frames: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let rom_set = match RomSet::from_directory(Path::new(&dir)) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("RomSet load FAILED from {dir}: {e}");
            std::process::exit(1);
        }
    };

    let mut sys = MarbleSystem::new();
    if let Err(e) = sys.load_rom_set(&rom_set) {
        eprintln!("ROM load FAILED: {e}");
        std::process::exit(1);
    }
    sys.reset();
    let reset_pc = sys.get_cpu_state().pc;
    println!("reset PC = {reset_pc:#08X}");

    for _ in 0..frames {
        sys.run_frame();
    }
    let pc = sys.get_cpu_state().pc;
    println!(
        "PC after {frames} frames = {pc:#08X}  (clock {} cycles)",
        sys.clock()
    );
    println!(
        "CPU left the reset vector: {}",
        if pc != reset_pc { "yes" } else { "NO" }
    );
    let (pal, alpha, pf) = sys.video_ram_stats();
    println!("video RAM non-zero bytes: palette {pal}  alpha {alpha}  playfield {pf}");
    let (held, snd_clk, cmd_pend, resp_pend) = sys.sound_debug();
    println!(
        "sound: held_reset {held}  cycles {snd_clk}  cmd_pending {cmd_pend}  resp_pending {resp_pend}"
    );
    let (ee_nonff, ee_writes) = sys.eeprom_debug();
    println!("eeprom: non-0xFF bytes {ee_nonff}  byte writes accepted {ee_writes}");

    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);

    let lit = buf.chunks(3).filter(|p| p != &[0, 0, 0]).count();
    let total = (w * h) as usize;
    let mut colors = std::collections::HashSet::new();
    for p in buf.chunks(3) {
        colors.insert((p[0], p[1], p[2]));
    }
    println!(
        "{w}x{h}  lit {lit}/{total} ({:.1}%)  distinct colors {}",
        100.0 * lit as f64 / total as f64,
        colors.len()
    );
    thumbnail(&buf, w as usize, h as usize);
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
