mod alu;
mod branch;
pub mod disasm;
mod load_store;
mod stack;
mod transfer;

use crate::core::{Bus, BusMaster, bus::InterruptState, component::BusMasterComponent};
use crate::cpu::{
    Cpu,
    state::{CpuStateTrait, M6809State},
};
use phosphor_macros::Saveable;

pub use super::m68xx::CcFlag;
use super::m68xx::{Acc, M68xxAlu};

/// Interrupt type being processed by the M6809 interrupt state machine.
/// SWI/SWI2/SWI3 have their own handlers and do not use this enum.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum InterruptType {
    None = 0,
    Nmi = 1,
    Firq = 2,
    Irq = 3,
}

/// Fields are ordered to match the save-state serialization layout (version 2).
#[derive(Saveable)]
#[save_version(2)]
pub struct M6809 {
    // Registers (a,b,x,y,u,s,pc,cc)
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub cc: u8,

    // Internal state (serialized)
    /// Previous NMI line state for edge detection
    pub(crate) nmi_previous: bool,
    /// True when the bus HALT line is asserted (TSC/RDY logic)
    pub(crate) halted: bool,

    // Execution temporaries — not saved, reset to defaults on load
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    #[save_skip(default)]
    pub(crate) opcode: u8,
    /// Transient scratch storage for postbyte, high-byte, or RMW operand
    #[save_skip(default)]
    pub(crate) scratch: u8,
    #[save_skip(default)]
    pub(crate) temp_addr: u16,
    /// Interrupt type being processed
    #[save_skip(default = InterruptType::None)]
    pub(crate) interrupt_type: InterruptType,
    /// Countdown for internal cycles during indexed addressing
    #[save_skip(default)]
    pub(crate) indexed_internal: u8,
    #[save_skip(default)]
    #[allow(dead_code)]
    resume_delay: u8, // For TSC/RDY release timing
}

#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    Fetch,
    Execute(u8, u8),      // (opcode, cycle)
    ExecutePage2(u8, u8), // (opcode, cycle) for 0x10 prefix
    ExecutePage3(u8, u8), // (opcode, cycle) for 0x11 prefix
    /// Hardware interrupt response sequence (NMI/IRQ/FIRQ push + vector)
    Interrupt(u8),
    /// CWAI: all registers pushed, waiting for interrupt
    WaitForInterrupt,
    /// SYNC: waiting for any interrupt signal
    SyncWait,
}

impl Default for M6809 {
    fn default() -> Self {
        Self::new()
    }
}

impl M6809 {
    pub fn new() -> Self {
        Self {
            a: 0,
            b: 0,
            dp: 0,
            x: 0,
            y: 0,
            u: 0,
            s: 0,
            pc: 0,
            cc: 0,
            nmi_previous: false,
            halted: false,
            state: ExecState::Fetch,
            opcode: 0,
            scratch: 0,
            temp_addr: 0,
            interrupt_type: InterruptType::None,
            indexed_internal: 0,
            resume_delay: 0,
        }
    }

    pub(crate) fn get_d(&self) -> u16 {
        u16::from_be_bytes([self.a, self.b])
    }

    pub(crate) fn set_d(&mut self, val: u16) {
        let bytes = val.to_be_bytes();
        self.a = bytes[0];
        self.b = bytes[1];
    }
}

impl M68xxAlu for M6809 {
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
}

