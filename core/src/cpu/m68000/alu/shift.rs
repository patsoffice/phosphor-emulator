//! Shifts and rotates — ASL/ASR, LSL/LSR, ROXL/ROXR, ROL/ROR (line 0xE).
//!
//! Register forms encode `1110 ccc d ss i tt rrr`: count/register `ccc`,
//! direction `d` (1 = left), size `ss`, immediate-or-register count `i`
//! (immediate count 0 means 8; a register count is taken modulo 64), shift
//! type `tt`, and the destination Dn. Memory forms use size bits 11 with
//! the type in bits 10-9: one word, shifted by exactly one bit.
//!
//! Flag rules (shared by all counts, including the degenerate count 0):
//!
//! - N/Z always come from the result; C is the last bit shifted out, or 0
//!   when the count is 0 (except ROXx, where a count of 0 copies X to C).
//! - **X = C** for the shifts and the extend rotates, but plain ROL/ROR
//!   never touch X — and no variant touches X when the count is 0.
//! - V is cleared by everything except ASL, which sets it if the sign bit
//!   changed at any point during the shift.

use super::super::M68000;
use super::super::addressing::{Size, ea_cycles};
use super::super::flags::SrFlag;
use super::binary::size_from_bits;
use crate::core::{Bus, BusMaster};

/// Shift type from bits 4-3 (register forms) or 10-9 (memory forms).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShiftKind {
    /// ASL / ASR — arithmetic (ASR sign-fills; ASL detects sign change in V)
    Arithmetic,
    /// LSL / LSR — logical (zero-fill)
    Logical,
    /// ROXL / ROXR — rotate through the X flag ((size+1)-bit rotation)
    RotateX,
    /// ROL / ROR — plain rotate
    Rotate,
}

impl ShiftKind {
    fn from_bits(bits: u16) -> ShiftKind {
        match bits & 3 {
            0 => ShiftKind::Arithmetic,
            1 => ShiftKind::Logical,
            2 => ShiftKind::RotateX,
            _ => ShiftKind::Rotate,
        }
    }
}

impl M68000 {
    /// Shared shift/rotate core: applies `kind`/`left` by `count` bit
    /// positions to the sized `src`, sets every affected flag, and returns
    /// the masked result.
    fn shift_core(&mut self, size: Size, kind: ShiftKind, left: bool, count: u32, src: u32) -> u32 {
        let bits = size.bytes() * 8;
        let mask = size.mask();
        let sign = size.sign_bit();
        let src = src & mask;

        if count == 0 {
            // Degenerate count: flags are still set (C = 0, or C = X for
            // ROXx), but X and the operand are untouched.
            self.set_flags_logical(size, src);
            if kind == ShiftKind::RotateX {
                self.set_flag(SrFlag::C, self.flag_is_set(SrFlag::X));
            }
            return src;
        }

        let (result, carry) = match (kind, left) {
            (ShiftKind::Arithmetic | ShiftKind::Logical, true) => {
                let result = if count >= bits {
                    0
                } else {
                    (src << count) & mask
                };
                let carry = count <= bits && (src >> (bits - count)) & 1 != 0;
                (result, carry)
            }
            (ShiftKind::Logical, false) => {
                let result = if count >= bits { 0 } else { src >> count };
                let carry = count <= bits && (src >> (count - 1)) & 1 != 0;
                (result, carry)
            }
            (ShiftKind::Arithmetic, false) => {
                let negative = src & sign != 0;
                let result = if count >= bits {
                    if negative { mask } else { 0 }
                } else {
                    let fill = if negative { mask & !(mask >> count) } else { 0 };
                    (src >> count) | fill
                };
                // Same carry rule as LSR: the last bit out for counts up to
                // the width (the sign bit at exactly the width), 0 beyond it
                // — observed hardware behavior, verified against the test
                // vectors (the sign does NOT keep shifting into C).
                let carry = count <= bits && (src >> (count - 1)) & 1 != 0;
                (result, carry)
            }
            (ShiftKind::Rotate, _) => {
                let k = count % bits;
                let result = if k == 0 {
                    src
                } else if left {
                    ((src << k) | (src >> (bits - k))) & mask
                } else {
                    ((src >> k) | (src << (bits - k))) & mask
                };
                // The bit rotated out last is the one that wrapped around:
                // the result's LSB for ROL, its MSB for ROR.
                let carry = if left {
                    result & 1 != 0
                } else {
                    result & sign != 0
                };
                (result, carry)
            }
            (ShiftKind::RotateX, _) => {
                // (size+1)-bit rotation through X, so the count works
                // modulo size+1.
                let span = bits + 1;
                let k = count % span;
                let x = self.flag_is_set(SrFlag::X) as u64;
                let value = src as u64 | (x << bits);
                let rotated = if k == 0 {
                    value
                } else if left {
                    ((value << k) | (value >> (span - k))) & ((1u64 << span) - 1)
                } else {
                    ((value >> k) | (value << (span - k))) & ((1u64 << span) - 1)
                };
                ((rotated as u32) & mask, rotated >> bits & 1 != 0)
            }
        };

        self.set_flags_logical(size, result);
        self.set_flag(SrFlag::C, carry);
        // Plain rotates never touch X; everything else sets X = C.
        if kind != ShiftKind::Rotate {
            self.set_flag(SrFlag::X, carry);
        }
        // ASL overflow: V is set if the sign bit changed at any point, i.e.
        // unless the top count+1 bits of the source were all equal.
        if kind == ShiftKind::Arithmetic && left {
            let overflow = if count >= bits {
                src != 0
            } else {
                let top = (mask << (bits - count - 1)) & mask;
                src & top != 0 && src & top != top
            };
            self.set_flag(SrFlag::V, overflow);
        }
        result
    }

    /// Register-form shift/rotate (line 0xE, size bits 00-10): immediate
    /// count 1-8 or a register count modulo 64, any size, Dn destination.
    pub(crate) fn op_shift_reg(&mut self, opcode: u16) {
        let size = size_from_bits(opcode >> 6).unwrap();
        let left = opcode & 0x0100 != 0;
        let kind = ShiftKind::from_bits(opcode >> 3);
        let reg = (opcode & 7) as usize;
        let count = if opcode & 0x0020 != 0 {
            self.d[((opcode >> 9) & 7) as usize] & 63
        } else {
            let c = ((opcode >> 9) & 7) as u32;
            if c == 0 { 8 } else { c }
        };

        let src = self.d[reg];
        let result = self.shift_core(size, kind, left, count, src);
        self.d[reg] = (src & !size.mask()) | result;

        let base = if size == Size::Long { 8 } else { 6 };
        self.finish(base + 2 * count);
    }

    /// Memory-form shift/rotate (line 0xE, size bits 11, type in bits
    /// 10-9): one word at a memory-alterable EA, shifted by exactly one bit.
    pub(crate) fn op_shift_mem<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let kind = ShiftKind::from_bits(opcode >> 9);
        let left = opcode & 0x0100 != 0;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode < 2 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish(4);
            return;
        }

        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let src = self.ea_read(bus, master, ea, Size::Word);
        let result = self.shift_core(Size::Word, kind, left, 1, src);
        self.ea_write(bus, master, ea, Size::Word, result);

        self.finish(8 + ea_cycles(ea_mode, ea_reg, Size::Word));
    }
}
