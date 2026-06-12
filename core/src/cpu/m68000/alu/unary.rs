//! Unary ALU: NEG / NEGX / NOT / CLR (line 0x4) and Scc (line 0x5).
//!
//! The four read-modify-write ops share one encoding shape — `0100 oooo ss
//! eeeeee` with a data-alterable destination — and differ only in the value
//! computed and the flag rule applied. EXT lives on the same line (0x4880 /
//! 0x48C0) but operates on a data register only.

use super::super::M68000;
use super::super::addressing::{AccessResult, Size, ea_cycles, sext8, sext16};
use super::super::flags::SrFlag;
use super::binary::size_from_bits;
use crate::core::{Bus, BusMaster};

/// Which line-0x4 read-modify-write operation to perform.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    /// NEGX: `0 - dst - X`, extended-arithmetic flag rule.
    Negx,
    /// CLR: write 0, logical flag rule (N=0, Z=1).
    Clr,
    /// NEG: `0 - dst`, arithmetic flag rule with X = C.
    Neg,
    /// NOT: `!dst`, logical flag rule.
    Not,
}

impl M68000 {
    /// NEGX / CLR / NEG / NOT <ea> — line 0x4, sub-ops 0x0/0x2/0x4/0x6,
    /// sizes 00-10, data-alterable destination.
    ///
    /// Flags: NEG sets N/Z/V/C from `0 - dst` with **X = C** (C is set for
    /// any non-zero operand); NEGX consumes X as borrow-in and follows the
    /// multi-precision Z rule (never set); NOT and CLR follow the logical
    /// rule (N/Z, V/C cleared, **X untouched**).
    pub(crate) fn op_unary<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        op: UnaryOp,
    ) -> AccessResult<()> {
        let Some(size) = size_from_bits(opcode >> 6) else {
            self.finish(4); // size 11 encodes the MOVE from/to SR/CCR group
            return Ok(());
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish(4);
            return Ok(());
        }

        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, size);
        let dst = self.ea_read(bus, master, ea, size)?;
        let result = match op {
            UnaryOp::Negx => self.subx_with_flags(size, 0, dst),
            UnaryOp::Neg => {
                let result = self.sub_with_flags(size, 0, dst);
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                result
            }
            UnaryOp::Not => {
                let result = !dst & size.mask();
                self.set_flags_logical(size, result);
                result
            }
            UnaryOp::Clr => {
                self.set_flags_logical(size, 0);
                0
            }
        };
        self.ea_write(bus, master, ea, size, result)?;

        let base = match (ea_mode == 0, size == Size::Long) {
            (true, false) => 4,
            (true, true) => 6,
            (false, false) => 8,
            (false, true) => 12,
        };
        self.finish(
            base + if ea_mode == 0 {
                0
            } else {
                ea_cycles(ea_mode, ea_reg, size)
            },
        );
        Ok(())
    }

    /// EXT.w / EXT.l Dn (0x4880 / 0x48C0): sign-extend the low byte to a
    /// word, or the low word to a long, within a data register.
    ///
    /// Flags: logical rule — N/Z from the extended result, V/C cleared,
    /// X untouched.
    pub(crate) fn op_ext(&mut self, opcode: u16) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        if opcode & 0x0040 != 0 {
            // EXT.l: word -> long
            let value = sext16(self.d[reg] as u16);
            self.d[reg] = value;
            self.set_flags_logical(Size::Long, value);
        } else {
            // EXT.w: byte -> word
            let value = sext8(self.d[reg] as u8) & 0xFFFF;
            self.d[reg] = (self.d[reg] & !0xFFFF) | value;
            self.set_flags_logical(Size::Word, value);
        }
        self.finish(4);
        Ok(())
    }

    /// TAS <ea> (0x4AC0, line 0x4 sub-op 0xA size bits 11): test-and-set —
    /// read the byte operand, set the flags from it, write it back with
    /// bit 7 set. On hardware this is one indivisible read-modify-write bus
    /// cycle; this core's two transactions are equivalent for a single bus
    /// master. Data-alterable destination only (0x4AFC is ILLEGAL, routed
    /// before this handler).
    ///
    /// Flags: N/Z from the value *before* bit 7 is set, V/C cleared,
    /// **X untouched**.
    pub(crate) fn op_tas<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish(4); // illegal destination
            return Ok(());
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
        let value = self.ea_read(bus, master, ea, Size::Byte)?;
        self.set_flags_logical(Size::Byte, value);
        self.ea_write(bus, master, ea, Size::Byte, value | 0x80)?;

        let base = if ea_mode == 0 { 4 } else { 14 };
        self.finish(
            base + if ea_mode == 0 {
                0
            } else {
                ea_cycles(ea_mode, ea_reg, Size::Byte)
            },
        );
        Ok(())
    }

    /// Scc <ea> (line 0x5, size bits 11, EA mode != An): write 0xFF to the
    /// byte destination if the condition holds, 0x00 otherwise.
    /// Data-alterable destination only.
    ///
    /// Flags: none (Scc never alters the CCR).
    pub(crate) fn op_scc<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 7 && ea_reg >= 2 {
            self.finish(4);
            return Ok(());
        }
        let cond = ((opcode >> 8) & 0xF) as u8;
        let taken = self.cc_true(cond);
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
        self.ea_write(bus, master, ea, Size::Byte, if taken { 0xFF } else { 0x00 })?;

        let base = if ea_mode == 0 {
            if taken { 6 } else { 4 }
        } else {
            8
        };
        self.finish(
            base + if ea_mode == 0 {
                0
            } else {
                ea_cycles(ea_mode, ea_reg, Size::Byte)
            },
        );
        Ok(())
    }
}
