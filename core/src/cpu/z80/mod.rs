mod alu;
mod bit;
mod block;
mod branch;
pub mod disasm;
mod load_store;
mod stack;

use crate::core::{Bus, BusMaster, bus::InterruptState, component::BusMasterComponent};
use crate::cpu::{
    Cpu,
    state::{CpuStateTrait, Z80State},
};
use crate::prelude::Saveable;

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum Flag {
    C = 0x01,  // Carry
    N = 0x02,  // Add/Subtract
    PV = 0x04, // Parity/Overflow
    X = 0x08,  // Unused (copy of bit 3)
    H = 0x10,  // Half Carry
    Y = 0x20,  // Unused (copy of bit 5)
    Z = 0x40,  // Zero
    S = 0x80,  // Sign
}

/// `Clone` snapshots the complete T-state-level machine state, including the
/// mid-instruction temporaries. Boards that model the WAIT input use this to
/// re-run a stalled bus cycle: snapshot before `execute_cycle`, restore if the
/// bus asserted WAIT during the access, and step again once WAIT releases —
/// which is what the real chip does when it holds the address on the bus and
/// latches the data only after WAIT goes away.
#[derive(Clone, Saveable)]
#[save_version(1)]
pub struct Z80 {
    // Registers
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    // Shadow Registers
    pub a_prime: u8,
    pub f_prime: u8,
    pub b_prime: u8,
    pub c_prime: u8,
    pub d_prime: u8,
    pub e_prime: u8,
    pub h_prime: u8,
    pub l_prime: u8,
    // Index & Special Registers (sp/pc before i/r to match save-state order)
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,
    pub r: u8,

    // Internal state
    pub iff1: bool,
    pub iff2: bool,
    pub im: u8,
    pub memptr: u16, // Hidden WZ register
    pub halted: bool,
    pub ei_delay: bool,
    pub p: bool,           // Set after LD A,I / LD A,R for interrupt PV behavior
    pub q: u8, // Copy of F when instruction modifies flags, 0 otherwise (for SCF/CCF X/Y)
    pub(crate) prev_q: u8, // Previous instruction's q value (saved at Fetch for SCF/CCF)

    // Interrupt state
    pub(crate) nmi_previous: bool,

    // Execution temporaries — not saved, reset to defaults on load
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    #[save_skip(default)]
    pub(crate) opcode: u8,
    #[save_skip(default)]
    pub(crate) temp_addr: u16,
    #[save_skip(default)]
    pub(crate) temp_data: u8,

    // Prefix handling — not saved, reset to defaults on load
    #[save_skip(default = IndexMode::HL)]
    pub(crate) index_mode: IndexMode,
    #[save_skip(default)]
    pub(crate) prefix_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IndexMode {
    HL,
    IX,
    IY,
}

/// Z80 interrupt response type, stored in ExecState::Interrupt.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum InterruptType {
    Nmi = 0,
    IrqIm01 = 1,
    IrqIm2 = 2,
}

#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    Fetch,           // M1 T1: address on bus, reset prefix state
    FetchRead,       // M1 T2: read opcode, increment PC, refresh R
    Execute(u8, u8), // (opcode, cycle)

    // Prefix States
    PrefixCB(u8),
    ExecuteCB(u8, u8),
    PrefixED(u8),
    ExecuteED(u8, u8),
    // Indexed CB (DD CB d o / FD CB d o)
    #[allow(non_camel_case_types)]
    PrefixIndexCB_ReadOffset(u8),
    #[allow(non_camel_case_types)]
    PrefixIndexCB_FetchOp(u8),
    ExecuteIndexCB(u8, u8),

    /// Interrupt response sequence (type, cycle counter).
    Interrupt(InterruptType, u8),
}

impl Default for Z80 {
    fn default() -> Self {
        Self::new()
    }
}

