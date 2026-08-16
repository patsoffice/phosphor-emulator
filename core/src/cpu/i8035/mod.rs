mod alu;
mod branch;
pub mod disasm;
mod load_store;

use crate::core::{Bus, BusMaster, bus::InterruptState, component::BusMasterComponent};
use crate::cpu::{
    Cpu,
    state::{CpuStateTrait, I8035State},
};

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum PswFlag {
    CY = 0x80, // Carry
    AC = 0x40, // Auxiliary carry
    F0 = 0x20, // User flag 0
    BS = 0x10, // Register bank select
}

impl From<PswFlag> for u8 {
    fn from(f: PswFlag) -> u8 {
        f as u8
    }
}

pub struct I8035 {
    // Registers
    pub a: u8,
    pub pc: u16,
    pub psw: u8,
    pub f1: bool,
    pub t: u8,
    pub dbbb: u8,
    pub p1: u8,
    pub p2: u8,

    // Internal RAM (sized for largest MCS-48 variant; 8035 uses 64 bytes)
    pub ram: [u8; 256],
    pub ram_mask: u8,

    // Memory bank flag
    pub a11: bool,
    pub a11_pending: bool,

    // Timer/counter state
    pub timer_enabled: bool,
    pub counter_enabled: bool,
    pub timer_overflow: bool,
    pub t1_prev: bool,
    pub prescaler: u8, // 5-bit prescaler (T increments every 32 machine cycles)

    // Interrupt state
    pub int_enabled: bool,
    pub tcnti_enabled: bool,
    pub in_interrupt: bool,
    pub irq_pending: bool,
    pub timer_irq_pending: bool,

    // Execution state
    pub(crate) state: ExecState,
    pub(crate) opcode: u8,
    pub(crate) temp_data: u8,
}

#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    Fetch,
    /// Second machine cycle of a 2-cycle instruction
    Execute(u8),
    /// Hardware interrupt entry sequence
    Interrupt(u8),
    /// Halted / idle (not used by standard MCS-48 but reserved)
    #[allow(dead_code)]
    Stopped,
}

impl Default for I8035 {
    fn default() -> Self {
        Self::new()
    }
}

impl I8035 {
    pub fn new() -> Self {
        Self {
            a: 0,
            pc: 0,
            psw: 0,
            f1: false,
            t: 0,
            dbbb: 0xFF,
            p1: 0xFF,
            p2: 0xFF,
            ram: [0; 256],
            ram_mask: 0x3F, // 64 bytes for 8035
            a11: false,
            a11_pending: false,
            timer_enabled: false,
            counter_enabled: false,
            timer_overflow: false,
            t1_prev: false,
            prescaler: 0,
            int_enabled: false,
            tcnti_enabled: false,
            in_interrupt: false,
            irq_pending: false,
            timer_irq_pending: false,
            state: ExecState::Fetch,
            opcode: 0,
            temp_data: 0,
        }
    }

    // --- Flag helpers ---

    #[inline]
    pub(crate) fn set_flag(&mut self, flag: PswFlag, set: bool) {
        crate::cpu::flags::set_flag(&mut self.psw, flag, set);
    }

    #[inline]
    pub(crate) fn flag_set(&self, flag: PswFlag) -> bool {
        crate::cpu::flags::flag_is_set(self.psw, flag)
    }

    // --- Register access ---

    /// Returns the RAM base address for the active register bank.
    /// Bank 0: 0x00-0x07, Bank 1: 0x18-0x1F.
    #[inline]
    fn reg_bank_offset(&self) -> u8 {
        if self.psw & PswFlag::BS as u8 != 0 {
            0x18
        } else {
            0x00
        }
    }

    #[inline]
    pub(crate) fn get_reg(&self, n: u8) -> u8 {
        let addr = self.reg_bank_offset() + (n & 0x07);
        self.ram[(addr & self.ram_mask) as usize]
    }

    #[inline]
    pub(crate) fn set_reg(&mut self, n: u8, val: u8) {
        let addr = self.reg_bank_offset() + (n & 0x07);
        self.ram[(addr & self.ram_mask) as usize] = val;
    }

    #[inline]
    pub(crate) fn read_ram(&self, addr: u8) -> u8 {
        self.ram[(addr & self.ram_mask) as usize]
    }

    #[inline]
    pub(crate) fn write_ram(&mut self, addr: u8, val: u8) {
        self.ram[(addr & self.ram_mask) as usize] = val;
    }

    // --- Stack ---

