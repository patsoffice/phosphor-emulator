//! Shared headless boot harness and ROM-path resolver.
//!
//! This crate owns the machine-construction sequence that the disasm tools
//! (`frameshot`, `trace`) and the frontend all need: resolve a ROM set from a
//! path, create a registered machine, reset it, script inputs on a frame
//! timeline, and step frames. Keeping it in one place stops each consumer from
//! forking its own boot path.

use std::path::PathBuf;

mod harness;
mod rom_path;

pub use harness::{Harness, MotionSpec, PressSpec};
pub use rom_path::load_rom_set;

/// Locate a ROM directory for ROM-gated integration tests: `PHOSPHOR_ROMS` if
/// set, else the conventional `~/ws/mame-runtime/roms`. Returns `None` when
/// neither exists, so a gated test can skip cleanly and CI without ROMs stays
/// green.
///
/// Shared by the disasm `trace`/`frameshot` tests and the `phosphor-script`
/// end-to-end test so there is a single copy of this convention.
pub fn roms_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PHOSPHOR_ROMS") {
        let p = PathBuf::from(dir);
        return p.is_dir().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join("ws/mame-runtime/roms");
    p.is_dir().then_some(p)
}
