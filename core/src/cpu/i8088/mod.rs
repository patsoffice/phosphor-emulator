//! Intel 8088 CPU emulation.
//!
//! The 8088 is the 8-bit external data bus variant of the 8086. Internally it
//! operates on 16-bit data with a segmented 20-bit address space (1 MB).
//! Physical addresses are computed as `(segment << 4) + offset`, masked to
//! 20 bits.
//!
//! This implementation models the CPU at the instruction level: each call to
//! `execute_cycle` runs one bus cycle, with multi-cycle instructions tracked
//! via internal state. The bus interface uses `Address = u32` for 20-bit
//! physical addresses and `Data = u8` for the 8-bit external data bus.

pub mod addressing;
pub mod alu;
pub mod decode;
pub mod execute;
pub mod flags;
pub mod registers;

pub use registers::SegReg;

use crate::core::bus::InterruptState;
use crate::core::component::BusMasterComponent;
use crate::core::{Bus, BusMaster};
use crate::cpu::Cpu;
use crate::cpu::state::CpuStateTrait;
use crate::prelude::Saveable;

/// Execution state machine for multi-cycle instructions.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Execute and Halted used starting in Step 1.3+
pub(crate) enum ExecState {
    /// Ready to fetch the next instruction.
    Fetch,
    /// Executing an instruction: (remaining_cycles).
    /// The instruction has already been decoded and its effect applied on the
    /// first cycle; remaining cycles are bus-idle wait states.
    Execute(u16),
    /// Halted (HLT instruction), waiting for interrupt.
    Halted,
}

/// REP/REPZ/REPNZ prefix state.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RepPrefix {
    Rep,   // REP (MOVS/STOS/LODS/INS/OUTS) or REPZ (CMPS/SCAS)
    Repnz, // REPNZ (CMPS/SCAS)
}

/// Interrupt type for the 8088 interrupt response sequence.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub(crate) enum InterruptType {
    /// Non-maskable interrupt (vector 2)
    Nmi = 0,
    /// Maskable hardware interrupt (vector from PIC)
    Irq = 1,
    /// Software interrupt (INT n instruction)
    Software = 2,
}

/// Fields are ordered to match the save-state serialization layout (version 1).
#[derive(Saveable)]
#[save_version(1)]
pub struct I8088 {
    // General-purpose registers (accessible as 16-bit or 8-bit halves)
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,

    // Index registers
    pub si: u16,
    pub di: u16,

    // Pointer registers
    pub bp: u16,
    pub sp: u16,

    // Segment registers
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,

    // Instruction pointer
    pub ip: u16,

    // FLAGS register (16-bit, with always-one bits)
    pub flags: u16,

    // Interrupt state (nmi_prev before nmi_pending to match save-state order)
    pub(crate) nmi_prev: bool,
    pub(crate) nmi_pending: bool,

    // Internal state (not serialized)
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    #[save_skip(default)]
    pub(crate) segment_override: Option<SegReg>,
    #[save_skip(default)]
    pub(crate) rep_prefix: Option<RepPrefix>,
    #[save_skip(default)]
    pub(crate) irq_line: bool,
    // Cycle counter (total bus cycles executed, not serialized, keeps current value)
    #[save_skip]
    pub(crate) clock: u64,
}

impl Default for I8088 {
    fn default() -> Self {
        Self::new()
    }
}

impl I8088 {
    pub fn new() -> Self {
        Self {
            ax: 0,
            bx: 0,
            cx: 0,
            dx: 0,
            si: 0,
            di: 0,
            bp: 0,
            sp: 0,
            // Reset state: CS=0xFFFF, all others 0
            cs: 0xFFFF,
            ds: 0,
            es: 0,
            ss: 0,
            ip: 0,
            flags: flags::normalize(0),
            state: ExecState::Fetch,
            segment_override: None,
            rep_prefix: None,
            nmi_pending: false,
            nmi_prev: false,
            irq_line: false,
            clock: 0,
        }
    }

