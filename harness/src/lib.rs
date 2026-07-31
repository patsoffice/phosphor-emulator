//! Shared headless boot harness and ROM-path resolver.
//!
//! This crate owns the machine-construction sequence that the disasm tools
//! (`frameshot`, `trace`) and the frontend all need: resolve a ROM set from a
//! path, create a registered machine, reset it, script inputs on a frame
//! timeline, and step frames. Keeping it in one place stops each consumer from
//! forking its own boot path.

mod harness;
mod rom_path;

pub use harness::{Harness, PressSpec};
pub use rom_path::load_rom_set;