    /// Push PC and PSW upper nibble onto the internal stack.
    /// Stack entry format: byte0 = PC[7:0], byte1 = PSW[7:4] | PC[11:8].
    pub(crate) fn push_pc_psw(&mut self) {
        let sp = self.psw & 0x07;
        let addr = 2 * sp + 8;
        self.write_ram(addr, self.pc as u8);
        self.write_ram(addr + 1, ((self.pc >> 8) as u8 & 0x0F) | (self.psw & 0xF0));
        let new_sp = (sp + 1) & 0x07;
        self.psw = (self.psw & 0xF8) | new_sp;
    }

    /// Pop PC (and optionally PSW flags) from the internal stack.
    /// RET uses restore_psw=false, RETR uses restore_psw=true.
    pub(crate) fn pop_pc_psw(&mut self, restore_psw: bool) {
        let sp = (self.psw & 0x07).wrapping_sub(1) & 0x07;
        self.psw = (self.psw & 0xF8) | sp;
        let addr = 2 * sp + 8;
        let lo = self.read_ram(addr);
        let hi = self.read_ram(addr + 1);
        self.pc = ((hi & 0x0F) as u16) << 8 | lo as u16;
        if restore_psw {
            self.psw = (self.psw & 0x0F) | (hi & 0xF0);
        }
    }

    // --- Timer/Counter ---

    /// Bus I/O address for T1 test pin (counter input).
    const PORT_T1: u16 = 0x111;

    /// Increment the T register; set overflow flag and IRQ pending on wrap.
    fn increment_t(&mut self) {
        let (new_t, overflow) = self.t.overflowing_add(1);
        self.t = new_t;
        if overflow {
            self.timer_overflow = true;
            if self.tcnti_enabled {
                self.timer_irq_pending = true;
            }
        }
    }