impl Z80 {
    pub fn new() -> Self {
        Self {
            a: 0xFF,
            f: 0xFF,
            b: 0xFF,
            c: 0xFF,
            d: 0xFF,
            e: 0xFF,
            h: 0xFF,
            l: 0xFF,
            a_prime: 0xFF,
            f_prime: 0xFF,
            b_prime: 0xFF,
            c_prime: 0xFF,
            d_prime: 0xFF,
            e_prime: 0xFF,
            h_prime: 0xFF,
            l_prime: 0xFF,
            ix: 0xFFFF,
            iy: 0xFFFF,
            sp: 0xFFFF,
            pc: 0x0000,
            i: 0,
            r: 0,
            iff1: false,
            iff2: false,
            im: 0,
            memptr: 0,
            halted: false,
            ei_delay: false,
            p: false,
            q: 0,
            prev_q: 0,
            nmi_previous: false,
            state: ExecState::Fetch,
            opcode: 0,
            temp_addr: 0,
            temp_data: 0,
            index_mode: IndexMode::HL,
            prefix_pending: false,
        }
    }

    /// Hardware reset — restores all CPU state to power-on defaults.
    /// Equivalent to pulling the /RESET pin low.
    pub fn hardware_reset(&mut self) {
        self.pc = 0x0000;
        self.sp = 0xFFFF;
        self.a = 0xFF;
        self.f = 0xFF;
        self.i = 0;
        self.r = 0;
        self.im = 0;
        self.iff1 = false;
        self.iff2 = false;
        self.halted = false;
        self.ei_delay = false;
        self.memptr = 0;
        self.nmi_previous = false;
        // Reset execution state machine
        self.state = ExecState::Fetch;
        self.prefix_pending = false;
        self.index_mode = IndexMode::HL;
    }

    // Helpers for 16-bit register access
    pub fn get_bc(&self) -> u16 {
        ((self.b as u16) << 8) | self.c as u16
    }
    pub fn set_bc(&mut self, val: u16) {
        self.b = (val >> 8) as u8;
        self.c = val as u8;
    }

    pub fn get_de(&self) -> u16 {
        ((self.d as u16) << 8) | self.e as u16
    }
    pub fn set_de(&mut self, val: u16) {
        self.d = (val >> 8) as u8;
        self.e = val as u8;
    }

    pub fn get_hl(&self) -> u16 {
        ((self.h as u16) << 8) | self.l as u16
    }
    pub fn set_hl(&mut self, val: u16) {
        self.h = (val >> 8) as u8;
        self.l = val as u8;
    }

    pub fn get_af(&self) -> u16 {
        ((self.a as u16) << 8) | self.f as u16
    }
    pub fn set_af(&mut self, val: u16) {
        self.a = (val >> 8) as u8;
        self.f = val as u8;
    }

    /// Get 8-bit register by index, respecting IX/IY prefix for H/L (undocumented IXH/IXL/IYH/IYL).
    /// Index 6 is NOT handled here — callers must handle (HL)/(IX+d)/(IY+d) separately.
    pub fn get_reg8_ix(&self, index: u8) -> u8 {
        match (index, self.index_mode) {
            (4, IndexMode::IX) => (self.ix >> 8) as u8,
            (5, IndexMode::IX) => self.ix as u8,
            (4, IndexMode::IY) => (self.iy >> 8) as u8,
            (5, IndexMode::IY) => self.iy as u8,
            _ => self.get_reg8(index),
        }
    }

    pub fn set_reg8_ix(&mut self, index: u8, val: u8) {
        match (index, self.index_mode) {
            (4, IndexMode::IX) => self.ix = (self.ix & 0x00FF) | ((val as u16) << 8),
            (5, IndexMode::IX) => self.ix = (self.ix & 0xFF00) | val as u16,
            (4, IndexMode::IY) => self.iy = (self.iy & 0x00FF) | ((val as u16) << 8),
            (5, IndexMode::IY) => self.iy = (self.iy & 0xFF00) | val as u16,
            _ => self.set_reg8(index, val),
        }
    }

    /// Get the effective address for (HL)/(IX+d)/(IY+d).
    /// For IX/IY modes, the displacement must already be stored in temp_data (as signed byte).
    pub(crate) fn get_index_addr(&self) -> u16 {
        match self.index_mode {
            IndexMode::HL => self.get_hl(),
            IndexMode::IX => self.ix.wrapping_add(self.temp_data as i8 as i16 as u16),
            IndexMode::IY => self.iy.wrapping_add(self.temp_data as i8 as i16 as u16),
        }
    }