impl M6809 {
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
                    return; // Interrupt taken, state changed to Interrupt sequence
                }

                self.opcode = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(self.opcode, 0);
            }
            ExecState::Execute(op, cyc) => {
                self.execute_instruction(op, cyc, bus, master);
            }
            ExecState::ExecutePage2(op, cyc) => {
                self.execute_instruction_page2(op, cyc, bus, master);
            }
            ExecState::ExecutePage3(op, cyc) => {
                self.execute_instruction_page3(op, cyc, bus, master);
            }
            ExecState::Interrupt(cycle) => {
                self.execute_interrupt(cycle, bus, master);
            }
            ExecState::WaitForInterrupt => {
                self.wait_for_interrupt(bus, master);
            }
            ExecState::SyncWait => {
                self.sync_wait(bus, master);
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
            // Page 2 Prefix (0x10)
            0x10 => {
                if cycle == 0 {
                    let next_op = bus.read(master, self.pc);
                    self.pc = self.pc.wrapping_add(1);
                    self.state = ExecState::ExecutePage2(next_op, 0);
                }
            }

            // Page 3 Prefix (0x11)
            0x11 => {
                if cycle == 0 {
                    let next_op = bus.read(master, self.pc);
                    self.pc = self.pc.wrapping_add(1);
                    self.state = ExecState::ExecutePage3(next_op, 0);
                }
            }

            // Misc inherent/immediate
            0x12 => self.op_nop(cycle),
            0x13 => self.op_sync(cycle, bus, master),
            0x16 => self.op_lbra(opcode, cycle, bus, master),
            0x17 => self.op_lbsr(opcode, cycle, bus, master),
            0x19 => self.op_daa(cycle),
            0x1A => self.op_orcc(cycle, bus, master),
            0x1C => self.op_andcc(cycle, bus, master),
            0x1D => self.op_sex(cycle),

            // Direct-page unary/shift (0x00-0x0F)
            0x00 | 0x01 => self.op_neg_direct(opcode, cycle, bus, master),
            0x03 => self.op_com_direct(opcode, cycle, bus, master),
            0x04 | 0x05 => self.op_lsr_direct(opcode, cycle, bus, master),
            0x06 => self.op_ror_direct(opcode, cycle, bus, master),
            0x07 => self.op_asr_direct(opcode, cycle, bus, master),
            0x08 => self.op_asl_direct(opcode, cycle, bus, master),
            0x09 => self.op_rol_direct(opcode, cycle, bus, master),
            0x0A | 0x0B => self.op_dec_direct(opcode, cycle, bus, master),
            0x0C => self.op_inc_direct(opcode, cycle, bus, master),
            0x0D => self.op_tst_direct(opcode, cycle, bus, master),
            0x0E => self.op_jmp_direct(opcode, cycle, bus, master),
            0x0F => self.op_clr_direct(opcode, cycle, bus, master),

            // Transfer/Exchange
            0x1E => self.op_exg(cycle, bus, master),
            0x1F => self.op_tfr(cycle, bus, master),

            // ALU instructions (A register inherent)
            0x3D => self.op_mul(cycle),
            0x40 | 0x41 => self.op_nega(cycle),
            0x43 => self.op_coma(cycle),
            0x44 | 0x45 => self.op_lsra(cycle),
            0x46 => self.op_rora(cycle),
            0x47 => self.op_asra(cycle),
            0x48 => self.op_asla(cycle),
            0x49 => self.op_rola(cycle),
            0x4A | 0x4B => self.op_deca(cycle),
            0x4C => self.op_inca(cycle),
            0x4D => self.op_tsta(cycle),
            0x4F => self.op_clra(cycle),

            // LEA instructions
            0x30 => self.op_leax(opcode, cycle, bus, master),
            0x31 => self.op_leay(opcode, cycle, bus, master),
            0x32 => self.op_leas(opcode, cycle, bus, master),
            0x33 => self.op_leau(opcode, cycle, bus, master),

            // Subroutine / Return / Interrupt
            0x39 => self.op_rts(cycle, bus, master),
            0x3A => self.op_abx(cycle),
            0x3B => self.op_rti(cycle, bus, master),
            0x3C => self.op_cwai(cycle, bus, master),
            0x3F => self.op_swi(cycle, bus, master),

            // Stack operations
            0x34 => self.op_pshs(cycle, bus, master),
            0x35 => self.op_puls(cycle, bus, master),
            0x36 => self.op_pshu(cycle, bus, master),
            0x37 => self.op_pulu(cycle, bus, master),

            // Branch instructions (Short)
            0x8D => self.op_bsr(opcode, cycle, bus, master),
            0x20 => self.op_bra(opcode, cycle, bus, master),
            0x21 => self.op_brn(opcode, cycle, bus, master),
            0x22 => self.op_bhi(opcode, cycle, bus, master),
            0x23 => self.op_bls(opcode, cycle, bus, master),
            0x24 => self.op_bcc(opcode, cycle, bus, master),
            0x25 => self.op_bcs(opcode, cycle, bus, master),
            0x26 => self.op_bne(opcode, cycle, bus, master),
            0x27 => self.op_beq(opcode, cycle, bus, master),
            0x28 => self.op_bvc(opcode, cycle, bus, master),
            0x29 => self.op_bvs(opcode, cycle, bus, master),
            0x2A => self.op_bpl(opcode, cycle, bus, master),
            0x2B => self.op_bmi(opcode, cycle, bus, master),
            0x2C => self.op_bge(opcode, cycle, bus, master),
            0x2D => self.op_blt(opcode, cycle, bus, master),
            0x2E => self.op_bgt(opcode, cycle, bus, master),
            0x2F => self.op_ble(opcode, cycle, bus, master),

            // Indexed memory unary/shift (0x60-0x6F)
            0x60 | 0x61 => self.op_neg_indexed(opcode, cycle, bus, master),
            0x63 => self.op_com_indexed(opcode, cycle, bus, master),
            0x64 | 0x65 => self.op_lsr_indexed(opcode, cycle, bus, master),
            0x66 => self.op_ror_indexed(opcode, cycle, bus, master),
            0x67 => self.op_asr_indexed(opcode, cycle, bus, master),
            0x68 => self.op_asl_indexed(opcode, cycle, bus, master),
            0x69 => self.op_rol_indexed(opcode, cycle, bus, master),
            0x6A | 0x6B => self.op_dec_indexed(opcode, cycle, bus, master),
            0x6C => self.op_inc_indexed(opcode, cycle, bus, master),
            0x6D => self.op_tst_indexed(opcode, cycle, bus, master),
            0x6E => self.op_jmp_indexed(opcode, cycle, bus, master),
            0x6F => self.op_clr_indexed(opcode, cycle, bus, master),

            // Extended unary/shift (0x70-0x7F)
            0x70 | 0x71 => self.op_neg_extended(opcode, cycle, bus, master),
            0x73 => self.op_com_extended(opcode, cycle, bus, master),
            0x74 | 0x75 => self.op_lsr_extended(opcode, cycle, bus, master),
            0x76 => self.op_ror_extended(opcode, cycle, bus, master),
            0x77 => self.op_asr_extended(opcode, cycle, bus, master),
            0x78 => self.op_asl_extended(opcode, cycle, bus, master),
            0x79 => self.op_rol_extended(opcode, cycle, bus, master),
            0x7A | 0x7B => self.op_dec_extended(opcode, cycle, bus, master),
            0x7C => self.op_inc_extended(opcode, cycle, bus, master),
            0x7D => self.op_tst_extended(opcode, cycle, bus, master),
            0x7E => self.op_jmp_extended(opcode, cycle, bus, master),
            0x7F => self.op_clr_extended(opcode, cycle, bus, master),

            // ALU immediate (A register)
            0x80 => self.op_suba_imm(cycle, bus, master),
            0x81 => self.op_cmpa_imm(cycle, bus, master),
            0x82 => self.op_sbca_imm(cycle, bus, master),
            0x83 => self.op_subd_imm(opcode, cycle, bus, master),
            0x84 => self.op_anda_imm(cycle, bus, master),
            0x85 => self.op_bita_imm(cycle, bus, master),
            0x88 => self.op_eora_imm(cycle, bus, master),
            0x89 => self.op_adca_imm(cycle, bus, master),
            0x8A => self.op_ora_imm(cycle, bus, master),
            0x8B => self.op_adda_imm(cycle, bus, master),
            0x8C => self.op_cmpx_imm(opcode, cycle, bus, master),
            0x8E => self.op_ldx_imm(opcode, cycle, bus, master),

            // ALU/load/store direct (A register page)
            0x90 => self.op_suba_direct(opcode, cycle, bus, master),
            0x91 => self.op_cmpa_direct(opcode, cycle, bus, master),
            0x92 => self.op_sbca_direct(opcode, cycle, bus, master),
            0x93 => self.op_subd_direct(opcode, cycle, bus, master),
            0x94 => self.op_anda_direct(opcode, cycle, bus, master),
            0x95 => self.op_bita_direct(opcode, cycle, bus, master),
            0x96 => self.op_lda_direct(opcode, cycle, bus, master),
            0x97 => self.op_sta_direct(opcode, cycle, bus, master),
            0x98 => self.op_eora_direct(opcode, cycle, bus, master),
            0x99 => self.op_adca_direct(opcode, cycle, bus, master),
            0x9A => self.op_ora_direct(opcode, cycle, bus, master),
            0x9B => self.op_adda_direct(opcode, cycle, bus, master),
            0x9C => self.op_cmpx_direct(opcode, cycle, bus, master),
            0x9E => self.op_ldx_direct(opcode, cycle, bus, master),
            0x9F => self.op_stx_direct(opcode, cycle, bus, master),

            // ALU/load/store indexed (A register page, 0xA0-0xAF)
            0xA0 => self.op_suba_indexed(opcode, cycle, bus, master),
            0xA1 => self.op_cmpa_indexed(opcode, cycle, bus, master),
            0xA2 => self.op_sbca_indexed(opcode, cycle, bus, master),
            0xA3 => self.op_subd_indexed(opcode, cycle, bus, master),
            0xA4 => self.op_anda_indexed(opcode, cycle, bus, master),
            0xA5 => self.op_bita_indexed(opcode, cycle, bus, master),
            0xA6 => self.op_lda_indexed(opcode, cycle, bus, master),
            0xA7 => self.op_sta_indexed(opcode, cycle, bus, master),
            0xA8 => self.op_eora_indexed(opcode, cycle, bus, master),
            0xA9 => self.op_adca_indexed(opcode, cycle, bus, master),
            0xAA => self.op_ora_indexed(opcode, cycle, bus, master),
            0xAB => self.op_adda_indexed(opcode, cycle, bus, master),
            0xAC => self.op_cmpx_indexed(opcode, cycle, bus, master),
            0xAD => self.op_jsr_indexed(opcode, cycle, bus, master),
            0xAE => self.op_ldx_indexed(opcode, cycle, bus, master),
            0xAF => self.op_stx_indexed(opcode, cycle, bus, master),

            // ALU extended (A register)
            0xB0 => self.op_suba_extended(opcode, cycle, bus, master),
            0xB1 => self.op_cmpa_extended(opcode, cycle, bus, master),
            0xB2 => self.op_sbca_extended(opcode, cycle, bus, master),
            0xB3 => self.op_subd_extended(opcode, cycle, bus, master),
            0xB4 => self.op_anda_extended(opcode, cycle, bus, master),
            0xB5 => self.op_bita_extended(opcode, cycle, bus, master),
            0xB6 => self.op_lda_extended(opcode, cycle, bus, master),
            0xB7 => self.op_sta_extended(opcode, cycle, bus, master),
            0xB8 => self.op_eora_extended(opcode, cycle, bus, master),
            0xB9 => self.op_adca_extended(opcode, cycle, bus, master),
            0xBA => self.op_ora_extended(opcode, cycle, bus, master),
            0xBB => self.op_adda_extended(opcode, cycle, bus, master),
            0xBC => self.op_cmpx_extended(opcode, cycle, bus, master),
            0xBD => self.op_jsr_extended(opcode, cycle, bus, master),
            0xBE => self.op_ldx_extended(opcode, cycle, bus, master),
            0xBF => self.op_stx_extended(opcode, cycle, bus, master),

            // ALU instructions (B register inherent)
            0x50 | 0x51 => self.op_negb(cycle),
            0x53 => self.op_comb(cycle),
            0x54 | 0x55 => self.op_lsrb(cycle),
            0x56 => self.op_rorb(cycle),
            0x57 => self.op_asrb(cycle),
            0x58 => self.op_aslb(cycle),
            0x59 => self.op_rolb(cycle),
            0x5A | 0x5B => self.op_decb(cycle),
            0x5C => self.op_incb(cycle),
            0x5D => self.op_tstb(cycle),
            0x5F => self.op_clrb(cycle),
            // ALU immediate (B register)
            0xC0 => self.op_subb_imm(cycle, bus, master),
            0xC1 => self.op_cmpb_imm(cycle, bus, master),
            0xC2 => self.op_sbcb_imm(cycle, bus, master),
            0xC3 => self.op_addd_imm(opcode, cycle, bus, master),
            0xC4 => self.op_andb_imm(cycle, bus, master),
            0xC5 => self.op_bitb_imm(cycle, bus, master),
            0xC8 => self.op_eorb_imm(cycle, bus, master),
            0xC9 => self.op_adcb_imm(cycle, bus, master),
            0xCA => self.op_orb_imm(cycle, bus, master),
            0xCB => self.op_addb_imm(cycle, bus, master),
            0xCC => self.op_ldd_imm(opcode, cycle, bus, master),
            0xCE => self.op_ldu_imm(opcode, cycle, bus, master),

            // ALU/load/store direct (B register page)
            0xD0 => self.op_subb_direct(opcode, cycle, bus, master),
            0xD1 => self.op_cmpb_direct(opcode, cycle, bus, master),
            0xD2 => self.op_sbcb_direct(opcode, cycle, bus, master),
            0xD3 => self.op_addd_direct(opcode, cycle, bus, master),
            0xD4 => self.op_andb_direct(opcode, cycle, bus, master),
            0xD5 => self.op_bitb_direct(opcode, cycle, bus, master),
            0xD6 => self.op_ldb_direct(opcode, cycle, bus, master),
            0xD7 => self.op_stb_direct(opcode, cycle, bus, master),
            0xD8 => self.op_eorb_direct(opcode, cycle, bus, master),
            0xD9 => self.op_adcb_direct(opcode, cycle, bus, master),
            0xDA => self.op_orb_direct(opcode, cycle, bus, master),
            0xDB => self.op_addb_direct(opcode, cycle, bus, master),
            0xDC => self.op_ldd_direct(opcode, cycle, bus, master),
            0xDD => self.op_std_direct(opcode, cycle, bus, master),
            0xDE => self.op_ldu_direct(opcode, cycle, bus, master),
            0xDF => self.op_stu_direct(opcode, cycle, bus, master),

            // ALU/load/store indexed (B register page, 0xE0-0xEF)
            0xE0 => self.op_subb_indexed(opcode, cycle, bus, master),
            0xE1 => self.op_cmpb_indexed(opcode, cycle, bus, master),
            0xE2 => self.op_sbcb_indexed(opcode, cycle, bus, master),
            0xE3 => self.op_addd_indexed(opcode, cycle, bus, master),
            0xE4 => self.op_andb_indexed(opcode, cycle, bus, master),
            0xE5 => self.op_bitb_indexed(opcode, cycle, bus, master),
            0xE6 => self.op_ldb_indexed(opcode, cycle, bus, master),
            0xE7 => self.op_stb_indexed(opcode, cycle, bus, master),
            0xE8 => self.op_eorb_indexed(opcode, cycle, bus, master),
            0xE9 => self.op_adcb_indexed(opcode, cycle, bus, master),
            0xEA => self.op_orb_indexed(opcode, cycle, bus, master),
            0xEB => self.op_addb_indexed(opcode, cycle, bus, master),
            0xEC => self.op_ldd_indexed(opcode, cycle, bus, master),
            0xED => self.op_std_indexed(opcode, cycle, bus, master),
            0xEE => self.op_ldu_indexed(opcode, cycle, bus, master),
            0xEF => self.op_stu_indexed(opcode, cycle, bus, master),

            // ALU extended (B register)
            0xF0 => self.op_subb_extended(opcode, cycle, bus, master),
            0xF1 => self.op_cmpb_extended(opcode, cycle, bus, master),
            0xF2 => self.op_sbcb_extended(opcode, cycle, bus, master),
            0xF3 => self.op_addd_extended(opcode, cycle, bus, master),
            0xF4 => self.op_andb_extended(opcode, cycle, bus, master),
            0xF5 => self.op_bitb_extended(opcode, cycle, bus, master),
            0xF6 => self.op_ldb_extended(opcode, cycle, bus, master),
            0xF7 => self.op_stb_extended(opcode, cycle, bus, master),
            0xF8 => self.op_eorb_extended(opcode, cycle, bus, master),
            0xF9 => self.op_adcb_extended(opcode, cycle, bus, master),
            0xFA => self.op_orb_extended(opcode, cycle, bus, master),
            0xFB => self.op_addb_extended(opcode, cycle, bus, master),
            0xFC => self.op_ldd_extended(opcode, cycle, bus, master),
            0xFD => self.op_std_extended(opcode, cycle, bus, master),
            0xFE => self.op_ldu_extended(opcode, cycle, bus, master),
            0xFF => self.op_stu_extended(opcode, cycle, bus, master),

            // Load/store immediate
            0x86 => self.op_lda_imm(cycle, bus, master),
            0xC6 => self.op_ldb_imm(cycle, bus, master),

            0x9D => self.op_jsr_direct(opcode, cycle, bus, master),

            // Unknown opcode - just fetch next
            _ => {
                self.state = ExecState::Fetch;
            }
        }
    }

    fn execute_instruction_page2<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match opcode {
            // SWI2
            0x3F => self.op_swi2(cycle, bus, master),

            // Long branches
            0x21 => self.op_lbrn(opcode, cycle, bus, master),
            0x22 => self.op_lbhi(opcode, cycle, bus, master),
            0x23 => self.op_lbls(opcode, cycle, bus, master),
            0x24 => self.op_lbcc(opcode, cycle, bus, master),
            0x25 => self.op_lbcs(opcode, cycle, bus, master),
            0x26 => self.op_lbne(opcode, cycle, bus, master),
            0x27 => self.op_lbeq(opcode, cycle, bus, master),
            0x28 => self.op_lbvc(opcode, cycle, bus, master),
            0x29 => self.op_lbvs(opcode, cycle, bus, master),
            0x2A => self.op_lbpl(opcode, cycle, bus, master),
            0x2B => self.op_lbmi(opcode, cycle, bus, master),
            0x2C => self.op_lbge(opcode, cycle, bus, master),
            0x2D => self.op_lblt(opcode, cycle, bus, master),
            0x2E => self.op_lbgt(opcode, cycle, bus, master),
            0x2F => self.op_lble(opcode, cycle, bus, master),

            // CMPD (immediate, direct, indexed, extended)
            0x83 => self.op_cmpd_imm(opcode, cycle, bus, master),
            0x93 => self.op_cmpd_direct(opcode, cycle, bus, master),
            0xA3 => self.op_cmpd_indexed(opcode, cycle, bus, master),
            0xB3 => self.op_cmpd_extended(opcode, cycle, bus, master),

            // CMPY (immediate, direct, indexed, extended)
            0x8C => self.op_cmpy_imm(opcode, cycle, bus, master),
            0x9C => self.op_cmpy_direct(opcode, cycle, bus, master),
            0xAC => self.op_cmpy_indexed(opcode, cycle, bus, master),
            0xBC => self.op_cmpy_extended(opcode, cycle, bus, master),

            // LDY / STY (immediate, direct, indexed, extended)
            0x8E => self.op_ldy_imm(opcode, cycle, bus, master),
            0x9E => self.op_ldy_direct(opcode, cycle, bus, master),
            0x9F => self.op_sty_direct(opcode, cycle, bus, master),
            0xAE => self.op_ldy_indexed(opcode, cycle, bus, master),
            0xAF => self.op_sty_indexed(opcode, cycle, bus, master),
            0xBE => self.op_ldy_extended(opcode, cycle, bus, master),
            0xBF => self.op_sty_extended(opcode, cycle, bus, master),

            // LDS / STS (immediate, direct, indexed, extended)
            0xCE => self.op_lds_imm(opcode, cycle, bus, master),
            0xDE => self.op_lds_direct(opcode, cycle, bus, master),
            0xDF => self.op_sts_direct(opcode, cycle, bus, master),
            0xEE => self.op_lds_indexed(opcode, cycle, bus, master),
            0xEF => self.op_sts_indexed(opcode, cycle, bus, master),
            0xFE => self.op_lds_extended(opcode, cycle, bus, master),
            0xFF => self.op_sts_extended(opcode, cycle, bus, master),

            _ => self.state = ExecState::Fetch,
        }
    }

    fn execute_instruction_page3<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match opcode {
            // SWI3
            0x3F => self.op_swi3(cycle, bus, master),

            // CMPU (immediate, direct, indexed, extended)
            0x83 => self.op_cmpu_imm(opcode, cycle, bus, master),
            0x93 => self.op_cmpu_direct(opcode, cycle, bus, master),
            0xA3 => self.op_cmpu_indexed(opcode, cycle, bus, master),
            0xB3 => self.op_cmpu_extended(opcode, cycle, bus, master),

            // CMPS (immediate, direct, indexed, extended)
            0x8C => self.op_cmps_imm(opcode, cycle, bus, master),
            0x9C => self.op_cmps_direct(opcode, cycle, bus, master),
            0xAC => self.op_cmps_indexed(opcode, cycle, bus, master),
            0xBC => self.op_cmps_extended(opcode, cycle, bus, master),

            _ => self.state = ExecState::Fetch,
        }
    }

    /// Check for pending hardware interrupts at instruction boundary.
    /// Returns true if an interrupt is taken (state changed to Interrupt sequence).
    /// Priority: NMI (edge-triggered) > FIRQ (level, masked by F) > IRQ (level, masked by I).
    fn handle_interrupts(&mut self, ints: InterruptState) -> bool {
        // NMI is edge-triggered: detect rising edge
        let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_previous);

        if nmi_edge {
            self.interrupt_type = InterruptType::Nmi;
            self.state = ExecState::Interrupt(0);
            return true;
        }

        // FIRQ: level-sensitive, masked by F flag
        if ints.firq && (self.cc & CcFlag::F as u8) == 0 {
            self.interrupt_type = InterruptType::Firq;
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

impl BusMasterComponent for M6809 {
    type Bus = dyn Bus<Address = u16, Data = u8>;

    fn tick_with_bus(&mut self, bus: &mut Self::Bus, master: BusMaster) -> bool {
        self.execute_cycle(bus, master);
        // Return true if instruction boundary reached
        matches!(self.state, ExecState::Fetch)
    }
}

impl Cpu for M6809 {
    fn reset(&mut self, bus: &mut Self::Bus, master: BusMaster) {
        self.cc = CcFlag::I as u8 | CcFlag::F as u8; // IRQ/FIRQ masked
        let hi = bus.read(master, 0xFFFE);
        let lo = bus.read(master, 0xFFFF);
        self.pc = u16::from_be_bytes([hi, lo]);
    }

    fn signal_interrupt(&mut self, _int: InterruptState) {
        // Latch interrupts for sampling at instruction boundary
    }

    fn is_sleeping(&self) -> bool {
        self.halted
            || matches!(
                self.state,
                ExecState::WaitForInterrupt | ExecState::SyncWait
            )
    }
}

impl CpuStateTrait for M6809 {
    type Snapshot = M6809State;

    fn snapshot(&self) -> M6809State {
        M6809State {
            a: self.a,
            b: self.b,
            dp: self.dp,
            x: self.x,
            y: self.y,
            u: self.u,
            s: self.s,
            pc: self.pc,
            cc: self.cc,
        }
    }
}

impl M6809 {
    /// Returns true when the CPU is at a saveable instruction boundary.
    pub fn is_at_save_boundary(&self) -> bool {
        matches!(
            self.state,
            ExecState::Fetch | ExecState::WaitForInterrupt | ExecState::SyncWait
        )
    }

    /// Returns true when the CPU is ready to fetch the next opcode.
    /// Used by the debugger for instruction-level stepping.
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }
}

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};
use crate::cpu::disasm::DisassembledInstruction;

impl Debuggable for M6809 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for M6809 {
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
