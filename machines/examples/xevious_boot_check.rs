//! Headless boot check for Xevious (Namco Galaga hardware).
//!
//! Loads the ROM set, runs frames, and reports how far boot gets: the main CPU
//! program counter, whether it released the sub/sound CPUs from reset, and how
//! much video RAM it populated. Clearing the lengthy power-on RAM test and
//! reaching the game loop means the 50XX start-up protection handshake
//! succeeded. Video rendering is not implemented yet (Milestone 1), so there is
//! no framebuffer thumbnail.
//!
//!   cargo run -p phosphor-machines --example xevious_boot_check -- <roms-dir> [frames]
//! where <roms-dir> holds the extracted `xevious` ROM files (unzip xevious.zip).

use std::path::Path;

use phosphor_core::core::machine::MachineCore;
use phosphor_machines::rom_loader::RomSet;
use phosphor_machines::xevious::XeviousSystem;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xevious".to_string());
    let frames: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(800);

    let rom_set = match RomSet::from_directory(Path::new(&dir)) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("RomSet load FAILED from {dir}: {e}");
            std::process::exit(1);
        }
    };

    let mut sys = XeviousSystem::new();
    if let Err(e) = sys.load_rom_set(&rom_set) {
        eprintln!("ROM load FAILED: {e}");
        std::process::exit(1);
    }
    sys.reset();
    println!("reset PC = {:#06X}", sys.main_pc());

    let mut released_at = None;
    for fr in 1..=frames {
        sys.run_frame();
        if released_at.is_none() && sys.sub_released() {
            released_at = Some(fr);
        }
        if fr % 200 == 0 || fr == frames {
            let (fg, bg) = sys.video_ram_nonzero();
            println!(
                "frame {fr:>4}: main {:#06X}  sub {:#06X}  snd {:#06X}  released {}  irq(m{} s{}) snmi {}  fg {fg}  bg {bg}",
                sys.main_pc(),
                sys.sub_pc(),
                sys.sound_pc(),
                sys.sub_released(),
                sys.main_irq_on() as u8,
                sys.sub_irq_on() as u8,
                sys.sound_nmi_on() as u8,
            );
        }
    }
    match released_at {
        Some(fr) => println!("\nsub/sound CPUs released at frame {fr} (and still running)"),
        None => println!("\nsub/sound CPUs never released — still in self-test"),
    }
}
