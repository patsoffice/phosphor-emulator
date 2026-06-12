extern crate self as phosphor_core;

pub mod audio;
pub mod core;
pub mod cpu;
pub mod device;
pub mod dirty_bitset;
pub mod gfx;

pub use cpu::m68000::M68000;

pub mod prelude {
    pub use crate::core::machine::{
        AnalogInput, FrontendMachine, InputButton, MachineCore, Nvram, Profilable, SaveState,
    };
    pub use crate::core::{
        Bus, BusMaster, BusMasterComponent, SaveError, Saveable, StateReader, StateWriter,
        bus::InterruptState,
    };
    pub use crate::cpu::Cpu;
    pub use phosphor_macros::Saveable;
}