    /// Advance timer and/or counter (called every machine cycle).
    /// Timer mode: 5-bit prescaler divides by 32, then increments T.
    /// Counter mode: increments T on T1 falling edge.
    fn tick_timer_counter<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        if self.timer_enabled {
            self.prescaler += 1;
            if self.prescaler >= 32 {
                self.prescaler = 0;
                self.increment_t();
            }
        }
        if self.counter_enabled {
            let t1 = bus.io_read(master, Self::PORT_T1) != 0;
            if self.t1_prev && !t1 {
                self.increment_t();
            }
            self.t1_prev = t1;
        }
    }

    // --- State machine ---

    /// Execute one machine cycle.
    pub fn execute_cycle<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        match self.state {
            ExecState::Fetch => {
                // Check interrupts at instruction boundary
                let ints = bus.check_interrupts(master);
                if self.handle_interrupts(ints) {
                    self.tick_timer_counter(bus, master);
                    return;
                }

                // Fetch opcode from program memory
                self.opcode = bus.read(master, self.pc);
                self.pc = (self.pc + 1) & 0x0FFF;

                // Execute cycle 0 (1-cycle ops complete here)
                self.execute_instruction(self.opcode, 0, bus, master);
                self.tick_timer_counter(bus, master);
            }
            ExecState::Execute(op) => {
                // Execute cycle 1 of a 2-cycle instruction
                self.execute_instruction(op, 1, bus, master);
                self.tick_timer_counter(bus, master);
            }
            ExecState::Interrupt(cycle) => {
                self.execute_interrupt(cycle, bus, master);
                self.tick_timer_counter(bus, master);
            }
            ExecState::Stopped => {}
        }
    }

    fn execute_instruction<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match opcode {
            // ===== NOP (0x00) =====
            0x00 => self.state = ExecState::Fetch,

            // ===== Accumulator unary (1-cycle) =====
            0x07 => {
                self.a = Self::perform_dec(self.a);
                self.state = ExecState::Fetch;
            }
            0x17 => {
                self.a = Self::perform_inc(self.a);
                self.state = ExecState::Fetch;
            }
            0x27 => {
                self.perform_clr_a();
                self.state = ExecState::Fetch;
            }
            0x37 => {
                self.perform_cpl_a();
                self.state = ExecState::Fetch;
            }
            0x47 => {
                self.perform_swap();
                self.state = ExecState::Fetch;
            }
            0x57 => {
                self.perform_da();
                self.state = ExecState::Fetch;
            }
            0x67 => {
                self.perform_rrc();
                self.state = ExecState::Fetch;
            }
            0x77 => {
                self.perform_rr();
                self.state = ExecState::Fetch;
            }
            0xE7 => {
                self.perform_rl();
                self.state = ExecState::Fetch;
            }
            0xF7 => {
                self.perform_rlc();
                self.state = ExecState::Fetch;
            }

            // ===== Status flag ops (1-cycle) =====
            0x97 => {
                self.set_flag(PswFlag::CY, false);
                self.state = ExecState::Fetch;
            }
            0xA7 => {
                let cy = !self.flag_set(PswFlag::CY);
                self.set_flag(PswFlag::CY, cy);
                self.state = ExecState::Fetch;
            }
            0x85 => {
                self.set_flag(PswFlag::F0, false);
                self.state = ExecState::Fetch;
            }
            0x95 => {
                let f0 = !self.flag_set(PswFlag::F0);
                self.set_flag(PswFlag::F0, f0);
                self.state = ExecState::Fetch;
            }
            0xA5 => {
                self.f1 = false;
                self.state = ExecState::Fetch;
            }
            0xB5 => {
                self.f1 = !self.f1;
                self.state = ExecState::Fetch;
            }

            // ===== Register INC/DEC (1-cycle) =====
            0x10 | 0x11 => {
                // INC @Ri
                let addr = self.get_reg(opcode & 0x01);
                self.write_ram(addr, Self::perform_inc(self.read_ram(addr)));
                self.state = ExecState::Fetch;
            }
            0x18..=0x1F => {
                // INC Rn
                let n = opcode & 0x07;
                self.set_reg(n, Self::perform_inc(self.get_reg(n)));
                self.state = ExecState::Fetch;
            }
            0xC8..=0xCF => {
                // DEC Rn
                let n = opcode & 0x07;
                self.set_reg(n, Self::perform_dec(self.get_reg(n)));
                self.state = ExecState::Fetch;
            }

            // ===== Register ALU (1-cycle) =====
            0x60 | 0x61 => {
                // ADD A,@Ri
                self.perform_add(self.read_ram(self.get_reg(opcode & 0x01)));
                self.state = ExecState::Fetch;
            }
            0x68..=0x6F => {
                // ADD A,Rn
                self.perform_add(self.get_reg(opcode & 0x07));
                self.state = ExecState::Fetch;
            }
            0x70 | 0x71 => {
                // ADDC A,@Ri
                self.perform_addc(self.read_ram(self.get_reg(opcode & 0x01)));
                self.state = ExecState::Fetch;
            }
            0x78..=0x7F => {
                // ADDC A,Rn
                self.perform_addc(self.get_reg(opcode & 0x07));
                self.state = ExecState::Fetch;
            }
            0x40 | 0x41 => {
                // ORL A,@Ri
                self.perform_orl(self.read_ram(self.get_reg(opcode & 0x01)));
                self.state = ExecState::Fetch;
            }
            0x48..=0x4F => {
                // ORL A,Rn
                self.perform_orl(self.get_reg(opcode & 0x07));
                self.state = ExecState::Fetch;
            }
            0x50 | 0x51 => {
                // ANL A,@Ri
                self.perform_anl(self.read_ram(self.get_reg(opcode & 0x01)));
                self.state = ExecState::Fetch;
            }
            0x58..=0x5F => {
                // ANL A,Rn
                self.perform_anl(self.get_reg(opcode & 0x07));
                self.state = ExecState::Fetch;
            }
            0xD0 | 0xD1 => {
                // XRL A,@Ri
                self.perform_xrl(self.read_ram(self.get_reg(opcode & 0x01)));
                self.state = ExecState::Fetch;
            }
            0xD8..=0xDF => {
                // XRL A,Rn
                self.perform_xrl(self.get_reg(opcode & 0x07));
                self.state = ExecState::Fetch;
            }

            // ===== Immediate ALU (2-cycle) =====
            0x03 => {
                // ADD A,#data
                match cycle {
                    0 => self.state = ExecState::Execute(self.opcode),
                    _ => {
                        let data = bus.read(master, self.pc);
                        self.pc = (self.pc + 1) & 0x0FFF;
                        self.perform_add(data);
                        self.state = ExecState::Fetch;
                    }
                }
            }
            0x13 => {
                // ADDC A,#data
                match cycle {
                    0 => self.state = ExecState::Execute(self.opcode),
                    _ => {
                        let data = bus.read(master, self.pc);
                        self.pc = (self.pc + 1) & 0x0FFF;
                        self.perform_addc(data);
                        self.state = ExecState::Fetch;
                    }
                }
            }
            0x43 => {
                // ORL A,#data
                match cycle {
                    0 => self.state = ExecState::Execute(self.opcode),
                    _ => {
                        let data = bus.read(master, self.pc);
                        self.pc = (self.pc + 1) & 0x0FFF;
                        self.perform_orl(data);
                        self.state = ExecState::Fetch;
                    }
                }
            }
            0x53 => {
                // ANL A,#data
                match cycle {
                    0 => self.state = ExecState::Execute(self.opcode),
                    _ => {
                        let data = bus.read(master, self.pc);
                        self.pc = (self.pc + 1) & 0x0FFF;
                        self.perform_anl(data);
                        self.state = ExecState::Fetch;
                    }
                }
            }
            0xD3 => {
                // XRL A,#data
                match cycle {
                    0 => self.state = ExecState::Execute(self.opcode),
                    _ => {
                        let data = bus.read(master, self.pc);
                        self.pc = (self.pc + 1) & 0x0FFF;
                        self.perform_xrl(data);
                        self.state = ExecState::Fetch;
                    }
                }
            }

            // ===== Data movement - register (1-cycle) =====
            0xF0 | 0xF1 => self.op_mov_a_indirect(opcode & 0x01),
            0xF8..=0xFF => self.op_mov_a_rn(opcode & 0x07),
            0xA0 | 0xA1 => self.op_mov_indirect_a(opcode & 0x01),
            0xA8..=0xAF => self.op_mov_rn_a(opcode & 0x07),
            0x20 | 0x21 => self.op_xch_a_indirect(opcode & 0x01),
            0x28..=0x2F => self.op_xch_a_rn(opcode & 0x07),
            0x30 | 0x31 => self.op_xchd_a_indirect(opcode & 0x01),
            0x42 => self.op_mov_a_t(),
            0x62 => self.op_mov_t_a(),
            0xC7 => self.op_mov_a_psw(),
            0xD7 => self.op_mov_psw_a(),

            // ===== Data movement - immediate (2-cycle) =====
            0x23 => self.op_mov_a_imm(cycle, bus, master),
            0xB0 | 0xB1 => self.op_mov_indirect_imm(opcode & 0x01, cycle, bus, master),
            0xB8..=0xBF => self.op_mov_rn_imm(opcode & 0x07, cycle, bus, master),

            // ===== External memory / program memory (2-cycle) =====
            0x80 | 0x81 => self.op_movx_a_indirect(opcode & 0x01, cycle, bus, master),
            0x90 | 0x91 => self.op_movx_indirect_a(opcode & 0x01, cycle, bus, master),
            0xA3 => self.op_movp_a(cycle, bus, master),
            0xE3 => self.op_movp3_a(cycle, bus, master),

            // ===== Port I/O (2-cycle) =====
            0x02 => self.op_outl_bus_a(cycle, bus, master),
            0x08 => self.op_ins_a_bus(cycle, bus, master),
            0x09 => self.op_in_a_p1(cycle, bus, master),
            0x0A => self.op_in_a_p2(cycle, bus, master),
            0x39 => self.op_outl_p1_a(cycle, bus, master),
            0x3A => self.op_outl_p2_a(cycle, bus, master),

            // ===== Port read-modify-write (2-cycle) =====
            0x88 => self.op_orl_bus_imm(cycle, bus, master),
            0x89 => self.op_orl_p1_imm(cycle, bus, master),
            0x8A => self.op_orl_p2_imm(cycle, bus, master),
            0x98 => self.op_anl_bus_imm(cycle, bus, master),
            0x99 => self.op_anl_p1_imm(cycle, bus, master),
            0x9A => self.op_anl_p2_imm(cycle, bus, master),

            // ===== 4-bit expander port I/O (2-cycle) =====
            0x0C..=0x0F => self.op_movd_a_pp(opcode & 0x03, cycle, bus, master),
            0x3C..=0x3F => self.op_movd_pp_a(opcode & 0x03, cycle, bus, master),
            0x8C..=0x8F => self.op_orld_pp_a(opcode & 0x03, cycle, bus, master),
            0x9C..=0x9F => self.op_anld_pp_a(opcode & 0x03, cycle, bus, master),

            // ===== Unconditional jumps / calls (2-cycle) =====
            0x04 | 0x24 | 0x44 | 0x64 | 0x84 | 0xA4 | 0xC4 | 0xE4 => {
                self.op_jmp(cycle, bus, master);
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xB4 | 0xD4 | 0xF4 => {
                self.op_call(cycle, bus, master);
            }
            0xB3 => self.op_jmpp(cycle, bus, master),

            // ===== Returns (2-cycle) =====
            0x83 => self.op_ret(cycle),
            0x93 => self.op_retr(cycle),

            // ===== DJNZ (2-cycle) =====
            0xE8..=0xEF => self.op_djnz(opcode & 0x07, cycle, bus, master),

            // ===== Conditional jumps - flags (2-cycle) =====
            0xF6 => self.op_jc(cycle, bus, master),
            0xE6 => self.op_jnc(cycle, bus, master),
            0xC6 => self.op_jz(cycle, bus, master),
            0x96 => self.op_jnz(cycle, bus, master),
            0xB6 => self.op_jf0(cycle, bus, master),
            0x76 => self.op_jf1(cycle, bus, master),

            // ===== Conditional jumps - pins/interrupts (2-cycle) =====
            0x36 => self.op_jt0(cycle, bus, master),
            0x26 => self.op_jnt0(cycle, bus, master),
            0x56 => self.op_jt1(cycle, bus, master),
            0x46 => self.op_jnt1(cycle, bus, master),
            0x16 => self.op_jtf(cycle, bus, master),
            0x86 => self.op_jni(cycle, bus, master),

            // ===== Bit test jumps (2-cycle) =====
            0x12 | 0x32 | 0x52 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                self.op_jbb(opcode >> 5, cycle, bus, master);
            }

            // ===== Control - interrupt enable/disable (1-cycle) =====
            0x05 => {
                self.int_enabled = true;
                self.state = ExecState::Fetch;
            }
            0x15 => {
                self.int_enabled = false;
                self.state = ExecState::Fetch;
            }
            0x25 => {
                self.tcnti_enabled = true;
                self.state = ExecState::Fetch;
            }
            0x35 => {
                self.tcnti_enabled = false;
                self.state = ExecState::Fetch;
            }

            // ===== Control - timer/counter (1-cycle) =====
            0x45 => {
                // STRT CNT
                self.counter_enabled = true;
                self.timer_enabled = false;
                self.state = ExecState::Fetch;
            }
            0x55 => {
                // STRT T
                self.timer_enabled = true;
                self.counter_enabled = false;
                self.prescaler = 0;
                self.state = ExecState::Fetch;
            }
            0x65 => {
                // STOP TCNT
                self.timer_enabled = false;
                self.counter_enabled = false;
                self.state = ExecState::Fetch;
            }

            // ===== Control - bank select (1-cycle) =====
            0xC5 => {
                self.set_flag(PswFlag::BS, false);
                self.state = ExecState::Fetch;
            }
            0xD5 => {
                self.set_flag(PswFlag::BS, true);
                self.state = ExecState::Fetch;
            }
            0xE5 => {
                self.a11_pending = false;
                self.state = ExecState::Fetch;
            }
            0xF5 => {
                self.a11_pending = true;
                self.state = ExecState::Fetch;
            }

            // ===== Undefined opcodes - treat as NOP =====
            _ => self.state = ExecState::Fetch,
        }
    }

    /// Check for pending interrupts at instruction boundary.
    /// Priority: external INT > timer/counter overflow.
    fn handle_interrupts(&mut self, ints: InterruptState) -> bool {
        // External interrupt (level-triggered, masked by int_enabled and in_interrupt)
        if self.int_enabled && !self.in_interrupt && ints.irq {
            self.irq_pending = true;
            self.state = ExecState::Interrupt(0);
            return true;
        }

        // Timer/counter overflow interrupt
        if self.tcnti_enabled && self.timer_irq_pending && !self.in_interrupt {
            self.state = ExecState::Interrupt(0);
            return true;
        }

        false
    }

    fn execute_interrupt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        _bus: &mut B,
        _master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Push PC and PSW to internal stack
                self.push_pc_psw();
                self.int_enabled = false;
                self.in_interrupt = true;
                self.state = ExecState::Interrupt(1);
            }
            1 => {
                // Jump to interrupt vector
                if self.irq_pending {
                    self.irq_pending = false;
                    self.pc = 0x003;
                } else if self.timer_irq_pending {
                    self.timer_irq_pending = false;
                    self.pc = 0x007;
                }
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }
}