    /// Get 16-bit register pair by index (0=BC, 1=DE, 2=HL/IX/IY, 3=SP).
    /// Index 2 respects current index_mode for DD/FD prefixed instructions.
    pub(crate) fn get_rp(&self, index: u8) -> u16 {
        match index {
            0 => self.get_bc(),
            1 => self.get_de(),
            2 => match self.index_mode {
                IndexMode::HL => self.get_hl(),
                IndexMode::IX => self.ix,
                IndexMode::IY => self.iy,
            },
            3 => self.sp,
            _ => unreachable!("get_rp called with index {}", index),
        }
    }

    /// Set 16-bit register pair by index (0=BC, 1=DE, 2=HL/IX/IY, 3=SP).
    pub(crate) fn set_rp(&mut self, index: u8, val: u16) {
        match index {
            0 => self.set_bc(val),
            1 => self.set_de(val),
            2 => match self.index_mode {
                IndexMode::HL => self.set_hl(val),
                IndexMode::IX => self.ix = val,
                IndexMode::IY => self.iy = val,
            },
            3 => self.sp = val,
            _ => unreachable!("set_rp called with index {}", index),
        }
    }

    /// Get 16-bit register pair by index for PUSH/POP (0=BC, 1=DE, 2=HL/IX/IY, 3=AF).
    pub(crate) fn get_rp_af(&self, index: u8) -> u16 {
        match index {
            0 => self.get_bc(),
            1 => self.get_de(),
            2 => match self.index_mode {
                IndexMode::HL => self.get_hl(),
                IndexMode::IX => self.ix,
                IndexMode::IY => self.iy,
            },
            3 => self.get_af(),
            _ => unreachable!("get_rp_af called with index {}", index),
        }
    }

    /// Set 16-bit register pair by index for PUSH/POP (0=BC, 1=DE, 2=HL/IX/IY, 3=AF).
    pub(crate) fn set_rp_af(&mut self, index: u8, val: u16) {
        match index {
            0 => self.set_bc(val),
            1 => self.set_de(val),
            2 => match self.index_mode {
                IndexMode::HL => self.set_hl(val),
                IndexMode::IX => self.ix = val,
                IndexMode::IY => self.iy = val,
            },
            3 => self.set_af(val),
            _ => unreachable!("set_rp_af called with index {}", index),
        }
    }

    pub fn get_reg8(&self, index: u8) -> u8 {
        match index {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            7 => self.a,
            _ => unreachable!("get_reg8 called with index {}", index),
        }
    }

    pub fn set_reg8(&mut self, index: u8, val: u8) {
        match index {
            0 => self.b = val,
            1 => self.c = val,
            2 => self.d = val,
            3 => self.e = val,
            4 => self.h = val,
            5 => self.l = val,
            7 => self.a = val,
            _ => unreachable!("set_reg8 called with index {}", index),
        }
    }