    /// Returns true when the CPU is at an instruction boundary (ready to fetch).
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }

    /// Total bus cycles executed since creation.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Execute one bus cycle.
    pub fn execute_cycle<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.clock += 1;

        match self.state {
            ExecState::Fetch => {
                // Check for interrupts before fetching
                let ints = bus.check_interrupts(master);
                if self.handle_interrupts(ints, bus, master) {
                    return;
                }

                // Consume any prefix bytes and fetch the opcode
                let opcode = self.consume_prefixes(bus, master);

                // Execute the instruction
                self.execute(opcode, bus, master);
            }
            ExecState::Execute(remaining) => {
                if remaining <= 1 {
                    self.state = ExecState::Fetch;
                } else {
                    self.state = ExecState::Execute(remaining - 1);
                }
            }
            ExecState::Halted => {
                // Check for interrupts that can wake us
                let ints = bus.check_interrupts(master);
                let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_prev);
                if nmi_edge {
                    self.nmi_pending = true;
                }
                if self.nmi_pending {
                    self.nmi_pending = false;
                    self.state = ExecState::Fetch;
                    self.interrupt(bus, master, 2);
                } else if ints.irq && flags::get(self.flags, flags::Flag::IF) {
                    self.state = ExecState::Fetch;
                    self.interrupt(bus, master, ints.irq_vector);
                }
            }
        }
    }

    /// Check for pending interrupts. Returns true if an interrupt was taken.
    fn handle_interrupts<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        ints: InterruptState,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        // NMI is edge-triggered
        let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_prev);
        if nmi_edge {
            self.nmi_pending = true;
        }

        // NMI takes priority over IRQ
        if self.nmi_pending {
            self.nmi_pending = false;
            self.interrupt(bus, master, 2); // NMI = vector 2
            return true;
        }

        // IRQ: level-triggered, masked by IF
        if ints.irq && flags::get(self.flags, flags::Flag::IF) {
            self.interrupt(bus, master, ints.irq_vector);
            return true;
        }

        false
    }

    /// Default segment for a given addressing mode base register.
    /// BP-based addressing uses SS; everything else uses DS.
    #[inline]
    pub fn default_segment_for_rm(&self, rm: u8, mod_bits: u8) -> SegReg {
        match rm & 7 {
            // [BP+SI], [BP+DI], [BP+disp]
            2 | 3 => SegReg::SS,
            // [BP] only when mod != 00 (mod=00 rm=110 is direct addressing, uses DS)
            6 if mod_bits != 0 => SegReg::SS,
            _ => SegReg::DS,
        }
    }

    /// Resolve the effective segment: use override if active, else the default.
    #[inline]
    pub fn effective_segment(&self, default: SegReg) -> u16 {
        self.get_seg(self.segment_override.unwrap_or(default))
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl BusMasterComponent for I8088 {
    type Bus = dyn Bus<Address = u32, Data = u8>;

    fn tick_with_bus(&mut self, bus: &mut Self::Bus, master: BusMaster) -> bool {
        self.execute_cycle(bus, master);
        matches!(self.state, ExecState::Fetch)
    }
}

impl Cpu for I8088 {
    fn reset(&mut self, bus: &mut Self::Bus, master: BusMaster) {
        self.ax = 0;
        self.bx = 0;
        self.cx = 0;
        self.dx = 0;
        self.si = 0;
        self.di = 0;
        self.bp = 0;
        self.sp = 0;
        self.cs = 0xFFFF;
        self.ds = 0;
        self.es = 0;
        self.ss = 0;
        self.ip = 0;
        self.flags = flags::normalize(0);
        self.state = ExecState::Fetch;
        self.segment_override = None;
        self.rep_prefix = None;
        self.nmi_pending = false;
        self.nmi_prev = false;
        self.irq_line = false;

        // The 8088 starts executing at CS:IP = FFFF:0000 (physical 0xFFFF0).
        // Unlike 6502/6809 which read a reset vector, the 8088 simply begins
        // execution at the fixed address. The ROM at that address typically
        // contains a far JMP to the actual entry point.
        //
        // Read the first byte to verify the bus is alive (matches hardware
        // behavior of the first fetch cycle after reset).
        let _first = bus.read(master, 0xFFFF0);
        // IP stays at 0; CS stays at 0xFFFF. Execution will proceed from FFFF:0000.
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {
        // External interrupt lines are handled in check_interrupts via the bus
    }

    fn is_sleeping(&self) -> bool {
        matches!(self.state, ExecState::Halted)
    }
}

// ---------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------

/// I8088 CPU state snapshot for debugging and save states.
#[derive(Debug, Clone, PartialEq)]
pub struct I8088State {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub ip: u16,
    pub flags: u16,
}

impl CpuStateTrait for I8088 {
    type Snapshot = I8088State;

    fn snapshot(&self) -> I8088State {
        I8088State {
            ax: self.ax,
            bx: self.bx,
            cx: self.cx,
            dx: self.dx,
            si: self.si,
            di: self.di,
            bp: self.bp,
            sp: self.sp,
            cs: self.cs,
            ds: self.ds,
            es: self.es,
            ss: self.ss,
            ip: self.ip,
            flags: self.flags,
        }
    }
}

// ---------------------------------------------------------------------------
// Debug support
// ---------------------------------------------------------------------------

use crate::core::debug::{DebugRegister, Debuggable};

impl I8088State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "CS:IP",
                value: ((self.cs as u64) << 16) | self.ip as u64,
                width: 32,
            },
            DebugRegister {
                name: "AX",
                value: self.ax as u64,
                width: 16,
            },
            DebugRegister {
                name: "BX",
                value: self.bx as u64,
                width: 16,
            },
            DebugRegister {
                name: "CX",
                value: self.cx as u64,
                width: 16,
            },
            DebugRegister {
                name: "DX",
                value: self.dx as u64,
                width: 16,
            },
            DebugRegister {
                name: "SI",
                value: self.si as u64,
                width: 16,
            },
            DebugRegister {
                name: "DI",
                value: self.di as u64,
                width: 16,
            },
            DebugRegister {
                name: "BP",
                value: self.bp as u64,
                width: 16,
            },
            DebugRegister {
                name: "SP",
                value: self.sp as u64,
                width: 16,
            },
            DebugRegister {
                name: "DS",
                value: self.ds as u64,
                width: 16,
            },
            DebugRegister {
                name: "ES",
                value: self.es as u64,
                width: 16,
            },
            DebugRegister {
                name: "SS",
                value: self.ss as u64,
                width: 16,
            },
            DebugRegister {
                name: "FLAGS",
                value: self.flags as u64,
                width: 16,
            },
        ]
    }
}

