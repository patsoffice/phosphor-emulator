mod alu;
mod branch;
pub mod disasm;
mod load_store;
mod stack;

use crate::core::{Bus, BusMaster, bus::InterruptState, component::BusMasterComponent};
use crate::cpu::{
    Cpu,
    state::{CpuStateTrait, M6800State},
};
use crate::prelude::Saveable;

pub use super::m68xx::CcFlag;
use super::m68xx::{Acc, M68xxAlu};

/// Bits 6-7 of the CC register are unused on the M6800 and always read as 1.
const CC_UNUSED_BITS: u8 = 0xC0;

/// Interrupt type being processed by the M6800 interrupt state machine.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum InterruptType {
    None = 0,
    Nmi = 1,
    Irq = 2,
    Swi = 3,
}

/// Fields are ordered to match the save-state serialization layout (version 1).
#[derive(Saveable)]
#[save_version(1)]
pub struct M6800 {
    // Registers
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub sp: u16,
    pub pc: u16,
    pub cc: u8,

    /// Previous NMI line state for edge detection
    pub(crate) nmi_previous: bool,
    /// True when the bus HALT line is asserted (TSC logic)
    pub(crate) halted: bool,

    // Internal state (not serialized)
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    #[save_skip(default)]
    pub(crate) opcode: u8,
    #[save_skip(default)]
    pub(crate) temp_addr: u16,
    /// Temporary data storage for multi-cycle operations (RMW operand, 16-bit hi byte)
    #[save_skip(default)]
    pub(crate) temp_data: u8,
    /// Interrupt type being processed
    #[save_skip(default = InterruptType::None)]
    pub(crate) interrupt_type: InterruptType,
}

#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    Fetch,
    Execute(u8, u8), // (opcode, cycle)
    /// Hardware interrupt response sequence (NMI/IRQ push + vector)
    Interrupt(u8),
    /// WAI: all registers pushed, waiting for interrupt
    WaitForInterrupt,
}

impl Default for M6800 {
    fn default() -> Self {
        Self::new()
    }
}

impl M68xxAlu for M6800 {
    #[inline]
    fn reg(&mut self, acc: Acc) -> &mut u8 {
        match acc {
            Acc::A => &mut self.a,
            Acc::B => &mut self.b,
        }
    }
    #[inline]
    fn reg_cc(&mut self) -> &mut u8 {
        &mut self.cc
    }

    /// M6800 TST also clears C flag (unlike M6809).
    #[inline]
    fn perform_tst(&mut self, val: u8) {
        self.set_flags_logical(val);
        self.set_flag(CcFlag::C, false);
    }
}

impl M6800 {
    pub fn new() -> Self {
        Self {
            a: 0,
            b: 0,
            x: 0,
            sp: 0,
            pc: 0,
            cc: 0,
            nmi_previous: false,
            halted: false,
            state: ExecState::Fetch,
            opcode: 0,
            temp_addr: 0,
            temp_data: 0,
            interrupt_type: InterruptType::None,
        }
    }

    /// Execute one cycle - handles fetch/execute state machine
    pub fn execute_cycle<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        // Check TSC via the generic bus
        if bus.is_halted_for(master) {
            self.halted = true;
            return;
        }

        // TSC released — one dead cycle for re-sync
        if self.halted {
            self.halted = false;
            return;
        }

