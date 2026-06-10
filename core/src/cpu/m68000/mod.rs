//! Motorola 68000 CPU emulation.
//!
//! The 68000 is a 16-bit-data / 32-bit-register big-endian CPU with eight
//! data registers, eight address registers (A7 doubles as the active stack
//! pointer), supervisor/user privilege modes, and a 256-entry vectored
//! exception table. The external data bus is 16 bits wide: the bus interface
//! uses `Address = u32` (24-bit physical address space on the 68000) and
//! `Data = u16` (one bus transaction = one word at an even address).
//!
//! Execution is modeled at the instruction level (like the i8088): the full
//! instruction is decoded and applied atomically on its first cycle, then the
//! remaining documented cycles are burned as bus-idle wait states. Per-cycle
//! bus traces and exact prefetch behavior are not modeled.

pub(crate) mod addressing;
pub mod flags;
mod move_ops;
pub use flags::SrFlag;

use crate::core::save_state::{SaveError, StateReader, StateWriter};
use crate::core::{Bus, BusMaster, bus::InterruptState, component::BusMasterComponent};
use crate::cpu::{
    Cpu,
    state::{CpuStateTrait, M68000State},
};
use crate::prelude::Saveable;

/// Which member of the 68000 family this CPU instance models.
///
/// Only `M68000` has behavior today; the enum exists so variant-dependent
/// logic (address-bus width, exception frame formats, brief-extension-word
/// scaling) can be gated in one place as later variants are added.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum M68kVariant {
    M68000 = 0,
    M68010 = 1,
    M68020 = 2,
    M68030 = 3,
}

impl Saveable for M68kVariant {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_u8(*self as u8);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        *self = match r.read_u8()? {
            1 => M68kVariant::M68010,
            2 => M68kVariant::M68020,
            3 => M68kVariant::M68030,
            _ => M68kVariant::M68000,
        };
        Ok(())
    }
}

/// Execution state machine for multi-cycle instructions.
#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    /// Ready to fetch the next instruction.
    Fetch,
    /// Executing an instruction: (remaining_cycles). The instruction has
    /// already been decoded and its effect applied on the first cycle;
    /// remaining cycles are bus-idle wait states.
    Execute(u32),
    /// STOP instruction executed, waiting for an interrupt.
    Stopped,
    /// Halted by a double bus fault or external HALT; only reset recovers.
    Halted,
}

/// Fields are ordered to match the save-state serialization layout (version 1).
#[derive(Saveable)]
#[save_version(1)]
pub struct M68000 {
    /// Data registers D0-D7.
    pub d: [u32; 8],
    /// Address registers A0-A6; `a[7]` is the ACTIVE stack pointer
    /// (USP or SSP depending on the SR supervisor bit).
    pub a: [u32; 8],
    /// Inactive user stack pointer (valid while in supervisor mode).
    pub usp: u32,
    /// Inactive supervisor stack pointer (valid while in user mode).
    pub ssp: u32,
    pub pc: u32,
    /// Status register: high byte = system byte (T, S, interrupt mask),
    /// low byte = condition code register (X N Z V C).
    pub sr: u16,
    pub variant: M68kVariant,

    // Internal state (serialized)
    /// Previous level-7 interrupt state for NMI edge detection (used from M5).
    #[allow(dead_code)]
    pub(crate) nmi_previous: bool,
    /// STOP instruction executed, waiting for interrupt.
    pub(crate) stopped: bool,
    /// Double bus fault or external halt; only reset recovers.
    pub(crate) halted: bool,

    // Execution temporaries — not saved, reset to defaults on load
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    /// Opcode word of the instruction currently executing.
    #[allow(dead_code)]
    #[save_skip(default)]
    pub(crate) opcode: u16,
    /// Set when a word/long access used an odd address during the current
    /// instruction. The address-error exception (vector 3) lands in M5;
    /// until then this flag lets callers identify such accesses (the
    /// validation harness skips them).
    #[save_skip(default)]
    pub(crate) address_error: bool,
}

impl Default for M68000 {
    fn default() -> Self {
        Self::new()
    }
}

