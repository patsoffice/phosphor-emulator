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
mod alu;
mod bit;
mod branch;
mod exception;
pub mod flags;
mod move_ops;
mod stack;
use alu::binary::LogicalOp;
use alu::unary::UnaryOp;
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
    /// Address of that opcode word (PC before the fetch). Exception frames
    /// that point at the faulting instruction push this value.
    #[save_skip(default)]
    pub(crate) instr_pc: u32,
    /// Set when a word/long access used an odd address during the current
    /// instruction. The exact mid-instruction abort of the address-error
    /// exception (vector 3) is not modeled; this flag lets callers identify
    /// such accesses (the validation harness skips them).
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
            instr_pc: 0,
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
                self.instr_pc = self.pc;
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
        let opmode = (opcode >> 6) & 7;
        let ea_mode = (opcode >> 3) & 7;
        match (opcode >> 12) & 0xF {
            // ANDI/ORI/EORI to CCR (byte forms) and to SR (word forms,
            // privileged)
            0x0 if matches!(opcode, 0x003C | 0x007C | 0x023C | 0x027C | 0x0A3C | 0x0A7C) => {
                self.op_sr_imm(opcode, bus, master)
            }
            // Dynamic bit ops (bit number in Dn); EA mode 001 there
            // encodes MOVEP (M7)
            0x0 if opcode & 0x0100 != 0 => {
                if ea_mode == 1 {
                    self.finish(4); // MOVEP (M7)
                } else {
                    self.op_bitop(opcode, bus, master, true);
                }
            }
            // Static bit ops (bit number in an extension word)
            0x0 if opcode & 0x0F00 == 0x0800 => self.op_bitop(opcode, bus, master, false),
            // ORI/ANDI/SUBI/ADDI/EORI/CMPI (the to-CCR/to-SR variants
            // land in M5)
            0x0 => {
                if !self.op_imm_alu(opcode, bus, master) {
                    self.finish(4);
                }
            }
            // MOVE.b / MOVE.l / MOVE.w (and MOVEA for An destinations)
            0x1..=0x3 => self.op_move(opcode, bus, master),
            // CHK (line 0x4, bits 8-6 = 110) and LEA (111) share the line
            0x4 if opcode & 0x01C0 == 0x0180 => self.op_chk(opcode, bus, master),
            0x4 if opcode & 0x01C0 == 0x01C0 => self.op_lea_pea(opcode, bus, master, false),
            // Line 0x4 "misc": the unary ALU group, the SR/CCR moves (the
            // size-11 encodings of the unary sub-ops), JMP/JSR/RTS/RTR,
            // MOVEM, and the privileged one-words all live here.
            0x4 => match (opcode >> 8) & 0xF {
                0x0 if opcode & 0x00C0 == 0x00C0 => self.op_move_from_sr(opcode, bus, master),
                0x0 => self.op_unary(opcode, bus, master, UnaryOp::Negx),
                // 0x42C0 (MOVE from CCR) is 68010+; on the 68000 the CLR
                // size-11 hole stays a bounded NOP via op_unary
                0x2 => self.op_unary(opcode, bus, master, UnaryOp::Clr),
                0x4 if opcode & 0x00C0 == 0x00C0 => self.op_move_to_ccr(opcode, bus, master),
                0x4 => self.op_unary(opcode, bus, master, UnaryOp::Neg),
                0x6 if opcode & 0x00C0 == 0x00C0 => self.op_move_to_sr(opcode, bus, master),
                0x6 => self.op_unary(opcode, bus, master, UnaryOp::Not),
                // NBCD (size bits 00); SWAP and PEA (M4) share sub-op 0x8
                0x8 if opcode & 0x00C0 == 0 => self.op_nbcd(opcode, bus, master),
                // SWAP Dn (PEA takes the other EA modes of this encoding)
                0x8 if opcode & 0x00F8 == 0x0040 => self.op_swap(opcode),
                0x8 if opcode & 0x00C0 == 0x0040 => self.op_lea_pea(opcode, bus, master, true),
                // EXT.w / EXT.l (EA mode bits 000); other modes with bit 7
                // set are the MOVEM store direction
                0x8 if opcode & 0x0038 == 0 && opcode & 0x0080 != 0 => self.op_ext(opcode),
                0x8 if opcode & 0x0080 != 0 => self.op_movem(opcode, bus, master, false),
                // ILLEGAL is the one architecturally-guaranteed illegal
                // encoding (a TAS hole); the rest of sub-op 0xA is TST/TAS
                0xA if opcode == 0x4AFC => self.op_illegal(bus, master, 4),
                0xA => self.op_tst(opcode, bus, master),
                // MOVEM load direction (bit 7 clear is unassigned here)
                0xC if opcode & 0x0080 != 0 => self.op_movem(opcode, bus, master, true),
                // 0x4E40-0x4EFF: JMP/JSR plus the one-word specials. NOP,
                // TRAP, LINK/UNLK, MOVE USP, RESET, STOP, RTE, and TRAPV
                // stay bounded NOPs until M4/M5.
                0xE => match (opcode >> 6) & 3 {
                    3 => self.op_jmp_jsr(opcode, bus, master, false),
                    2 => self.op_jmp_jsr(opcode, bus, master, true),
                    1 => match opcode {
                        0x4E40..=0x4E4F => self.op_trap(opcode, bus, master),
                        0x4E50..=0x4E57 => self.op_link(opcode, bus, master),
                        0x4E58..=0x4E5F => self.op_unlk(opcode, bus, master),
                        0x4E60..=0x4E6F => self.op_move_usp(opcode, bus, master),
                        0x4E70 => self.op_reset_instruction(bus, master),
                        0x4E71 => self.finish(4), // NOP
                        0x4E72 => self.op_stop(bus, master),
                        0x4E73 => self.op_rte(bus, master),
                        0x4E75 => self.op_rts(bus, master),
                        0x4E76 => self.op_trapv(bus, master),
                        0x4E77 => self.op_rtr(bus, master),
                        _ => self.finish(4),
                    },
                    _ => self.finish(4),
                },
                _ => self.finish(4),
            },
            // Size bits 11 on line 0x5 split into DBcc (EA mode 001 = An)
            // and Scc (everything else); the other sizes are ADDQ/SUBQ
            0x5 if opmode & 3 == 3 && ea_mode == 1 => self.op_dbcc(opcode, bus, master),
            0x5 if opmode & 3 == 3 => self.op_scc(opcode, bus, master),
            0x5 => self.op_addq_subq(opcode, bus, master),
            // BRA / BSR / Bcc
            0x6 => self.op_bcc(opcode, bus, master),
            // MOVEQ (bit 8 set is unassigned on the 68000)
            0x7 if opcode & 0x0100 == 0 => self.op_moveq(opcode),
            // OR / DIVU / DIVS / SBCD plus the illegal PACK/UNPK slots
            0x8 => match opmode {
                3 => self.op_div(opcode, bus, master, false),
                7 => self.op_div(opcode, bus, master, true),
                4 if ea_mode < 2 => self.op_bcd(opcode, bus, master, false),
                5 | 6 if ea_mode < 2 => self.finish(4), // illegal (PACK/UNPK on 68020+)
                _ => self.op_logical(opcode, bus, master, LogicalOp::Or),
            },
            // SUB / SUBA / SUBX
            0x9 => match opmode {
                4..=6 if ea_mode < 2 => self.op_addx_subx(opcode, bus, master, false),
                _ => self.op_add_sub(opcode, bus, master, false),
            },
            // CMP / CMPA / CMPM / EOR
            0xB => match opmode {
                4..=6 if ea_mode == 1 => self.op_cmpm(opcode, bus, master),
                4..=6 => self.op_logical(opcode, bus, master, LogicalOp::Eor),
                _ => self.op_cmp(opcode, bus, master),
            },
            // AND / MULU / MULS / ABCD; the line also carries EXG (M4)
            0xC => match opmode {
                3 => self.op_mul(opcode, bus, master, false),
                7 => self.op_mul(opcode, bus, master, true),
                4 if ea_mode < 2 => self.op_bcd(opcode, bus, master, true),
                5 | 6 if ea_mode < 2 => self.op_exg(opcode),
                _ => self.op_logical(opcode, bus, master, LogicalOp::And),
            },
            // ADD / ADDA / ADDX
            0xD => match opmode {
                4..=6 if ea_mode < 2 => self.op_addx_subx(opcode, bus, master, true),
                _ => self.op_add_sub(opcode, bus, master, true),
            },
            // Shifts and rotates. Size bits 11 select the one-bit memory
            // form (bit 11 set there is unassigned on the 68000); other
            // sizes are the register form.
            0xE if opmode & 3 == 3 => {
                if opcode & 0x0800 == 0 {
                    self.op_shift_mem(opcode, bus, master);
                } else {
                    self.finish(4);
                }
            }
            0xE => self.op_shift_reg(opcode),
            // Line-A and line-F opcodes are unassigned on the 68000 and
            // vector through their dedicated exceptions
            0xA => self.op_illegal(bus, master, 10),
            0xF => self.op_illegal(bus, master, 11),
            // Remaining unassigned encodings inside implemented lines stay
            // bounded NOPs (full illegal-instruction coverage lands in M7)
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
        // TAS is not implemented until M7; until then it executes as a
        // bounded NOP.
        bus.load(0x1000, &[0x4A, 0xC0]);

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
