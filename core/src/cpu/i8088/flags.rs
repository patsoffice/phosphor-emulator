//! Intel 8088 FLAGS register definitions and helpers.
//!
//! The 8088 FLAGS register is 16 bits wide. Only 9 flags are defined;
//! undefined bits read as 1 on an 8088 (bits 12-15 are always 1, bit 1 is
//! always 1). We store the full 16-bit value and provide per-flag accessors.

/// PF uses the same even-parity rule as the Z80's P/V, so it comes from the
/// shared helper rather than a private table.
pub use crate::cpu::flags::parity;

/// Individual flag bits within the 16-bit FLAGS register.
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Flag {
    CF = 0x0001, // Carry
    PF = 0x0004, // Parity (even parity of low 8 bits of result)
    AF = 0x0010, // Auxiliary carry (BCD half-carry)
    ZF = 0x0040, // Zero
    SF = 0x0080, // Sign
    TF = 0x0100, // Trap (single-step)
    IF = 0x0200, // Interrupt enable
    DF = 0x0400, // Direction (0 = up, 1 = down)
    OF = 0x0800, // Overflow
}

impl From<Flag> for u16 {
    fn from(f: Flag) -> u16 {
        f as u16
    }
}

/// Bits that are always 1 on an 8088 (bits 12-15 and bit 1).
const ALWAYS_ONE: u16 = 0xF002;

/// All defined flag bits ORed together.
const DEFINED_MASK: u16 = 0x0FD5;

/// Test whether a flag is set.
#[inline]
pub fn get(flags: u16, flag: Flag) -> bool {
    flags & (flag as u16) != 0
}

/// Set or clear a flag.
#[inline]
pub fn set(flags: &mut u16, flag: Flag, value: bool) {
    if value {
        *flags |= flag as u16;
    } else {
        *flags &= !(flag as u16);
    }
}

/// Normalize the FLAGS register: set the always-one bits and clear undefined bits.
#[inline]
pub fn normalize(flags: u16) -> u16 {
    (flags & DEFINED_MASK) | ALWAYS_ONE
}

// ---------------------------------------------------------------------------
// Composite flag updates
// ---------------------------------------------------------------------------

/// Update SF, ZF, PF from an 8-bit result.
#[inline]
pub fn update_szp8(flags: &mut u16, result: u8) {
    set(flags, Flag::SF, result & 0x80 != 0);
    set(flags, Flag::ZF, result == 0);
    set(flags, Flag::PF, parity(result));
}

/// Update SF, ZF, PF from a 16-bit result.
/// SF and ZF use the full 16-bit value; PF uses only the low byte.
#[inline]
pub fn update_szp16(flags: &mut u16, result: u16) {
    set(flags, Flag::SF, result & 0x8000 != 0);
    set(flags, Flag::ZF, result == 0);
    set(flags, Flag::PF, parity(result as u8));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_set_clear() {
        let mut flags = normalize(0);
        assert!(!get(flags, Flag::CF));
        set(&mut flags, Flag::CF, true);
        assert!(get(flags, Flag::CF));
        set(&mut flags, Flag::CF, false);
        assert!(!get(flags, Flag::CF));
    }

    #[test]
    fn always_one_bits() {
        let flags = normalize(0);
        // Bit 1 always set
        assert_ne!(flags & 0x0002, 0);
        // Bits 12-15 always set
        assert_eq!(flags & 0xF000, 0xF000);
    }

    #[test]
    fn normalize_clears_undefined() {
        // Set every bit
        let flags = normalize(0xFFFF);
        // Only defined flags + always-one should remain
        assert_eq!(flags, DEFINED_MASK | ALWAYS_ONE);
    }

    #[test]
    fn parity_spot_checks() {
        assert!(parity(0x00)); // 0 bits set → even
        assert!(!parity(0x01)); // 1 bit set → odd
        assert!(parity(0x03)); // 2 bits set → even
        assert!(!parity(0x07)); // 3 bits set → odd
        assert!(parity(0xFF)); // 8 bits set → even
        assert!(!parity(0x80)); // 1 bit set → odd
    }

    #[test]
    fn update_szp8_zero() {
        let mut flags = normalize(0);
        update_szp8(&mut flags, 0);
        assert!(get(flags, Flag::ZF));
        assert!(!get(flags, Flag::SF));
        assert!(get(flags, Flag::PF)); // 0 has even parity
    }

    #[test]
    fn update_szp8_negative() {
        let mut flags = normalize(0);
        update_szp8(&mut flags, 0x80);
        assert!(!get(flags, Flag::ZF));
        assert!(get(flags, Flag::SF));
        assert!(!get(flags, Flag::PF)); // 0x80 has 1 bit → odd
    }

    #[test]
    fn update_szp16_zero() {
        let mut flags = normalize(0);
        update_szp16(&mut flags, 0);
        assert!(get(flags, Flag::ZF));
        assert!(!get(flags, Flag::SF));
        assert!(get(flags, Flag::PF));
    }

    #[test]
    fn update_szp16_high_sign() {
        let mut flags = normalize(0);
        update_szp16(&mut flags, 0x8000);
        assert!(!get(flags, Flag::ZF));
        assert!(get(flags, Flag::SF));
        // Low byte is 0x00 → even parity
        assert!(get(flags, Flag::PF));
    }
}