impl M68000 {
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            // Supervisor mode, interrupts masked (reset state)
            sr: 0x2700,
            variant: M68kVariant::M68000,
            nmi_previous: false,
            stopped: false,
            halted: false,
            state: ExecState::Fetch,
            opcode: 0,
            address_error: false,
        }
    }

    /// Returns true when the CPU is at an instruction boundary (ready to fetch).
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }

    /// True if the instruction that just executed performed a word or long
    /// access at an odd address (would raise an address error on hardware).
    pub fn took_address_error(&self) -> bool {
        self.address_error
    }

    /// Mask an effective address to the physical address-bus width.
    /// The 68000/68010 drive 24 address lines; 68020+ drive all 32.
    #[inline]
    pub(crate) fn mask_addr(&self, addr: u32) -> u32 {
        match self.variant {
            M68kVariant::M68000 | M68kVariant::M68010 => addr & 0x00FF_FFFF,
            M68kVariant::M68020 | M68kVariant::M68030 => addr,
        }
    }

    /// Execute one bus cycle.
    pub fn execute_cycle<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        match self.state {
            ExecState::Fetch => {
                if self.halted {
                    self.state = ExecState::Halted;
                    return;
                }
                if self.stopped {
                    self.state = ExecState::Stopped;
                    return;
                }
                // Interrupt sampling at the instruction boundary lands in M5.

                // Fetch the opcode word and execute the instruction atomically.
                self.address_error = false;
                let opcode = self.read_imm_word(bus, master);
                self.opcode = opcode;
                self.execute_instruction(opcode, bus, master);
            }
            ExecState::Execute(remaining) => {
                if remaining <= 1 {
                    self.state = ExecState::Fetch;
                } else {
                    self.state = ExecState::Execute(remaining - 1);
                }
            }
            ExecState::Stopped => {
                // Wake-up on interrupt lands in M5 (STOP itself is an M5
                // instruction); nothing can leave this state yet.
            }
            ExecState::Halted => {
                // Only an external reset recovers from a halt.
            }
        }
    }

    /// Complete an instruction that took `total_cycles` clock cycles: the
    /// current tick already counts as one, and any remainder is burned as
    /// bus-idle wait states.
    pub(crate) fn finish(&mut self, total_cycles: u32) {
        self.state = if total_cycles <= 1 {
            ExecState::Fetch
        } else {
            ExecState::Execute(total_cycles - 1)
        };
    }

    /// Decode and execute one instruction, leaving `self.state` either back
    /// at `Fetch` or in `Execute(n)` to burn the remaining documented cycles.
    ///
    /// Dispatch is two-level: first on the opcode "line" (top 4 bits), then
    /// on the line-specific sub-encoding. Instruction families are wired in
    /// here as they are implemented.
    fn execute_instruction<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        match (opcode >> 12) & 0xF {
            // MOVE.b / MOVE.l / MOVE.w (and MOVEA for An destinations)
            0x1..=0x3 => self.op_move(opcode, bus, master),
            // MOVEQ (bit 8 set is unassigned on the 68000)
            0x7 if opcode & 0x0100 == 0 => self.op_moveq(opcode),
            // Remaining lines are treated as 4-cycle NOPs; the
            // illegal-instruction / line-A / line-F exceptions land in M5.
            _ => self.finish(4),
        }
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl BusMasterComponent for M68000 {
    type Bus = dyn Bus<Address = u32, Data = u16>;

    fn tick_with_bus(&mut self, bus: &mut Self::Bus, master: BusMaster) -> bool {
        self.execute_cycle(bus, master);
        matches!(self.state, ExecState::Fetch)
    }
}

impl Cpu for M68000 {
    fn reset(&mut self, bus: &mut Self::Bus, master: BusMaster) {
        // Reset enters supervisor mode with trace off and interrupts masked,
        // then loads SSP from vector 0 and PC from vector 1.
        self.sr = 0x2700; // S=1, T=0, interrupt mask = 7
        self.stopped = false;
        self.halted = false;
        self.nmi_previous = false;
        self.address_error = false;
        self.state = ExecState::Fetch;

        self.a[7] = self.read_long_at(bus, master, 0x0000_0000);
        self.pc = self.read_long_at(bus, master, 0x0000_0004);
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {
        // Interrupts are sampled from the bus at instruction boundaries (M5).
    }

    fn is_sleeping(&self) -> bool {
        self.stopped || self.halted
    }
}

impl CpuStateTrait for M68000 {
    type Snapshot = M68000State;

    fn snapshot(&self) -> M68000State {
        M68000State {
            d: self.d,
            a: self.a,
            usp: self.usp,
            ssp: self.ssp,
            pc: self.pc,
            sr: self.sr,
        }
    }
}

// ---------------------------------------------------------------------------
// Debug support
// ---------------------------------------------------------------------------

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};

impl Debuggable for M68000 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for M68000 {
    fn debug_pc(&self) -> u16 {
        // The debug interface is 16-bit today; expose the low half of PC.
        self.pc as u16
    }

    fn debug_at_instruction_boundary(&self) -> bool {
        self.at_instruction_boundary()
    }