impl BusMasterComponent for I8035 {
    type Address = u16;
    type Data = u8;

    fn tick_with_bus<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        self.execute_cycle(bus, master);
        matches!(self.state, ExecState::Fetch)
    }
}

impl Cpu for I8035 {
    fn reset<B: Bus<Address = Self::Address, Data = Self::Data> + ?Sized>(
        &mut self,
        _bus: &mut B,
        _master: BusMaster,
    ) {
        self.pc = 0;
        self.psw = 0;
        self.a11 = false;
        self.a11_pending = false;
        self.timer_enabled = false;
        self.counter_enabled = false;
        self.timer_overflow = false;
        self.prescaler = 0;
        self.int_enabled = false;
        self.tcnti_enabled = false;
        self.in_interrupt = false;
        self.irq_pending = false;
        self.timer_irq_pending = false;
        self.state = ExecState::Fetch;
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {
        // Interrupts are sampled at instruction boundary via check_interrupts
    }

    fn is_sleeping(&self) -> bool {
        matches!(self.state, ExecState::Stopped)
    }
}

impl CpuStateTrait for I8035 {
    type Snapshot = I8035State;

    fn snapshot(&self) -> I8035State {
        I8035State {
            a: self.a,
            pc: self.pc,
            psw: self.psw,
            f1: self.f1,
            t: self.t,
            dbbb: self.dbbb,
            p1: self.p1,
            p2: self.p2,
        }
    }
}

// -- Save state support ------------------------------------------------------

use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};

