//! Shared ALU trait for the Motorola 68xx CPU family (M6800, M6809, and derivatives).
//!
//! These CPUs share identical 8-bit ALU operations (add, subtract, compare, logic,
//! shift/rotate) and condition code flag semantics. The first 6 CC bits (C, V, Z, N,
//! I, H) are identical across the family; M6809 adds F and E in bits 6-7.

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum CcFlag {
    C = 0x01, // Carry
    V = 0x02, // Overflow
    Z = 0x04, // Zero
    N = 0x08, // Negative
    I = 0x10, // IRQ mask
    H = 0x20, // Half carry
    F = 0x40, // FIRQ mask (M6809 only)
    E = 0x80, // Entire flag (M6809 only)
}

impl From<CcFlag> for u8 {
    fn from(f: CcFlag) -> u8 {
        f as u8
    }
}

/// Accumulator selector for the M68xx register-pair ALU operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Acc {
    A,
    B,
}

/// Shared ALU operations for the M68xx CPU family.
///
/// Implementors provide register accessors; all ALU logic is provided as
/// default methods. These are monomorphized at compile time for zero overhead.
pub trait M68xxAlu {
    fn reg(&mut self, acc: Acc) -> &mut u8;
    fn reg_cc(&mut self) -> &mut u8;

    // --- Flag helpers ---

    #[inline]
    fn set_flag(&mut self, flag: CcFlag, set: bool) {
        super::flags::set_flag(self.reg_cc(), flag, set);
    }

    /// Set N, Z, V (cleared) flags for logical operations.
    #[inline]
    fn set_flags_logical(&mut self, result: u8) {
        self.set_flag(CcFlag::N, result & 0x80 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, false);
    }

    /// Set N, Z, V, C flags for arithmetic operations.
    #[inline]
    fn set_flags_arithmetic(&mut self, result: u8, overflow: bool, carry: bool) {
        self.set_flag(CcFlag::N, result & 0x80 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, overflow);
        self.set_flag(CcFlag::C, carry);
    }

    /// Set N, Z, V (cleared) flags for 16-bit logical operations (LDX, LDS, etc.).
    #[inline]
    fn set_flags_logical16(&mut self, result: u16) {
        self.set_flag(CcFlag::N, result & 0x8000 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, false);
    }

