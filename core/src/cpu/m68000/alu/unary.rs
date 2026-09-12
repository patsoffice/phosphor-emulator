//! Unary ALU: NEG / NEGX / NOT / CLR (line 0x4) and Scc (line 0x5).
//!
//! The four read-modify-write ops share one encoding shape — `0100 oooo ss
//! eeeeee` with a data-alterable destination — and differ only in the value
//! computed and the flag rule applied. EXT lives on the same line (0x4880 /
//! 0x48C0) but operates on a data register only.

use super::super::M68000;
use super::super::addressing::{AccessResult, Ea, Size, ea_internal, sext8, sext16};
use super::super::flags::SrFlag;
use super::binary::size_from_bits;
use crate::core::{Bus16, BusMaster};

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
    pub(crate) fn op_unary<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        op: UnaryOp,
    ) -> AccessResult<()> {
        let Some(size) = size_from_bits(opcode >> 6) else {
            self.finish_from_bus(bus, master, 0); // size 11 encodes the MOVE from/to SR/CCR group
            return Ok(());
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish_from_bus(bus, master, 0);
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
        self.ea_write_rmw(bus, master, ea, size, result)?;

        // A register destination makes no operand transfer, so only the long
        // ALU pass is left; a memory one reads and writes, both counted.
        let internal = if ea_mode == 0 {
            if size == Size::Long { 2 } else { 0 }
        } else {
            ea_internal(ea_mode, ea_reg)
        };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }

    /// EXT.w / EXT.l Dn (0x4880 / 0x48C0): sign-extend the low byte to a
    /// word, or the low word to a long, within a data register.
    ///
    /// Flags: logical rule — N/Z from the extended result, V/C cleared,
    /// X untouched.
    pub(crate) fn op_ext<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
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
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// TAS <ea> (0x4AC0, line 0x4 sub-op 0xA size bits 11): test-and-set —
    /// read the byte operand, set the flags from it, write it back with
    /// bit 7 set. Data-alterable destination only (0x4AFC is ILLEGAL, routed
    /// before this handler).
    ///
    /// **In memory this is one bus cycle, not two**, and it is the only
    /// instruction on the part that drives one: the address strobe is held
    /// across the read and the write so no other master can get between the
    /// test and the set. Ten clocks, where two ordinary transfers would be
    /// eight, because the part spends two between the halves. See
    /// [`M68000::read_modify_write_byte`].
    ///
    /// Flags: N/Z from the value *before* bit 7 is set, V/C cleared,
    /// **X untouched**.
    pub(crate) fn op_tas<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish_from_bus(bus, master, 0); // illegal destination
            return Ok(());
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
        if ea_mode == 0 {
            // A register destination touches no bus at all: the read, the test
            // and the write-back are internal, and the opcode's own refill is
            // the whole cost.
            let value = self.ea_read(bus, master, ea, Size::Byte)?;
            self.set_flags_logical(Size::Byte, value);
            self.ea_write(bus, master, ea, Size::Byte, value | 0x80)?;
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        }
        let Ea::Mem(addr) = ea else {
            unreachable!("the register form returned above and no other mode is legal here")
        };
        let value = self.read_modify_write_byte(bus, master, addr, |byte| byte | 0x80)?;
        self.set_flags_logical(Size::Byte, u32::from(value));

        // Two transfers, the held cycle and the refill behind the opcode, and
        // six clocks left: two inside the cycle and four the part spends before
        // it issues the fetch. They run ahead of that fetch, which is why the
        // refill lands ten clocks after the cycle starts rather than four.
        self.finish_from_bus_address_first(bus, master, 6 + ea_internal(ea_mode, ea_reg));
        Ok(())
    }

    /// Scc <ea> (line 0x5, size bits 11, EA mode != An): write 0xFF to the
    /// byte destination if the condition holds, 0x00 otherwise.
    /// Data-alterable destination only.
    ///
    /// Flags: none (Scc never alters the CCR).
    pub(crate) fn op_scc<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 7 && ea_reg >= 2 {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        }
        let cond = ((opcode >> 8) & 0xF) as u8;
        let taken = self.cc_true(cond);
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
        // Scc reads its destination before writing it, like the other
        // read-modify-write forms. This core used to say the opposite in a
        // comment here and charge the missing transfer as time off the bus, so
        // the total came out right with the wrong activity underneath it: the
        // trace records `Scc -(A5)` as a read, a program read and a write, at
        // fourteen clocks, which is three transfers and the predecrement.
        if ea_mode != 0 {
            let _ = self.ea_read(bus, master, ea, Size::Byte)?;
        }
        self.ea_write_rmw(bus, master, ea, Size::Byte, if taken { 0xFF } else { 0x00 })?;

        // In a register the whole cost is the condition test, and a true one
        // takes two clocks longer than a false one. A memory form is all bus
        // besides its addressing mode's own arithmetic.
        let internal = if ea_mode == 0 {
            if taken { 2 } else { 0 }
        } else {
            ea_internal(ea_mode, ea_reg)
        };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }
}