    fn debug_disassemble(
        &self,
        _addr: u16,
        bytes: &[u8],
    ) -> crate::cpu::disasm::DisassembledInstruction {
        // Stub disassembler: show the raw opcode word. Full 68000 disassembly
        // lands in M6.
        let opcode = if bytes.len() >= 2 {
            u16::from_be_bytes([bytes[0], bytes[1]])
        } else {
            0
        };
        crate::cpu::disasm::DisassembledInstruction {
            mnemonic: "DC.W",
            operands: format!("${opcode:04X}"),
            byte_len: 2,
            bytes: [(opcode >> 8) as u8, opcode as u8, 0, 0, 0, 0],
            target_addr: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Minimal word bus shared by the m68000 unit tests: 64 KB of big-endian
/// byte memory served 16 bits at a time at even addresses.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::core::{Bus, BusMaster, bus::InterruptState};

    pub(crate) struct WordBus {
        pub(crate) memory: Vec<u8>,
    }

    impl WordBus {
        pub(crate) fn new() -> Self {
            Self {
                memory: vec![0; 0x10000],
            }
        }

        /// Load bytes at a byte address (test setup helper).
        pub(crate) fn load(&mut self, addr: u32, data: &[u8]) {
            let start = addr as usize;
            self.memory[start..start + data.len()].copy_from_slice(data);
        }
    }

    impl Bus for WordBus {
        type Address = u32;
        type Data = u16;

        fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
            let i = (addr & 0xFFFE) as usize;
            u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
        }

        fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
            let i = (addr & 0xFFFE) as usize;
            self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
        }

        fn is_halted_for(&self, _master: BusMaster) -> bool {
            false
        }

        fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
            InterruptState::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::WordBus;
    use super::*;

    #[test]
    fn new_state() {
        let cpu = M68000::new();
        assert_eq!(cpu.sr, 0x2700);
        assert_eq!(cpu.pc, 0);
        assert_eq!(cpu.d, [0; 8]);
        assert_eq!(cpu.a, [0; 8]);
        assert_eq!(cpu.variant, M68kVariant::M68000);
        assert!(cpu.at_instruction_boundary());
        assert!(!cpu.is_sleeping());
    }

    #[test]
    fn reset_loads_ssp_and_pc_vectors() {
        let mut cpu = M68000::new();
        let mut bus = WordBus::new();
        // Vector 0 (SSP) = $00012000, vector 1 (PC) = $00000400
        bus.memory[0..8].copy_from_slice(&[0x00, 0x01, 0x20, 0x00, 0x00, 0x00, 0x04, 0x00]);

        cpu.reset(&mut bus, BusMaster::Cpu(0));

        assert_eq!(cpu.a[7], 0x0001_2000);
        assert_eq!(cpu.pc, 0x0000_0400);
        assert_eq!(cpu.sr, 0x2700);
        assert!(cpu.at_instruction_boundary());
    }

    #[test]
    fn mask_addr_is_24_bit_on_68000() {
        let cpu = M68000::new();
        assert_eq!(cpu.mask_addr(0xFF12_3456), 0x0012_3456);
        let mut cpu20 = M68000::new();
        cpu20.variant = M68kVariant::M68020;
        assert_eq!(cpu20.mask_addr(0xFF12_3456), 0xFF12_3456);
    }

    #[test]
    fn unimplemented_opcode_burns_cycles_to_boundary() {
        let mut cpu = M68000::new();
        let mut bus = WordBus::new();
        cpu.pc = 0x1000;

        // First tick fetches and "executes"; instruction must complete in a
        // bounded number of cycles and advance PC by one word.
        let mut ticks = 0;
        while !cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            ticks += 1;
            assert!(ticks < 100, "instruction never reached a boundary");
        }
        assert_eq!(cpu.pc, 0x1002);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut cpu = M68000::new();
        cpu.d[0] = 0x1234_5678;
        cpu.a[6] = 0xDEAD_BEEF;
        cpu.usp = 0x0000_8000;
        cpu.ssp = 0x0000_9000;
        cpu.pc = 0x0040_0000;
        cpu.sr = 0x2704;
        let snap = cpu.snapshot();
        assert_eq!(snap.d[0], 0x1234_5678);
        assert_eq!(snap.a[6], 0xDEAD_BEEF);
        assert_eq!(snap.usp, 0x0000_8000);
        assert_eq!(snap.ssp, 0x0000_9000);
        assert_eq!(snap.pc, 0x0040_0000);
        assert_eq!(snap.sr, 0x2704);
    }

    #[test]
    fn is_sleeping_when_stopped_or_halted() {
        let mut cpu = M68000::new();
        assert!(!cpu.is_sleeping());
        cpu.stopped = true;
        assert!(cpu.is_sleeping());
        cpu.stopped = false;
        cpu.halted = true;
        assert!(cpu.is_sleeping());
    }
}
