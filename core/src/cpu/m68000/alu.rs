//! M68000 integer ALU — shared flag-setting arithmetic cores.
//!
//! Submodules hold the instruction families. The sized add/subtract cores
//! and the logical N/Z/V/C rule live here so every family shares one flag
//! computation.

pub mod binary;
pub mod muldiv;
pub mod shift;
pub mod unary;

use super::M68000;
use super::addressing::Size;
use super::flags::SrFlag;

impl M68000 {
    /// Set the flags for a logical / data-movement result: N and Z from the
    /// sized value, V and C cleared. X is *never* touched by this rule
    /// (AND/OR/EOR/NOT/TST/MOVE/CLR/Scc all leave it alone).
    pub(crate) fn set_flags_logical(&mut self, size: Size, value: u32) {
        let value = value & size.mask();
        self.set_flag(SrFlag::N, value & size.sign_bit() != 0);
        self.set_flag(SrFlag::Z, value == 0);
        self.set_flag(SrFlag::V, false);
        self.set_flag(SrFlag::C, false);
    }

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

    /// Extended add core for ADDX: returns `(a + b + X)` masked to `size`
    /// and sets N/V/C and **X = C**. Z follows the multi-precision rule:
    /// cleared by a non-zero result, *unchanged* by a zero one, so a chained
    /// sum reports zero only if every limb was zero.
    pub(crate) fn addx_with_flags(&mut self, size: Size, a: u32, b: u32) -> u32 {
        let x = self.flag_is_set(SrFlag::X) as u32;
        let mask = size.mask();
        let sign = size.sign_bit();
        let (a, b) = (a & mask, b & mask);
        let result = a.wrapping_add(b).wrapping_add(x) & mask;

        let carry = (a as u64 + b as u64 + x as u64) > mask as u64;
        let overflow = !(a ^ b) & (a ^ result) & sign != 0;

        self.set_flag(SrFlag::N, result & sign != 0);
        if result != 0 {
            self.set_flag(SrFlag::Z, false);
        }
        self.set_flag(SrFlag::V, overflow);
        self.set_flag(SrFlag::C, carry);
        self.set_flag(SrFlag::X, carry);
        result
    }

    /// Extended subtract core for SUBX/NEGX: returns `(a - b - X)` masked to
    /// `size` and sets N/V/C (C = borrow) and **X = C**. Z follows the same
    /// multi-precision rule as [`Self::addx_with_flags`].
    pub(crate) fn subx_with_flags(&mut self, size: Size, a: u32, b: u32) -> u32 {
        let x = self.flag_is_set(SrFlag::X) as u32;
        let mask = size.mask();
        let sign = size.sign_bit();
        let (a, b) = (a & mask, b & mask);
        let result = a.wrapping_sub(b).wrapping_sub(x) & mask;

        let borrow = (b as u64 + x as u64) > a as u64;
        let overflow = (a ^ b) & (a ^ result) & sign != 0;

        self.set_flag(SrFlag::N, result & sign != 0);
        if result != 0 {
            self.set_flag(SrFlag::Z, false);
        }
        self.set_flag(SrFlag::V, overflow);
        self.set_flag(SrFlag::C, borrow);
        self.set_flag(SrFlag::X, borrow);
        result
    }
}