    pub fn execute_cycle<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        match self.state {
            ExecState::Fetch => {
                // Check for interrupts at instruction boundary (not during prefix chains)
                if !self.prefix_pending {
                    if self.ei_delay {
                        // EI delay: skip interrupt check for one instruction after EI
                        self.ei_delay = false;
                    } else {
                        let ints = bus.check_interrupts(master);

                        // NMI: edge-triggered (higher priority than IRQ)
                        let nmi_edge =
                            crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_previous);

                        if nmi_edge {
                            if self.halted {
                                self.halted = false;
                            }
                            self.state = ExecState::Interrupt(InterruptType::Nmi, 0);
                            return;
                        }

                        // IRQ: level-triggered, masked by IFF1
                        if ints.irq && self.iff1 {
                            if self.halted {
                                self.halted = false;
                            }
                            let int_type = if self.im == 2 {
                                InterruptType::IrqIm2
                            } else {
                                InterruptType::IrqIm01
                            };
                            self.state = ExecState::Interrupt(int_type, 0);
                            return;
                        }
                    }
                }

                // HALT: re-fetch the HALT opcode for a proper 4T NOP cycle.
                // PC already points past HALT; decrement so FetchRead re-reads
                // it and increments PC back. This gives correct 4T timing with
                // R refresh, matching real Z80 behavior.
                if self.halted {
                    self.pc = self.pc.wrapping_sub(1);
                    self.state = ExecState::FetchRead;
                    return;
                }

                // M1 T1: address on bus, reset prefix state
                if !self.prefix_pending {
                    self.index_mode = IndexMode::HL;
                    self.p = false;
                    self.prev_q = self.q;
                    self.q = 0;
                }
                self.prefix_pending = false;
                self.state = ExecState::FetchRead;
            }
            ExecState::FetchRead => {
                // M1 T2: read opcode, increment PC, refresh R
                self.opcode = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                self.state = ExecState::Execute(self.opcode, 0);
            }
            ExecState::Execute(op, cyc) => {
                self.execute_instruction(op, cyc, bus, master);
            }
            ExecState::PrefixCB(cyc) => {
                // CB prefix M1 fetch: 3 states (T5-T7)
                // cyc 0 = T1 (address on bus)
                // cyc 1 = T2 (read CB opcode, R refresh)
                // cyc 2 → dispatch to ExecuteCB (T4 of CB M1 = first handler cycle)
                match cyc {
                    0 => self.state = ExecState::PrefixCB(1),
                    1 => {
                        self.opcode = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                        self.state = ExecState::PrefixCB(2);
                    }
                    2 => self.state = ExecState::ExecuteCB(self.opcode, 0),
                    _ => unreachable!(),
                }
            }
            ExecState::ExecuteCB(op, cyc) => {
                self.execute_instruction_cb(op, cyc, bus, master);
            }
            ExecState::PrefixED(cyc) => {
                // ED prefix M1 fetch: 3 states
                // cyc 0 = T1, cyc 1 = T2 (read), cyc 2 → dispatch to ExecuteED
                match cyc {
                    0 => self.state = ExecState::PrefixED(1),
                    1 => {
                        self.opcode = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                        self.state = ExecState::PrefixED(2);
                    }
                    2 => self.state = ExecState::ExecuteED(self.opcode, 0),
                    _ => unreachable!(),
                }
            }
            ExecState::ExecuteED(op, cyc) => {
                self.execute_instruction_ed(op, cyc, bus, master);
            }
            ExecState::PrefixIndexCB_ReadOffset(cyc) => {
                // Read displacement byte for DD CB d op / FD CB d op
                // 3 states: 0=pad, 1=read d from PC, 2=pad → FetchOp
                match cyc {
                    0 => self.state = ExecState::PrefixIndexCB_ReadOffset(1),
                    1 => {
                        self.temp_data = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.state = ExecState::PrefixIndexCB_ReadOffset(2);
                    }
                    2 => self.state = ExecState::PrefixIndexCB_FetchOp(0),
                    _ => unreachable!(),
                }
            }
            ExecState::PrefixIndexCB_FetchOp(cyc) => {
                // Fetch CB sub-opcode + 2 internal cycles for address computation
                // 5 states: 0=pad, 1=read opcode, 2=internal, 3=internal, 4=dispatch
                match cyc {
                    0 | 2 | 3 => self.state = ExecState::PrefixIndexCB_FetchOp(cyc + 1),
                    1 => {
                        // Read CB sub-opcode as data (NOT an M1 cycle — no R increment)
                        self.opcode = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        // Compute indexed address and store in temp_addr
                        self.temp_addr = self.get_index_addr();
                        self.state = ExecState::PrefixIndexCB_FetchOp(2);
                    }
                    4 => self.state = ExecState::ExecuteIndexCB(self.opcode, 0),
                    _ => unreachable!(),
                }
            }
            ExecState::ExecuteIndexCB(op, cyc) => {
                self.execute_instruction_index_cb(op, cyc, bus, master);
            }
            ExecState::Interrupt(int_type, cyc) => {
                self.execute_interrupt(int_type, cyc, bus, master);
            }
        }
    }

    /// Interrupt response handler.
    /// The Fetch cycle that detected the interrupt counts as T1, so handler cycles
    /// are T2..Tn: NMI 10 cycles (11T), IRQ IM1 12 cycles (13T), IRQ IM2 18 cycles (19T).
    fn execute_interrupt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        int_type: InterruptType,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match int_type {
            // NMI — 11T total (1 Fetch + 10 handler cycles 0-9)
            InterruptType::Nmi => match cycle {
                0 => {
                    // Acknowledge + disable IFF1 (IFF2 preserved for RETN)
                    self.iff1 = false;
                    self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                    self.state = ExecState::Interrupt(int_type, 1);
                }
                1 | 2 | 4 | 5 | 7 | 8 => {
                    self.state = ExecState::Interrupt(int_type, cycle + 1);
                }
                3 => {
                    // Push PC high
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, (self.pc >> 8) as u8);
                    self.state = ExecState::Interrupt(int_type, 4);
                }
                6 => {
                    // Push PC low
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, self.pc as u8);
                    self.state = ExecState::Interrupt(int_type, 7);
                }
                9 => {
                    // Jump to NMI vector
                    self.pc = 0x0066;
                    self.memptr = self.pc;
                    self.state = ExecState::Fetch;
                }
                _ => unreachable!(),
            },
            // IRQ IM0/IM1 — 13T total (1 Fetch + 12 handler cycles 0-11)
            InterruptType::IrqIm01 => match cycle {
                0 => {
                    // Acknowledge + disable interrupts
                    self.iff1 = false;
                    self.iff2 = false;
                    self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                    self.state = ExecState::Interrupt(int_type, 1);
                }
                1 | 2 | 3 | 4 | 6 | 7 | 8 | 10 => {
                    self.state = ExecState::Interrupt(int_type, cycle + 1);
                }
                5 => {
                    // Push PC high
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, (self.pc >> 8) as u8);
                    self.state = ExecState::Interrupt(int_type, 6);
                }
                9 => {
                    // Push PC low
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, self.pc as u8);
                    self.state = ExecState::Interrupt(int_type, 10);
                }
                11 => {
                    // Jump to IM1 vector (0x0038)
                    // IM0: data bus typically 0xFF (RST 38h), same effect
                    self.pc = 0x0038;
                    self.memptr = self.pc;
                    self.state = ExecState::Fetch;
                }
                _ => unreachable!(),
            },
            // IRQ IM2 — 19T total (1 Fetch + 18 handler cycles 0-17)
            InterruptType::IrqIm2 => match cycle {
                0 => {
                    self.iff1 = false;
                    self.iff2 = false;
                    self.r = (self.r & 0x80) | (self.r.wrapping_add(1) & 0x7F);
                    self.state = ExecState::Interrupt(int_type, 1);
                }
                1 | 2 | 3 | 4 | 6 | 7 | 8 | 10 | 11 | 13 | 14 | 16 => {
                    self.state = ExecState::Interrupt(int_type, cycle + 1);
                }
                5 => {
                    // Push PC high
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, (self.pc >> 8) as u8);
                    self.state = ExecState::Interrupt(int_type, 6);
                }
                9 => {
                    // Push PC low
                    self.sp = self.sp.wrapping_sub(1);
                    bus.write(master, self.sp, self.pc as u8);
                    self.state = ExecState::Interrupt(int_type, 10);
                }
                12 => {
                    // Read vector low byte: Z80 places I on upper address bus,
                    // interrupting device places vector byte on data bus
                    let ints = bus.check_interrupts(master);
                    self.temp_addr = ((self.i as u16) << 8) | (ints.irq_vector as u16);
                    self.temp_data = bus.read(master, self.temp_addr);
                    self.state = ExecState::Interrupt(int_type, 13);
                }
                15 => {
                    // Read vector high byte
                    let high = bus.read(master, self.temp_addr.wrapping_add(1));
                    self.pc = ((high as u16) << 8) | self.temp_data as u16;
                    self.memptr = self.pc;
                    self.state = ExecState::Interrupt(int_type, 16);
                }
                17 => {
                    self.state = ExecState::Fetch;
                }
                _ => unreachable!(),
            },
        }
    }

    /// M1 T3 overhead (1 cycle), then dispatch to handlers.
    /// Handlers receive raw cycle numbers starting at 1 (T4 of M1).
    /// Total T-states: Fetch(T1) + FetchRead(T2) + overhead(T3) + handler cycles.
    /// 4T instruction: handler cycle 1 → Fetch (1 handler cycle, total 4).
    /// 7T instruction: handler cycles 1-4 (4 handler cycles, total 7).
    fn execute_instruction<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // M1 T3: shared overhead (refresh)
        if cycle == 0 {
            self.state = ExecState::Execute(opcode, 1);
            return;
        }

        match opcode {
            // NOP — 4 T: M1 only. No flags affected.
            0x00 => self.state = ExecState::Fetch,

            // HALT — 4 T: M1 only. No flags affected. PC stays past HALT (already incremented by FetchRead).
            0x76 => {
                self.halted = true;
                self.state = ExecState::Fetch;
            }

            // Prefixes — 4 T each (M1 only)
            0xCB => {
                if self.index_mode != IndexMode::HL {
                    // DD CB d op / FD CB d op: indexed bit operations
                    self.state = ExecState::PrefixIndexCB_ReadOffset(0);
                } else {
                    self.state = ExecState::PrefixCB(0);
                }
            }
            0xED => {
                self.index_mode = IndexMode::HL;
                self.state = ExecState::PrefixED(0);
            }
            0xDD => {
                self.index_mode = IndexMode::IX;
                self.prefix_pending = true;
                self.state = ExecState::Fetch;
            }
            0xFD => {
                self.index_mode = IndexMode::IY;
                self.prefix_pending = true;
                self.state = ExecState::Fetch;
            }

            // --- Load/Store ---

            // LD (BC), A — 7 T
            0x02 => self.op_ld_bc_a(opcode, cycle, bus, master),
            // LD (DE), A — 7 T
            0x12 => self.op_ld_de_a(opcode, cycle, bus, master),
            // LD (nn), HL — 16 T
            0x22 => self.op_ld_nn_hl(opcode, cycle, bus, master),
            // LD (nn), A — 13 T
            0x32 => self.op_ld_nn_a(opcode, cycle, bus, master),

            // EX AF, AF' — 4 T
            0x08 => self.op_ex_af_af(),

            // LD A, (BC) — 7 T
            0x0A => self.op_ld_a_bc(opcode, cycle, bus, master),
            // LD A, (DE) — 7 T
            0x1A => self.op_ld_a_de(opcode, cycle, bus, master),
            // LD HL, (nn) — 16 T
            0x2A => self.op_ld_hl_nn_ind(opcode, cycle, bus, master),
            // LD A, (nn) — 13 T
            0x3A => self.op_ld_a_nn(opcode, cycle, bus, master),

            // LD rr, nn (0x01/0x11/0x21/0x31) — 10 T
            op if (op & 0xCF) == 0x01 => self.op_ld_rr_nn(op, cycle, bus, master),

            // LD r, n (0x06, 0x0E, ... 0x3E) — 7 T: M1 + MR
            op if (op & 0xC7) == 0x06 => self.op_ld_r_n(op, cycle, bus, master),

            // LD r, r' (0x40-0x7F excluding 0x76) — 4/7 T
            op if (op & 0xC0) == 0x40 => self.op_ld_r_r(op, cycle, bus, master),

            // LD SP, HL — 6 T
            0xF9 => self.op_ld_sp_hl(opcode, cycle, bus, master),

            // EX DE, HL — 4 T
            0xEB => self.op_ex_de_hl(),
            // EXX — 4 T
            0xD9 => self.op_exx(),
            // EX (SP), HL — 19 T
            0xE3 => self.op_ex_sp_hl(opcode, cycle, bus, master),

            // --- Stack ---

            // PUSH rr (0xC5/D5/E5/F5) — 11 T
            op if (op & 0xCF) == 0xC5 => self.op_push(op, cycle, bus, master),
            // POP rr (0xC1/D1/E1/F1) — 10 T
            op if (op & 0xCF) == 0xC1 => self.op_pop(op, cycle, bus, master),

            // --- ALU ---

            // ALU A, r (0x80 - 0xBF) — 4 T (reg) or 7 T ((HL))
            op if (op & 0xC0) == 0x80 => self.op_alu_r(op, cycle, bus, master),
            // ALU A, n (0xC6, 0xCE, ... 0xFE) — 7 T: M1 + MR
            op if (op & 0xC7) == 0xC6 => self.op_alu_n(op, cycle, bus, master),

            // INC r (0x04, 0x0C...) — 4 T (reg) or 11 T ((HL))
            op if (op & 0xC7) == 0x04 => self.op_inc_dec_r(op, cycle, bus, master),
            // DEC r (0x05, 0x0D...) — 4 T (reg) or 11 T ((HL))
            op if (op & 0xC7) == 0x05 => self.op_inc_dec_r(op, cycle, bus, master),

            // ADD HL,rr (0x09/0x19/0x29/0x39) — 11 T
            op if (op & 0xCF) == 0x09 => self.op_add_hl_rr(op, cycle),
            // INC rr (0x03/0x13/0x23/0x33) — 6 T
            op if (op & 0xCF) == 0x03 => self.op_inc_dec_rr(op, cycle),
            // DEC rr (0x0B/0x1B/0x2B/0x3B) — 6 T
            op if (op & 0xCF) == 0x0B => self.op_inc_dec_rr(op, cycle),

            // Accumulator rotates — 4 T
            0x07 => self.op_rlca(),
            0x0F => self.op_rrca(),
            0x17 => self.op_rla(),
            0x1F => self.op_rra(),

            // Misc ALU — 4 T
            0x27 => self.op_daa(),
            0x2F => self.op_cpl(),
            0x37 => self.op_scf(),
            0x3F => self.op_ccf(),

            // --- Branch/Control Flow ---

            // JP nn — 10 T
            0xC3 => self.op_jp_nn(opcode, cycle, bus, master),
            // JP (HL) — 4 T
            0xE9 => self.op_jp_hl(),
            // JR e — 12 T
            0x18 => self.op_jr_e(opcode, cycle, bus, master),
            // DJNZ e — 13/8 T
            0x10 => self.op_djnz(opcode, cycle, bus, master),
            // CALL nn — 17 T
            0xCD => self.op_call_nn(opcode, cycle, bus, master),
            // RET — 10 T
            0xC9 => self.op_ret(opcode, cycle, bus, master),
            // IN A,(n) — 11 T
            0xDB => self.op_in_a_n(opcode, cycle, bus, master),
            // OUT (n),A — 11 T
            0xD3 => self.op_out_n_a(opcode, cycle, bus, master),

            // DI — 4 T
            0xF3 => self.op_di(),
            // EI — 4 T
            0xFB => self.op_ei(),

            // JP cc,nn — 10 T
            op if (op & 0xC7) == 0xC2 => self.op_jp_cc_nn(op, cycle, bus, master),
            // JR cc,e — 12/7 T (NZ/Z/NC/C only)
            op if (op & 0xE7) == 0x20 => self.op_jr_cc_e(op, cycle, bus, master),
            // CALL cc,nn — 17/10 T
            op if (op & 0xC7) == 0xC4 => self.op_call_cc_nn(op, cycle, bus, master),
            // RET cc — 11/5 T
            op if (op & 0xC7) == 0xC0 => self.op_ret_cc(op, cycle, bus, master),
            // RST p — 11 T
            op if (op & 0xC7) == 0xC7 => self.op_rst(op, cycle, bus, master),

            _ => self.state = ExecState::Fetch,
        }
    }

    /// ED prefix dispatch. Handler cycle 0 = 4th T of ED opcode M1.
    /// Total T = 7 (base) + handler cycles.
    /// 8T: 1 cycle. 9T: 2. 12T: 5. 14T: 7. 15T: 8. 16T: 9. 18T: 11. 20T: 13.
    fn execute_instruction_ed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match opcode {
            // --- Specific ED opcodes (low 3 bits = 111) ---
            0x47 => self.op_ld_i_a(opcode, cycle), // LD I,A — 9T
            0x4F => self.op_ld_r_a(opcode, cycle), // LD R,A — 9T
            0x57 => self.op_ld_a_i(opcode, cycle), // LD A,I — 9T
            0x5F => self.op_ld_a_r(opcode, cycle), // LD A,R — 9T
            0x67 => self.op_rrd(opcode, cycle, bus, master), // RRD — 18T
            0x6F => self.op_rld(opcode, cycle, bus, master), // RLD — 18T

            // --- Block transfer/compare ---
            0xA0 | 0xA8 => self.op_ldi_ldd(opcode, cycle, bus, master), // LDI/LDD — 16T
            0xA1 | 0xA9 => self.op_cpi_cpd(opcode, cycle, bus, master), // CPI/CPD — 16T
            0xA2 | 0xAA => self.op_ini_ind(opcode, cycle, bus, master), // INI/IND — 16T
            0xA3 | 0xAB => self.op_outi_outd(opcode, cycle, bus, master), // OUTI/OUTD — 16T
            0xB0 | 0xB8 => self.op_ldir_lddr(opcode, cycle, bus, master), // LDIR/LDDR — 21/16T
            0xB1 | 0xB9 => self.op_cpir_cpdr(opcode, cycle, bus, master), // CPIR/CPDR — 21/16T
            0xB2 | 0xBA => self.op_inir_indr(opcode, cycle, bus, master), // INIR/INDR — 21/16T
            0xB3 | 0xBB => self.op_otir_otdr(opcode, cycle, bus, master), // OTIR/OTDR — 21/16T

            // --- Pattern-based (40-7F range, low 3 bits 0-6) ---
            op if (op & 0xC7) == 0x40 => self.op_in_r_c(op, cycle, bus, master), // IN r,(C) — 12T
            op if (op & 0xC7) == 0x41 => self.op_out_c_r(op, cycle, bus, master), // OUT (C),r — 12T
            op if (op & 0xCF) == 0x42 => self.op_sbc_hl_rr(op, cycle),           // SBC HL,rr — 15T
            op if (op & 0xCF) == 0x43 => self.op_ld_nn_rr_ed(op, cycle, bus, master), // LD (nn),rr — 20T
            op if (op & 0xC7) == 0x44 => self.op_neg(),                               // NEG — 8T
            op if (op & 0xC7) == 0x45 => self.op_retn(op, cycle, bus, master), // RETN/RETI — 14T
            op if (op & 0xC7) == 0x46 => self.op_im(op),                       // IM 0/1/2 — 8T
            op if (op & 0xCF) == 0x4A => self.op_adc_hl_rr(op, cycle),         // ADC HL,rr — 15T
            op if (op & 0xCF) == 0x4B => self.op_ld_rr_nn_ed(op, cycle, bus, master), // LD rr,(nn) — 20T

            // ED NOP — 8T: undefined opcodes act as NOP
            _ => self.state = ExecState::Fetch,
        }
    }
}