    /// Set N, Z, V, C flags for left-shift/rotate operations (ASL, ROL).
    /// V = N XOR C (post-operation).
    #[inline]
    fn set_flags_shift_left(&mut self, result: u8, carry: bool) {
        let n = result & 0x80 != 0;
        self.set_flag(CcFlag::N, n);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::C, carry);
        self.set_flag(CcFlag::V, n ^ carry);
    }

    /// Set N, Z, C flags for right-shift/rotate operations (LSR, ASR, ROR).
    /// V is not affected by right-shift operations.
    #[inline]
    fn set_flags_shift_right(&mut self, result: u8, carry: bool) {
        self.set_flag(CcFlag::N, result & 0x80 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::C, carry);
    }

    // --- Binary ALU operations ---

    /// ADD: acc = acc + operand. Sets H, N, Z, V, C.
    #[inline]
    fn perform_add(&mut self, acc: Acc, operand: u8) {
        let r = *self.reg(acc);
        let (result, carry) = r.overflowing_add(operand);
        let half_carry = (r & 0x0F) + (operand & 0x0F) > 0x0F;
        let overflow = (r ^ operand) & 0x80 == 0 && (r ^ result) & 0x80 != 0;
        *self.reg(acc) = result;
        self.set_flag(CcFlag::H, half_carry);
        self.set_flags_arithmetic(result, overflow, carry);
    }

    /// ADC: acc = acc + operand + C. Sets H, N, Z, V, C.
    #[inline]
    fn perform_adc(&mut self, acc: Acc, operand: u8) {
        let carry_in = (*self.reg_cc() & CcFlag::C as u8) as u16;
        let r = *self.reg(acc);
        let r_u16 = r as u16;
        let m_u16 = operand as u16;
        let sum = r_u16 + m_u16 + carry_in;
        let result = sum as u8;
        let carry_out = sum > 0xFF;
        let half_carry = (r & 0x0F) + (operand & 0x0F) + (carry_in as u8) > 0x0F;
        let overflow = (r ^ operand) & 0x80 == 0 && (r ^ result) & 0x80 != 0;
        *self.reg(acc) = result;
        self.set_flag(CcFlag::H, half_carry);
        self.set_flags_arithmetic(result, overflow, carry_out);
    }

    /// SUB: acc = acc - operand. Sets N, Z, V, C.
    #[inline]
    fn perform_sub(&mut self, acc: Acc, operand: u8) {
        let r = *self.reg(acc);
        let (result, borrow) = r.overflowing_sub(operand);
        let overflow = (r ^ operand) & 0x80 != 0 && (r ^ result) & 0x80 != 0;
        *self.reg(acc) = result;
        self.set_flags_arithmetic(result, overflow, borrow);
    }

    /// SBC: acc = acc - operand - C. Sets N, Z, V, C.
    #[inline]
    fn perform_sbc(&mut self, acc: Acc, operand: u8) {
        let carry = (*self.reg_cc() & CcFlag::C as u8) as u16;
        let r = *self.reg(acc);
        let r16 = r as u16;
        let m = operand as u16;
        let diff = r16.wrapping_sub(m).wrapping_sub(carry);
        let result = diff as u8;
        let borrow = r16 < m + carry;
        let overflow = (r ^ operand) & 0x80 != 0 && (r ^ result) & 0x80 != 0;
        *self.reg(acc) = result;
        self.set_flags_arithmetic(result, overflow, borrow);
    }

    /// CMP: acc - operand (discard result). Sets N, Z, V, C.
    #[inline]
    fn perform_cmp(&mut self, acc: Acc, operand: u8) {
        let r = *self.reg(acc);
        let (result, borrow) = r.overflowing_sub(operand);
        let overflow = (r ^ operand) & 0x80 != 0 && (r ^ result) & 0x80 != 0;
        self.set_flags_arithmetic(result, overflow, borrow);
    }

    /// AND: acc = acc & operand. Sets N, Z. V cleared.
    #[inline]
    fn perform_and(&mut self, acc: Acc, operand: u8) {
        *self.reg(acc) &= operand;
        let r = *self.reg(acc);
        self.set_flags_logical(r);
    }

    /// BIT: acc & operand (discard result). Sets N, Z. V cleared.
    #[inline]
    fn perform_bit(&mut self, acc: Acc, operand: u8) {
        let result = *self.reg(acc) & operand;
        self.set_flags_logical(result);
    }

    /// EOR: acc = acc ^ operand. Sets N, Z. V cleared.
    #[inline]
    fn perform_eor(&mut self, acc: Acc, operand: u8) {
        *self.reg(acc) ^= operand;
        let r = *self.reg(acc);
        self.set_flags_logical(r);
    }

    /// OR: acc = acc | operand. Sets N, Z. V cleared.
    #[inline]
    fn perform_or(&mut self, acc: Acc, operand: u8) {
        *self.reg(acc) |= operand;
        let r = *self.reg(acc);
        self.set_flags_logical(r);
    }

    // --- Unary ALU operations ---

    /// NEG: result = 0 - val. Sets N, Z, V, C.
    #[inline]
    fn perform_neg(&mut self, val: u8) -> u8 {
        let (result, borrow) = (0u8).overflowing_sub(val);
        let overflow = val == 0x80;
        self.set_flags_arithmetic(result, overflow, borrow);
        result
    }

    /// COM: result = ~val. Sets N, Z. V cleared. C set.
    #[inline]
    fn perform_com(&mut self, val: u8) -> u8 {
        let result = !val;
        self.set_flags_logical(result);
        self.set_flag(CcFlag::C, true);
        result
    }

    /// CLR: result = 0. N=0, Z=1, V=0, C=0.
    #[inline]
    fn perform_clr(&mut self) -> u8 {
        self.set_flag(CcFlag::N, false);
        self.set_flag(CcFlag::Z, true);
        self.set_flag(CcFlag::V, false);
        self.set_flag(CcFlag::C, false);
        0
    }

    /// INC: result = val + 1. Sets N, Z, V. C not affected.
    #[inline]
    fn perform_inc(&mut self, val: u8) -> u8 {
        let overflow = val == 0x7F;
        let result = val.wrapping_add(1);
        self.set_flag(CcFlag::N, result & 0x80 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, overflow);
        result
    }

    /// DEC: result = val - 1. Sets N, Z, V. C not affected.
    #[inline]
    fn perform_dec(&mut self, val: u8) -> u8 {
        let overflow = val == 0x80;
        let result = val.wrapping_sub(1);
        self.set_flag(CcFlag::N, result & 0x80 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, overflow);
        result
    }

    /// TST: set flags based on val, no modification.
    /// Default: sets N, Z. V cleared. (M6809 behavior.)
    /// M6800 overrides to also clear C.
    #[inline]
    fn perform_tst(&mut self, val: u8) {
        self.set_flags_logical(val);
    }

    // --- Shift/rotate operations ---

    /// ASL (Arithmetic Shift Left): bit 7 -> C, bits shift left, 0 -> bit 0.
    #[inline]
    fn perform_asl(&mut self, val: u8) -> u8 {
        let carry = val & 0x80 != 0;
        let result = val << 1;
        self.set_flags_shift_left(result, carry);
        result
    }

    /// ASR (Arithmetic Shift Right): bit 7 preserved, bits shift right, bit 0 -> C.
    #[inline]
    fn perform_asr(&mut self, val: u8) -> u8 {
        let carry = val & 0x01 != 0;
        let result = ((val as i8) >> 1) as u8;
        self.set_flags_shift_right(result, carry);
        result
    }

    /// LSR (Logical Shift Right): 0 -> bit 7, bits shift right, bit 0 -> C.
    #[inline]
    fn perform_lsr(&mut self, val: u8) -> u8 {
        let carry = val & 0x01 != 0;
        let result = val >> 1;
        self.set_flags_shift_right(result, carry);
        result
    }

    /// ROL (Rotate Left through Carry): bit 7 -> C, bits shift left, old C -> bit 0.
    #[inline]
    fn perform_rol(&mut self, val: u8) -> u8 {
        let old_carry = *self.reg_cc() & (CcFlag::C as u8) != 0;
        let new_carry = val & 0x80 != 0;
        let result = (val << 1) | (old_carry as u8);
        self.set_flags_shift_left(result, new_carry);
        result
    }

    /// ROR (Rotate Right through Carry): bit 0 -> C, bits shift right, old C -> bit 7.
    #[inline]
    fn perform_ror(&mut self, val: u8) -> u8 {
        let old_carry = *self.reg_cc() & (CcFlag::C as u8) != 0;
        let new_carry = val & 0x01 != 0;
        let result = (val >> 1) | ((old_carry as u8) << 7);
        self.set_flags_shift_right(result, new_carry);
        result
    }
}
