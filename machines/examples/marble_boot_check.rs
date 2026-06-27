//! Headless boot check for Marble Madness (Atari System 1, Phase 1 skeleton).
//!
//! Loads the ROM set, runs a few seconds of frames, and reports that the 68010
//! left the reset vector plus coarse framebuffer stats. Video is Phase 3, so the
//! frame is expected to stay black for now — this exists to prove the main CPU
//! boots the motherboard BIOS and runs without panicking.
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

    // Run ~3 seconds of frames; the BIOS should get well past the reset vector.
    for _ in 0..180 {
        sys.run_frame();
    }
    let pc = sys.get_cpu_state().pc;
    println!(
        "PC after 180 frames = {pc:#08X}  (clock {} cycles)",
        sys.clock()
    );
    println!(
        "CPU left the reset vector: {}",
        if pc != reset_pc { "yes" } else { "NO" }
    );

    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);
    let lit = buf.chunks(3).filter(|p| p != &[0, 0, 0]).count();
    println!(
        "{w}x{h}  lit {lit}/{} pixels (video lands in Phase 3)",
        w * h
    );
}