impl Debuggable for I8088 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl crate::core::debug::DebugCpu for I8088 {
    fn debug_pc(&self) -> u32 {
        u32::from(self.ip)
    }

    fn debug_at_instruction_boundary(&self) -> bool {
        self.at_instruction_boundary()
    }

    fn debug_disassemble(
        &self,
        _addr: u32,
        bytes: &[u8],
    ) -> crate::cpu::disasm::DisassembledInstruction {
        // Stub disassembler: show raw opcode byte. Full x86 disassembly TBD.
        let opcode = if bytes.is_empty() { 0 } else { bytes[0] };
        crate::cpu::disasm::DisassembledInstruction {
            mnemonic: "DB",
            operands: format!("${opcode:02X}"),
            byte_len: 1,
            bytes: [opcode, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            target_addr: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reset_state() {
        let cpu = I8088::new();
        assert_eq!(cpu.cs, 0xFFFF);
        assert_eq!(cpu.ip, 0x0000);
        assert_eq!(cpu.ax, 0);
        assert_eq!(cpu.ds, 0);
        assert_eq!(cpu.sp, 0);
        assert!(cpu.at_instruction_boundary());
    }

    #[test]
    fn flags_normalized_on_new() {
        let cpu = I8088::new();
        // Always-one bits should be set
        assert_ne!(cpu.flags & 0x0002, 0); // bit 1
        assert_eq!(cpu.flags & 0xF000, 0xF000); // bits 12-15
    }

    #[test]
    fn snapshot_round_trip() {
        let mut cpu = I8088::new();
        cpu.ax = 0x1234;
        cpu.bx = 0x5678;
        cpu.cs = 0xABCD;
        cpu.ip = 0xEF01;
        let snap = cpu.snapshot();
        assert_eq!(snap.ax, 0x1234);
        assert_eq!(snap.bx, 0x5678);
        assert_eq!(snap.cs, 0xABCD);
        assert_eq!(snap.ip, 0xEF01);
    }

    #[test]
    fn default_segment_for_bp() {
        let cpu = I8088::new();
        // rm=6 with mod=01 or mod=10 (BP-based) → SS
        assert_eq!(cpu.default_segment_for_rm(6, 1), SegReg::SS);
        assert_eq!(cpu.default_segment_for_rm(6, 2), SegReg::SS);
        // rm=6 with mod=00 → direct addressing → DS
        assert_eq!(cpu.default_segment_for_rm(6, 0), SegReg::DS);
        // rm=2 ([BP+SI]) → SS regardless of mod
        assert_eq!(cpu.default_segment_for_rm(2, 0), SegReg::SS);
        assert_eq!(cpu.default_segment_for_rm(2, 1), SegReg::SS);
        // rm=7 ([BX]) → DS
        assert_eq!(cpu.default_segment_for_rm(7, 0), SegReg::DS);
    }

    #[test]
    fn effective_segment_default() {
        let mut cpu = I8088::new();
        cpu.ds = 0x1000;
        cpu.ss = 0x2000;
        cpu.segment_override = None;
        assert_eq!(cpu.effective_segment(SegReg::DS), 0x1000);
        assert_eq!(cpu.effective_segment(SegReg::SS), 0x2000);
    }

    #[test]
    fn effective_segment_override() {
        let mut cpu = I8088::new();
        cpu.ds = 0x1000;
        cpu.es = 0x3000;
        cpu.segment_override = Some(SegReg::ES);
        // Override forces ES regardless of default
        assert_eq!(cpu.effective_segment(SegReg::DS), 0x3000);
    }

    #[test]
    fn is_sleeping_when_halted() {
        let mut cpu = I8088::new();
        assert!(!cpu.is_sleeping());
        cpu.state = ExecState::Halted;
        assert!(cpu.is_sleeping());
    }
}