        match self.state {
            ExecState::Fetch => {
                let ints = bus.check_interrupts(master);
                if self.handle_interrupts(ints) {
                    return;
                }

                self.opcode = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(self.opcode, 0);
            }
            ExecState::Execute(op, cyc) => {
                self.execute_instruction(op, cyc, bus, master);
            }
            ExecState::Interrupt(cycle) => {
                self.execute_interrupt(cycle, bus, master);
            }
            ExecState::WaitForInterrupt => {
                self.wait_for_interrupt(bus, master);
            }
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
            // NOP (0x01) - 2 cycles total: 1 fetch + 1 internal
            0x01 => {
                if cycle == 0 {
                    self.state = ExecState::Fetch;
                }
            }

            // --- Branches (4 cycles) ---
            0x20 => self.op_bra(cycle, bus, master),
            0x22 => self.op_bhi(cycle, bus, master),
            0x23 => self.op_bls(cycle, bus, master),
            0x24 => self.op_bcc(cycle, bus, master),
            0x25 => self.op_bcs(cycle, bus, master),
            0x26 => self.op_bne(cycle, bus, master),
            0x27 => self.op_beq(cycle, bus, master),
            0x28 => self.op_bvc(cycle, bus, master),
            0x29 => self.op_bvs(cycle, bus, master),
            0x2A => self.op_bpl(cycle, bus, master),
            0x2B => self.op_bmi(cycle, bus, master),
            0x2C => self.op_bge(cycle, bus, master),
            0x2D => self.op_blt(cycle, bus, master),
            0x2E => self.op_bgt(cycle, bus, master),
            0x2F => self.op_ble(cycle, bus, master),

            // --- Stack/Interrupt ops ---
            0x39 => self.op_rts(cycle, bus, master),
            0x3B => self.op_rti(cycle, bus, master),
            0x3E => self.op_wai(cycle, bus, master),
            0x3F => self.op_swi(cycle),

            // --- Transfer / Flag / Misc inherent ops (2 cycles) ---
            0x06 => self.op_tap(cycle),
            0x07 => self.op_tpa(cycle),
            0x0A => self.op_clv(cycle),
            0x0B => self.op_sev(cycle),
            0x0C => self.op_clc(cycle),
            0x0D => self.op_sec(cycle),
            0x0E => self.op_cli(cycle),
            0x0F => self.op_sei(cycle),
            0x10 => self.op_sba(cycle),
            0x11 => self.op_cba(cycle),
            0x16 => self.op_tab(cycle),
            0x17 => self.op_tba(cycle),
            0x19 => self.op_daa(cycle),
            0x1B => self.op_aba(cycle),

            // --- 16-bit register ops (4 cycles) ---
            0x08 => self.op_inx(cycle),
            0x09 => self.op_dex(cycle),
            0x30 => self.op_tsx(cycle),
            0x31 => self.op_ins(cycle),
            0x32 => self.op_pula(cycle, bus, master),
            0x33 => self.op_pulb(cycle, bus, master),
            0x34 => self.op_des(cycle),
            0x35 => self.op_txs(cycle),
            0x36 => self.op_psha(cycle, bus, master),
            0x37 => self.op_pshb(cycle, bus, master),

            // --- Inherent unary/shift A register (2 cycles) ---
            0x40 => self.op_nega(cycle),
            0x43 => self.op_coma(cycle),
            0x44 => self.op_lsra(cycle),
            0x46 => self.op_rora(cycle),
            0x47 => self.op_asra(cycle),
            0x48 => self.op_asla(cycle),
            0x49 => self.op_rola(cycle),
            0x4A => self.op_deca(cycle),
            0x4C => self.op_inca(cycle),
            0x4D => self.op_tsta(cycle),
            0x4F => self.op_clra(cycle),

            // --- Inherent unary/shift B register (2 cycles) ---
            0x50 => self.op_negb(cycle),
            0x53 => self.op_comb(cycle),
            0x54 => self.op_lsrb(cycle),
            0x56 => self.op_rorb(cycle),
            0x57 => self.op_asrb(cycle),
            0x58 => self.op_aslb(cycle),
            0x59 => self.op_rolb(cycle),
            0x5A => self.op_decb(cycle),
            0x5C => self.op_incb(cycle),
            0x5D => self.op_tstb(cycle),
            0x5F => self.op_clrb(cycle),

            // --- 0x6x: Memory unary/shift indexed (7 cycles) ---
            0x60 => self.op_neg_idx(cycle, bus, master),
            0x63 => self.op_com_idx(cycle, bus, master),
            0x64 => self.op_lsr_idx(cycle, bus, master),
            0x66 => self.op_ror_idx(cycle, bus, master),
            0x67 => self.op_asr_idx(cycle, bus, master),
            0x68 => self.op_asl_idx(cycle, bus, master),
            0x69 => self.op_rol_idx(cycle, bus, master),
            0x6A => self.op_dec_idx(cycle, bus, master),
            0x6C => self.op_inc_idx(cycle, bus, master),
            0x6D => self.op_tst_idx(cycle, bus, master),
            0x6E => self.op_jmp_idx(cycle, bus, master),
            0x6F => self.op_clr_idx(cycle, bus, master),

            // --- 0x7x: Memory unary/shift extended (6 cycles) ---
            0x70 => self.op_neg_ext(cycle, bus, master),
            0x73 => self.op_com_ext(cycle, bus, master),
            0x74 => self.op_lsr_ext(cycle, bus, master),
            0x76 => self.op_ror_ext(cycle, bus, master),
            0x77 => self.op_asr_ext(cycle, bus, master),
            0x78 => self.op_asl_ext(cycle, bus, master),
            0x79 => self.op_rol_ext(cycle, bus, master),
            0x7A => self.op_dec_ext(cycle, bus, master),
            0x7C => self.op_inc_ext(cycle, bus, master),
            0x7D => self.op_tst_ext(cycle, bus, master),
            0x7E => self.op_jmp_ext(cycle, bus, master),
            0x7F => self.op_clr_ext(cycle, bus, master),

            // --- 0x8x: A register immediate + 16-bit immediate ---
            0x80 => self.op_suba_imm(cycle, bus, master),
            0x81 => self.op_cmpa_imm(cycle, bus, master),
            0x82 => self.op_sbca_imm(cycle, bus, master),
            0x84 => self.op_anda_imm(cycle, bus, master),
            0x85 => self.op_bita_imm(cycle, bus, master),
            0x86 => self.op_ldaa_imm(cycle, bus, master),
            0x88 => self.op_eora_imm(cycle, bus, master),
            0x89 => self.op_adca_imm(cycle, bus, master),
            0x8A => self.op_oraa_imm(cycle, bus, master),
            0x8B => self.op_adda_imm(cycle, bus, master),
            0x8C => self.op_cpx_imm(cycle, bus, master),
            0x8D => self.op_bsr(cycle, bus, master),
            0x8E => self.op_lds_imm(cycle, bus, master),

            // --- 0x9x: A register direct + 16-bit direct ---
            0x90 => self.op_suba_dir(cycle, bus, master),
            0x91 => self.op_cmpa_dir(cycle, bus, master),
            0x92 => self.op_sbca_dir(cycle, bus, master),
            0x94 => self.op_anda_dir(cycle, bus, master),
            0x95 => self.op_bita_dir(cycle, bus, master),
            0x96 => self.op_ldaa_dir(cycle, bus, master),
            0x97 => self.op_staa_dir(cycle, bus, master),
            0x98 => self.op_eora_dir(cycle, bus, master),
            0x99 => self.op_adca_dir(cycle, bus, master),
            0x9A => self.op_oraa_dir(cycle, bus, master),
            0x9B => self.op_adda_dir(cycle, bus, master),
            0x9C => self.op_cpx_dir(cycle, bus, master),
            0x9E => self.op_lds_dir(cycle, bus, master),
            0x9F => self.op_sts_dir(cycle, bus, master),

            // --- 0xAx: A register indexed + 16-bit indexed ---
            0xA0 => self.op_suba_idx(cycle, bus, master),
            0xA1 => self.op_cmpa_idx(cycle, bus, master),
            0xA2 => self.op_sbca_idx(cycle, bus, master),
            0xA4 => self.op_anda_idx(cycle, bus, master),
            0xA5 => self.op_bita_idx(cycle, bus, master),
            0xA6 => self.op_ldaa_idx(cycle, bus, master),
            0xA7 => self.op_staa_idx(cycle, bus, master),
            0xA8 => self.op_eora_idx(cycle, bus, master),
            0xA9 => self.op_adca_idx(cycle, bus, master),
            0xAA => self.op_oraa_idx(cycle, bus, master),
            0xAB => self.op_adda_idx(cycle, bus, master),
            0xAC => self.op_cpx_idx(cycle, bus, master),
            0xAD => self.op_jsr_idx(cycle, bus, master),
            0xAE => self.op_lds_idx(cycle, bus, master),
            0xAF => self.op_sts_idx(cycle, bus, master),

            // --- 0xBx: A register extended + 16-bit extended ---
            0xB0 => self.op_suba_ext(cycle, bus, master),
            0xB1 => self.op_cmpa_ext(cycle, bus, master),
            0xB2 => self.op_sbca_ext(cycle, bus, master),
            0xB4 => self.op_anda_ext(cycle, bus, master),
            0xB5 => self.op_bita_ext(cycle, bus, master),
            0xB6 => self.op_ldaa_ext(cycle, bus, master),
            0xB7 => self.op_staa_ext(cycle, bus, master),
            0xB8 => self.op_eora_ext(cycle, bus, master),
            0xB9 => self.op_adca_ext(cycle, bus, master),
            0xBA => self.op_oraa_ext(cycle, bus, master),
            0xBB => self.op_adda_ext(cycle, bus, master),
            0xBC => self.op_cpx_ext(cycle, bus, master),
            0xBD => self.op_jsr_ext(cycle, bus, master),
            0xBE => self.op_lds_ext(cycle, bus, master),
            0xBF => self.op_sts_ext(cycle, bus, master),

            // --- 0xCx: B register immediate + 16-bit immediate ---
            0xC0 => self.op_subb_imm(cycle, bus, master),
            0xC1 => self.op_cmpb_imm(cycle, bus, master),
            0xC2 => self.op_sbcb_imm(cycle, bus, master),
            0xC4 => self.op_andb_imm(cycle, bus, master),
            0xC5 => self.op_bitb_imm(cycle, bus, master),
            0xC6 => self.op_ldab_imm(cycle, bus, master),
            0xC8 => self.op_eorb_imm(cycle, bus, master),
            0xC9 => self.op_adcb_imm(cycle, bus, master),
            0xCA => self.op_orab_imm(cycle, bus, master),
            0xCB => self.op_addb_imm(cycle, bus, master),
            0xCE => self.op_ldx_imm(cycle, bus, master),

            // --- 0xDx: B register direct + 16-bit direct ---
            0xD0 => self.op_subb_dir(cycle, bus, master),
            0xD1 => self.op_cmpb_dir(cycle, bus, master),
            0xD2 => self.op_sbcb_dir(cycle, bus, master),
            0xD4 => self.op_andb_dir(cycle, bus, master),
            0xD5 => self.op_bitb_dir(cycle, bus, master),
            0xD6 => self.op_ldab_dir(cycle, bus, master),
            0xD7 => self.op_stab_dir(cycle, bus, master),
            0xD8 => self.op_eorb_dir(cycle, bus, master),
            0xD9 => self.op_adcb_dir(cycle, bus, master),
            0xDA => self.op_orab_dir(cycle, bus, master),
            0xDB => self.op_addb_dir(cycle, bus, master),
            0xDE => self.op_ldx_dir(cycle, bus, master),
            0xDF => self.op_stx_dir(cycle, bus, master),

            // --- 0xEx: B register indexed + 16-bit indexed ---
            0xE0 => self.op_subb_idx(cycle, bus, master),
            0xE1 => self.op_cmpb_idx(cycle, bus, master),
            0xE2 => self.op_sbcb_idx(cycle, bus, master),
            0xE4 => self.op_andb_idx(cycle, bus, master),
            0xE5 => self.op_bitb_idx(cycle, bus, master),
            0xE6 => self.op_ldab_idx(cycle, bus, master),
            0xE7 => self.op_stab_idx(cycle, bus, master),
            0xE8 => self.op_eorb_idx(cycle, bus, master),
            0xE9 => self.op_adcb_idx(cycle, bus, master),
            0xEA => self.op_orab_idx(cycle, bus, master),
            0xEB => self.op_addb_idx(cycle, bus, master),
            0xEE => self.op_ldx_idx(cycle, bus, master),
            0xEF => self.op_stx_idx(cycle, bus, master),

            // --- 0xFx: B register extended + 16-bit extended ---
            0xF0 => self.op_subb_ext(cycle, bus, master),
            0xF1 => self.op_cmpb_ext(cycle, bus, master),
            0xF2 => self.op_sbcb_ext(cycle, bus, master),
            0xF4 => self.op_andb_ext(cycle, bus, master),
            0xF5 => self.op_bitb_ext(cycle, bus, master),
            0xF6 => self.op_ldab_ext(cycle, bus, master),
            0xF7 => self.op_stab_ext(cycle, bus, master),
            0xF8 => self.op_eorb_ext(cycle, bus, master),
            0xF9 => self.op_adcb_ext(cycle, bus, master),
            0xFA => self.op_orab_ext(cycle, bus, master),
            0xFB => self.op_addb_ext(cycle, bus, master),
            0xFE => self.op_ldx_ext(cycle, bus, master),
            0xFF => self.op_stx_ext(cycle, bus, master),

            // Unknown opcode - just fetch next
            _ => {
                self.state = ExecState::Fetch;
            }
        }
    }

    // --- Transfer ops (2 cycles: 1 fetch + 1 internal) ---

    /// TAP (0x06): Transfer A to Condition Codes.
    /// All flags set to corresponding bits in A.
    fn op_tap(&mut self, cycle: u8) {
        if cycle == 0 {
            self.cc = self.a;
            self.state = ExecState::Fetch;
        }
    }

    /// TPA (0x07): Transfer Condition Codes to A.
    /// No flags affected. Bits 6-7 read as 1 on real hardware.
    fn op_tpa(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.cc | CC_UNUSED_BITS;
            self.state = ExecState::Fetch;
        }
    }

    /// TAB (0x16): Transfer A to B.
    /// N, Z affected. V cleared.
    fn op_tab(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.a;
            self.set_flags_logical(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// TBA (0x17): Transfer B to A.
    /// N, Z affected. V cleared.
    fn op_tba(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.b;
            self.set_flags_logical(self.a);
            self.state = ExecState::Fetch;
        }
    }

    // --- Flag set/clear ops (2 cycles: 1 fetch + 1 internal) ---

    /// CLC (0x0C): Clear Carry flag.
    fn op_clc(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::C, false);
            self.state = ExecState::Fetch;
        }
    }

    /// SEC (0x0D): Set Carry flag.
    fn op_sec(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::C, true);
            self.state = ExecState::Fetch;
        }
    }

    /// CLV (0x0A): Clear Overflow flag.
    fn op_clv(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::V, false);
            self.state = ExecState::Fetch;
        }
    }

    /// SEV (0x0B): Set Overflow flag.
    fn op_sev(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::V, true);
            self.state = ExecState::Fetch;
        }
    }

    /// CLI (0x0E): Clear Interrupt mask (enable IRQ).
    fn op_cli(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::I, false);
            self.state = ExecState::Fetch;
        }
    }

    /// SEI (0x0F): Set Interrupt mask (disable IRQ).
    fn op_sei(&mut self, cycle: u8) {
        if cycle == 0 {
            self.set_flag(CcFlag::I, true);
            self.state = ExecState::Fetch;
        }
    }

    // --- Accumulator arithmetic ops (2 cycles: 1 fetch + 1 internal) ---

    /// ABA (0x1B): Add B to A (A = A + B).
    /// H, N, Z, V, C all affected.
    fn op_aba(&mut self, cycle: u8) {
        if cycle == 0 {
            let a = self.a;
            let b = self.b;
            let result16 = a as u16 + b as u16;
            let result = result16 as u8;

            let h = (a & 0x0F) + (b & 0x0F) > 0x0F;
            let c = result16 > 0xFF;
            let v = (!(a ^ b) & (a ^ result) & 0x80) != 0;

            self.a = result;
            self.set_flag(CcFlag::H, h);
            self.set_flags_arithmetic(result, v, c);
            self.state = ExecState::Fetch;
        }
    }

    /// SBA (0x10): Subtract B from A (A = A - B).
    /// N, Z, V, C affected. H not affected.
    fn op_sba(&mut self, cycle: u8) {
        if cycle == 0 {
            let a = self.a;
            let b = self.b;
            let (result, borrow) = a.overflowing_sub(b);

            let v = ((a ^ b) & (a ^ result) & 0x80) != 0;

            self.a = result;
            self.set_flags_arithmetic(result, v, borrow);
            self.state = ExecState::Fetch;
        }
    }

    /// CBA (0x11): Compare A to B (A - B, discard result).
    /// N, Z, V, C affected. H not affected.
    fn op_cba(&mut self, cycle: u8) {
        if cycle == 0 {
            let a = self.a;
            let b = self.b;
            let (result, borrow) = a.overflowing_sub(b);

            let v = ((a ^ b) & (a ^ result) & 0x80) != 0;

            self.set_flags_arithmetic(result, v, borrow);
            self.state = ExecState::Fetch;
        }
    }

    /// DAA (0x19): Decimal Adjust A after BCD addition.
    /// N, Z affected. V cleared. C can be set (never cleared).
    fn op_daa(&mut self, cycle: u8) {
        if cycle == 0 {
            let mut correction: u8 = 0;
            let mut carry = self.cc & (CcFlag::C as u8) != 0;
            let lsn = self.a & 0x0F;

            if lsn > 0x09 || (self.cc & (CcFlag::H as u8) != 0) {
                correction = 0x06;
            }

            if carry || (self.a >> 4) > 9 || ((self.a >> 4) > 8 && lsn > 9) {
                correction |= 0x60;
                carry = true;
            }

            let result = self.a.wrapping_add(correction);
            self.a = result;
            self.set_flag(CcFlag::N, self.a & 0x80 != 0);
            self.set_flag(CcFlag::Z, self.a == 0);
            self.set_flag(CcFlag::V, false);
            self.set_flag(CcFlag::C, carry);
            self.state = ExecState::Fetch;
        }
    }

    // --- 16-bit register ops (4 cycles: 1 fetch + 3 internal) ---

    /// INX (0x08): Increment Index Register X.
    /// Only Z flag affected (set if X == 0 after increment).
    fn op_inx(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.x = self.x.wrapping_add(1);
                self.set_flag(CcFlag::Z, self.x == 0);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// DEX (0x09): Decrement Index Register X.
    /// Only Z flag affected (set if X == 0 after decrement).
    fn op_dex(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.x = self.x.wrapping_sub(1);
                self.set_flag(CcFlag::Z, self.x == 0);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// INS (0x31): Increment Stack Pointer.
    /// No flags affected.
    fn op_ins(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.sp = self.sp.wrapping_add(1);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// DES (0x34): Decrement Stack Pointer.
    /// No flags affected.
    fn op_des(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.sp = self.sp.wrapping_sub(1);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// TSX (0x30): Transfer Stack Pointer to X (X = SP + 1).
    /// No flags affected. The +1 accounts for the 6800 SP pointing to
    /// the next free location (one below the last pushed byte).
    fn op_tsx(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.x = self.sp.wrapping_add(1);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// TXS (0x35): Transfer X to Stack Pointer (SP = X - 1).
    /// No flags affected. The -1 accounts for the 6800 SP convention.
    fn op_txs(&mut self, cycle: u8) {
        match cycle {
            0 | 1 => {
                self.state = ExecState::Execute(self.opcode, cycle + 1);
            }
            2 => {
                self.sp = self.x.wrapping_sub(1);
                self.state = ExecState::Fetch;
            }
            _ => self.state = ExecState::Fetch,
        }
    }

    /// Check for pending hardware interrupts at instruction boundary.
    /// Returns true if an interrupt is taken.
    /// Priority: NMI (edge-triggered) > IRQ (level, masked by I).
    fn handle_interrupts(&mut self, ints: InterruptState) -> bool {
        // NMI is edge-triggered: detect rising edge
        let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_previous);

        if nmi_edge {
            self.interrupt_type = InterruptType::Nmi;
            self.state = ExecState::Interrupt(0);
            return true;
        }

        // IRQ: level-sensitive, masked by I flag
        if ints.irq && (self.cc & CcFlag::I as u8) == 0 {
            self.interrupt_type = InterruptType::Irq;
            self.state = ExecState::Interrupt(0);
            return true;
        }

        false
    }
}

impl BusMasterComponent for M6800 {
    type Bus = dyn Bus<Address = u16, Data = u8>;

    fn tick_with_bus(&mut self, bus: &mut Self::Bus, master: BusMaster) -> bool {
        self.execute_cycle(bus, master);
        matches!(self.state, ExecState::Fetch)
    }
}

impl Cpu for M6800 {
    fn reset(&mut self, bus: &mut Self::Bus, master: BusMaster) {
        self.cc = CcFlag::I as u8 | CC_UNUSED_BITS;
        let hi = bus.read(master, 0xFFFE);
        let lo = bus.read(master, 0xFFFF);
        self.pc = u16::from_be_bytes([hi, lo]);
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {
        // Latch interrupts for sampling at instruction boundary
    }

    fn is_sleeping(&self) -> bool {
        self.halted || matches!(self.state, ExecState::WaitForInterrupt)
    }
}

impl CpuStateTrait for M6800 {
    type Snapshot = M6800State;

    fn snapshot(&self) -> M6800State {
        M6800State {
            a: self.a,
            b: self.b,
            x: self.x,
            sp: self.sp,
            pc: self.pc,
            cc: self.cc,
        }
    }
}

// -- Save state support ------------------------------------------------------

impl M6800 {
    /// Returns true when the CPU is at a saveable instruction boundary.
    pub fn is_at_save_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch | ExecState::WaitForInterrupt)
    }

    /// Returns true when the CPU is ready to fetch the next opcode.
    /// Used by the debugger for instruction-level stepping.
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }
}

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};
use crate::cpu::disasm::DisassembledInstruction;

impl Debuggable for M6800 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for M6800 {
    fn debug_pc(&self) -> u32 {
        u32::from(self.pc)
    }

    fn debug_at_instruction_boundary(&self) -> bool {
        self.at_instruction_boundary()
    }

    fn debug_disassemble(&self, addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        <Self as crate::cpu::Disassemble>::disassemble(addr as u16, bytes)
    }
}
