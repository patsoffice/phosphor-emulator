//! Control flow: BRA / BSR / Bcc (line 0x6), DBcc (line 0x5), and
//! JMP / JSR / RTS / RTR (line 0x4).
//!
//! Branch displacements are relative to the address of the word following
//! the opcode (where the optional 16-bit displacement word lives), for both
//! the 8-bit and 16-bit forms. Conditions are evaluated by
//! [`M68000::cc_true`]; none of these instructions alter the CCR except
//! RTR, which exists to restore it.
//!
//! A control-flow target at an odd address raises an address error
//! (vector 3) on the target fetch. Exception entry lands in M5; until then
//! the odd target is flagged via `address_error`, the same as odd operand
//! accesses (the validation harness skips such vectors).

use super::M68000;
use super::addressing::{Ea, Size, sext8, sext16};
use crate::core::{Bus, BusMaster};

/// Documented JMP timing per control addressing mode (M68000UM table 8-1);
/// JSR is uniformly 8 cycles more for the return-address push.
fn jump_cycles(mode: u8, reg: u8) -> u32 {
    match mode & 7 {
        2 => 8,  // (An)
        5 => 10, // d16(An)
        6 => 14, // d8(An,Xn)
        _ => match reg & 7 {
            0 => 10, // abs.w
            1 => 12, // abs.l
            2 => 10, // d16(PC)
            _ => 14, // d8(PC,Xn)
        },
    }
}

impl M68000 {
    /// Load a new PC, flagging the address error a real 68000 would raise
    /// when fetching the first instruction word from an odd address.
    #[inline]
    pub(crate) fn set_pc_checked(&mut self, target: u32) {
        if target & 1 != 0 {
            self.address_error = true;
        }
        self.pc = target;
    }

    /// BRA / BSR / Bcc `<label>` (line 0x6): PC-relative branch. The low
    /// opcode byte is the 8-bit displacement; zero selects the 16-bit form
    /// (one extension word). Condition 0 is BRA (always taken), condition 1
    /// is BSR, which pushes the return address — the word after the whole
    /// instruction — before branching.
    ///
    /// Flags: none.
    /// Cycles: taken 10 (BSR 18); not taken 8 (byte) / 12 (word).
    pub(crate) fn op_bcc<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        // The displacement base is the address of the word after the opcode.
        let base = self.pc;
        let disp8 = opcode as u8;
        let (disp, word_form) = if disp8 == 0 {
            (sext16(self.read_imm_word(bus, master)), true)
        } else {
            // disp8 == 0xFF selects a 32-bit displacement on 68020+ only;
            // the 68000 takes it as -1.
            (sext8(disp8), false)
        };
        let cond = ((opcode >> 8) & 0xF) as u8;
        match cond {
            // BSR: the return address is past the displacement word
            1 => {
                self.push_long(bus, master, self.pc);
                self.set_pc_checked(base.wrapping_add(disp));
                self.finish(18);
            }
            // BRA (condition 0 encodes T) and taken Bcc
            _ if self.cc_true(cond) => {
                self.set_pc_checked(base.wrapping_add(disp));
                self.finish(10);
            }
            // Not taken: the word form pays for its extension-word fetch
            _ => self.finish(if word_form { 12 } else { 8 }),
        }
    }

    /// DBcc Dn,`<label>` (line 0x5, size bits 11, EA mode 001): loop
    /// primitive. If the condition holds, fall through. Otherwise decrement
    /// the low word of Dn (upper word untouched) and branch back unless the
    /// counter wrapped from 0 to -1.
    ///
    /// Flags: none.
    /// Cycles: condition true 12; loop taken 10; counter expired 14.
    pub(crate) fn op_dbcc<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let base = self.pc;
        let disp = sext16(self.read_imm_word(bus, master));
        let cond = ((opcode >> 8) & 0xF) as u8;
        if self.cc_true(cond) {
            self.finish(12);
            return;
        }
        let reg = (opcode & 7) as usize;
        let counter = (self.d[reg] as u16).wrapping_sub(1);
        self.d[reg] = (self.d[reg] & 0xFFFF_0000) | counter as u32;
        if counter == 0xFFFF {
            self.finish(14);
        } else {
            self.set_pc_checked(base.wrapping_add(disp));
            self.finish(10);
        }
    }

    /// JMP `<ea>` / JSR `<ea>` (0x4EC0 / 0x4E80): load PC from a
    /// control-mode effective address. JSR first pushes the return address —
    /// the word after the extension words, which is where PC sits once the
    /// EA is decoded.
    ///
    /// Flags: none.
    pub(crate) fn op_jmp_jsr<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        call: bool,
    ) {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Control addressing only: register direct, (An)+/-(An), and #imm
        // are illegal here (the exception lands in M5).
        if !(matches!(ea_mode, 2 | 5 | 6) || (ea_mode == 7 && ea_reg < 4)) {
            self.finish(4);
            return;
        }
        // The size only governs operand access, which never happens for an
        // address-only decode; control modes have no side effects.
        let Ea::Mem(target) = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word) else {
            unreachable!("control addressing modes always resolve to memory");
        };
        if call {
            self.push_long(bus, master, self.pc);
        }
        self.set_pc_checked(target);
        self.finish(jump_cycles(ea_mode, ea_reg) + if call { 8 } else { 0 });
    }

    /// RTS (0x4E75): pop the return address into PC.
    ///
    /// Flags: none. 16 cycles.
    pub(crate) fn op_rts<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let target = self.pop_long(bus, master);
        self.set_pc_checked(target);
        self.finish(16);
    }

    /// RTR (0x4E77): pop a word into the CCR, then pop the return address
    /// into PC. Pairs with a MOVE SR,-(SP) / PEA-style prologue to restore
    /// caller flags.
    ///
    /// Flags: X/N/Z/V/C loaded from the stacked word (only the five
    /// implemented CCR bits; the system byte is untouched). 20 cycles.
    pub(crate) fn op_rtr<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let ccr = self.pop_word(bus, master);
        self.sr = (self.sr & 0xFF00) | (ccr & 0x001F);
        let target = self.pop_long(bus, master);
        self.set_pc_checked(target);
        self.finish(20);
    }
}
