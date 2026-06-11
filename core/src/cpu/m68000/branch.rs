//! Control flow: BRA / BSR / Bcc (line 0x6) and DBcc (line 0x5).
//!
//! Branch displacements are relative to the address of the word following
//! the opcode (where the optional 16-bit displacement word lives), for both
//! the 8-bit and 16-bit forms. Conditions are evaluated by
//! [`M68000::cc_true`]; none of these instructions alter the CCR.
//!
//! A control-flow target at an odd address raises an address error
//! (vector 3) on the target fetch. Exception entry lands in M5; until then
//! the odd target is flagged via `address_error`, the same as odd operand
//! accesses (the validation harness skips such vectors).

use super::M68000;
use super::addressing::{sext8, sext16};
use crate::core::{Bus, BusMaster};

impl M68000 {
    /// Load a new PC, flagging the address error a real 68000 would raise
    /// when fetching the first instruction word from an odd address.
    #[inline]
    fn set_pc_checked(&mut self, target: u32) {
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
}