impl BusMasterComponent for Z80 {
    type Address = u16;
    type Data = u8;

    fn tick_with_bus<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        self.execute_cycle(bus, master);
        // Instruction boundary: at Fetch AND not mid-prefix (DD/FD set prefix_pending)
        matches!(self.state, ExecState::Fetch) && !self.prefix_pending
    }
}

impl Cpu for Z80 {
    fn reset<B: Bus<Address = Self::Address, Data = Self::Data> + ?Sized>(
        &mut self,
        _bus: &mut B,
        _master: BusMaster,
    ) {
        self.pc = 0x0000;
        self.a = 0xFF;
        self.f = 0xFF;
        self.sp = 0xFFFF;
        self.i = 0;
        self.r = 0;
        self.im = 0;
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {}

    fn is_sleeping(&self) -> bool {
        self.halted
    }
}

impl CpuStateTrait for Z80 {
    type Snapshot = Z80State;

    fn snapshot(&self) -> Z80State {
        Z80State {
            a: self.a,
            f: self.f,
            b: self.b,
            c: self.c,
            d: self.d,
            e: self.e,
            h: self.h,
            l: self.l,
            a_prime: self.a_prime,
            f_prime: self.f_prime,
            b_prime: self.b_prime,
            c_prime: self.c_prime,
            d_prime: self.d_prime,
            e_prime: self.e_prime,
            h_prime: self.h_prime,
            l_prime: self.l_prime,
            ix: self.ix,
            iy: self.iy,
            sp: self.sp,
            pc: self.pc,
            i: self.i,
            r: self.r,
            iff1: self.iff1,
            iff2: self.iff2,
            im: self.im,
            memptr: self.memptr,
            p: self.p,
            q: self.q,
        }
    }
}

impl Z80 {
    /// Returns true when the CPU is at a saveable instruction boundary.
    pub fn is_at_save_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch) && !self.prefix_pending
    }

    /// Returns true when the CPU is ready to fetch the next opcode
    /// (not mid-prefix). Used by the debugger for instruction-level stepping.
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch) && !self.prefix_pending
    }
}

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};
use crate::cpu::disasm::DisassembledInstruction;

impl Debuggable for Z80 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for Z80 {
    fn debug_pc(&self) -> u32 {
        u32::from(self.pc)
    }

    fn debug_at_instruction_boundary(&self) -> bool {
        self.at_instruction_boundary()
    }

    fn debug_disassemble(&self, addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        <Self as crate::cpu::Disassemble>::disassemble(addr, bytes)
    }
}
