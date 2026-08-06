use crate::core::BusMaster;
use crate::core::component::BusMasterComponent;

/// Generic CPU interface
pub trait Cpu: BusMasterComponent + CpuStateTrait {
    /// Reset the CPU and fetch the reset vector from the bus (matching real hardware).
    fn reset(&mut self, bus: &mut Self::Bus, master: BusMaster);

    /// Signal a specific interrupt line (implementation-defined)
    fn signal_interrupt(&mut self, int: crate::core::bus::InterruptState);

    /// Query if CPU is halted internally (CWAI, WAI, STOP instruction)
    fn is_sleeping(&self) -> bool;
}

// Disassembly support
pub mod disasm;
pub use disasm::{Disassemble, DisassembledInstruction};

// Re-export state types
pub mod state;
pub use state::{
    CpuStateTrait, I8035State, I8088State, M6502State, M6800State, M6809State, M68000State,
    Mb88xxState, Z80State,
};

// Shared flag and signal helpers
pub mod flags;

// Shared M68xx ALU trait
pub mod m68xx;

// Declarative generators for the M68xx ALU opcode wrappers
mod m68xx_alu_macros;

// Re-export specific CPUs
pub mod m6800;
pub use m6800::M6800;

pub mod m6809;
pub use m6809::M6809;

// Placeholder for future
pub mod m6502;
pub use m6502::M6502;

// Z80 CPU
pub mod z80;
pub use z80::Z80;

// Intel 8035 (MCS-48 family)
pub mod i8035;
pub use i8035::I8035;

// Intel 8088 (x86 16-bit, 8-bit bus)
pub mod i8088;
pub use i8088::I8088;

// Fujitsu MB88xx (4-bit MCU, used in Namco custom chips)
pub mod mb88xx;
pub use mb88xx::Mb88xx;

// Motorola 68000 (16/32-bit, word bus)
pub mod m68000;
pub use m68000::M68000;