const STATE_TAG_FETCH: u8 = 0;
const STATE_TAG_STOPPED: u8 = 1;

impl I8035 {
    /// Returns true when the CPU is at a saveable instruction boundary.
    pub fn is_at_save_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch | ExecState::Stopped)
    }

    /// Returns true when the CPU is ready to fetch the next opcode.
    /// Used by the debugger for instruction-level stepping.
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }
}

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};
use crate::cpu::disasm::DisassembledInstruction;

impl Debuggable for I8035 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for I8035 {
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

impl Saveable for I8035 {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        // Registers
        w.write_u8(self.a);
        w.write_u16_le(self.pc);
        w.write_u8(self.psw);
        w.write_bool(self.f1);
        w.write_u8(self.t);
        w.write_u8(self.dbbb);
        w.write_u8(self.p1);
        w.write_u8(self.p2);
        // Internal RAM (length-prefixed)
        w.write_bytes(&self.ram[..(self.ram_mask as usize + 1)]);
        w.write_u8(self.ram_mask);
        // Memory bank
        w.write_bool(self.a11);
        w.write_bool(self.a11_pending);
        // Timer/counter
        w.write_bool(self.timer_enabled);
        w.write_bool(self.counter_enabled);
        w.write_bool(self.timer_overflow);
        w.write_bool(self.t1_prev);
        w.write_u8(self.prescaler);
        // Interrupts
        w.write_bool(self.int_enabled);
        w.write_bool(self.tcnti_enabled);
        w.write_bool(self.in_interrupt);
        w.write_bool(self.irq_pending);
        w.write_bool(self.timer_irq_pending);
        // ExecState tag
        let tag = match self.state {
            ExecState::Stopped => STATE_TAG_STOPPED,
            _ => STATE_TAG_FETCH,
        };
        w.write_u8(tag);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.a = r.read_u8()?;
        self.pc = r.read_u16_le()?;
        self.psw = r.read_u8()?;
        self.f1 = r.read_bool()?;
        self.t = r.read_u8()?;
        self.dbbb = r.read_u8()?;
        self.p1 = r.read_u8()?;
        self.p2 = r.read_u8()?;
        // Internal RAM
        let ram_data = r.read_bytes()?;
        let len = ram_data.len().min(self.ram.len());
        self.ram[..len].copy_from_slice(&ram_data[..len]);
        self.ram_mask = r.read_u8()?;
        // Memory bank
        self.a11 = r.read_bool()?;
        self.a11_pending = r.read_bool()?;
        // Timer/counter
        self.timer_enabled = r.read_bool()?;
        self.counter_enabled = r.read_bool()?;
        self.timer_overflow = r.read_bool()?;
        self.t1_prev = r.read_bool()?;
        self.prescaler = r.read_u8()?;
        // Interrupts
        self.int_enabled = r.read_bool()?;
        self.tcnti_enabled = r.read_bool()?;
        self.in_interrupt = r.read_bool()?;
        self.irq_pending = r.read_bool()?;
        self.timer_irq_pending = r.read_bool()?;
        // ExecState
        let tag = r.read_u8()?;
        self.state = match tag {
            STATE_TAG_STOPPED => ExecState::Stopped,
            _ => ExecState::Fetch,
        };
        self.opcode = 0;
        self.temp_data = 0;
        Ok(())
    }
}
