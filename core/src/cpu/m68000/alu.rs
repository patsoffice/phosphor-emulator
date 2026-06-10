//! M68000 integer ALU — shared flag-setting arithmetic cores.
//!
//! Submodules hold the instruction families (`binary` = ADD/SUB/CMP now;
//! AND/OR/EOR and the X/BCD variants land in M2). The sized add/subtract
//! cores live here so every family shares one flag computation.

pub mod binary;

use super::M68000;
use super::addressing::Size;
use super::flags::SrFlag;

impl M68000 {
    /// Sized add core: returns `(a + b)` masked to `size` and sets N/Z/V/C.
    ///
    /// X is *not* touched here — ADD-family callers set X = C themselves,
    /// CMP-family callers leave X alone (the X-flag rules in `flags.rs`).
    pub(crate) fn add_with_flags(&mut self, size: Size, a: u32, b: u32) -> u32 {
        let mask = size.mask();
        let sign = size.sign_bit();
        let (a, b) = (a & mask, b & mask);
        let result = a.wrapping_add(b) & mask;

        let carry = (a as u64 + b as u64) > mask as u64;
        // Overflow: both operands share a sign that the result lost.
        let overflow = !(a ^ b) & (a ^ result) & sign != 0;

        self.set_flag(SrFlag::N, result & sign != 0);
        self.set_flag(SrFlag::Z, result == 0);
        self.set_flag(SrFlag::V, overflow);
        self.set_flag(SrFlag::C, carry);
        result
    }

    /// Sized subtract core: returns `(a - b)` masked to `size` and sets
    /// N/Z/V/C (C = borrow). Used by SUB, CMP, and the immediate forms.
    ///
    /// X is *not* touched here — SUB-family callers set X = C themselves,
    /// CMP never alters X.
    pub(crate) fn sub_with_flags(&mut self, size: Size, a: u32, b: u32) -> u32 {
        let mask = size.mask();
        let sign = size.sign_bit();
        let (a, b) = (a & mask, b & mask);
        let result = a.wrapping_sub(b) & mask;

        let borrow = b > a;
        // Overflow: operand signs differ and the result took b's sign.
        let overflow = (a ^ b) & (a ^ result) & sign != 0;

        self.set_flag(SrFlag::N, result & sign != 0);
        self.set_flag(SrFlag::Z, result == 0);
        self.set_flag(SrFlag::V, overflow);
        self.set_flag(SrFlag::C, borrow);
        result
    }
}
