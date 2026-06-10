//! M68000 status register (SR) flags, privilege handling, and condition codes.
//!
//! The 16-bit SR is split into the user-visible condition code register
//! (CCR, low byte) and the system byte (high byte). User-mode code can only
//! touch the CCR; the system byte is supervisor-only.
//!
//! # X (extend) flag semantics
//!
//! X is the subtle CCR bit. It shadows C for *arithmetic* results but is used
//! as the carry-in for multi-precision sequences, so the rules differ by
//! instruction family:
//!
//! - **Arithmetic** (ADD, SUB, NEG, shifts/rotates through extend): set X = C.
//! - **Logical and data movement** (AND, OR, EOR, NOT, TST, MOVE, CLR, Scc):
//!   leave X untouched even though they rewrite N/Z/V/C.
//! - **Extended arithmetic** (ADDX, SUBX, NEGX, ABCD, SBCD, ROXL/ROXR):
//!   consume X as carry-in and set X = C on the way out.
//!
//! Every instruction's doc comment states which rule it follows.

use super::M68000;
use crate::cpu::flags::{flag_is_set_u16, set_flag_u16};

/// M68000 status register flag bits.
///
/// Bits 0-4 form the CCR; bits 8-10 are the interrupt priority mask; S and T
/// live in the system byte. (T0 / M / I-of-68020+ bits are reserved-zero on
/// the 68000.)
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SrFlag {
    /// Carry (CCR bit 0)
    C = 1 << 0,
    /// Overflow (CCR bit 1)
    V = 1 << 1,
    /// Zero (CCR bit 2)
    Z = 1 << 2,
    /// Negative (CCR bit 3)
    N = 1 << 3,
    /// Extend (CCR bit 4) — see module docs for the X-flag rules
    X = 1 << 4,
    /// Interrupt priority mask bit 0 (system byte)
    I0 = 1 << 8,
    /// Interrupt priority mask bit 1 (system byte)
    I1 = 1 << 9,
    /// Interrupt priority mask bit 2 (system byte)
    I2 = 1 << 10,
    /// Supervisor mode (system byte); selects SSP/USP as the active A7
    S = 1 << 13,
    /// Trace mode (system byte)
    T = 1 << 15,
}

impl From<SrFlag> for u16 {
    fn from(f: SrFlag) -> u16 {
        f as u16
    }
}

impl M68000 {
    /// Set or clear a single SR flag bit.
    #[inline]
    pub fn set_flag(&mut self, flag: SrFlag, set: bool) {
        set_flag_u16(&mut self.sr, flag, set);
    }

    /// Test whether a single SR flag bit is set.
    #[inline]
    pub fn flag_is_set(&self, flag: SrFlag) -> bool {
        flag_is_set_u16(self.sr, flag)
    }

    /// Current interrupt priority mask (0-7) from SR bits 8-10.
    #[inline]
    pub fn interrupt_mask(&self) -> u8 {
        ((self.sr >> 8) & 7) as u8
    }

    /// Set the interrupt priority mask (0-7) in SR bits 8-10.
    #[inline]
    pub fn set_interrupt_mask(&mut self, level: u8) {
        self.sr = (self.sr & !0x0700) | ((level as u16 & 7) << 8);
    }

    /// Switch between supervisor and user mode, swapping the active stack
    /// pointer.
    ///
    /// `a[7]` always holds the *active* SP; the inactive one is parked in
    /// `usp`/`ssp`. Must be called from every path that can rewrite the SR
    /// system byte (MOVE to SR, ANDI/ORI/EORI to SR, RTE, exception entry,
    /// reset) so the swap happens exactly once per S-bit change.
    pub fn set_supervisor(&mut self, on: bool) {
        if self.flag_is_set(SrFlag::S) == on {
            return;
        }
        if on {
            // Entering supervisor mode: park USP, activate SSP.
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        } else {
            // Dropping to user mode: park SSP, activate USP.
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        }
        self.set_flag(SrFlag::S, on);
    }

    /// Evaluate a 4-bit condition code (Bcc/Scc/DBcc `cond` field) against
    /// the current CCR.
    pub fn cc_true(&self, cond: u8) -> bool {
        let c = self.flag_is_set(SrFlag::C);
        let v = self.flag_is_set(SrFlag::V);
        let z = self.flag_is_set(SrFlag::Z);
        let n = self.flag_is_set(SrFlag::N);
        match cond & 0xF {
            0x0 => true,         // T  — always
            0x1 => false,        // F  — never
            0x2 => !c && !z,     // HI — high
            0x3 => c || z,       // LS — low or same
            0x4 => !c,           // CC — carry clear
            0x5 => c,            // CS — carry set
            0x6 => !z,           // NE — not equal
            0x7 => z,            // EQ — equal
            0x8 => !v,           // VC — overflow clear
            0x9 => v,            // VS — overflow set
            0xA => !n,           // PL — plus
            0xB => n,            // MI — minus
            0xC => n == v,       // GE — greater or equal
            0xD => n != v,       // LT — less than
            0xE => !z && n == v, // GT — greater than
            _ => z || n != v,    // LE — less or equal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_test_flags() {
        let mut cpu = M68000::new();
        cpu.sr = 0;
        cpu.set_flag(SrFlag::X, true);
        cpu.set_flag(SrFlag::C, true);
        assert_eq!(cpu.sr, 0x0011);
        assert!(cpu.flag_is_set(SrFlag::X));
        assert!(cpu.flag_is_set(SrFlag::C));
        assert!(!cpu.flag_is_set(SrFlag::Z));
        cpu.set_flag(SrFlag::X, false);
        assert_eq!(cpu.sr, 0x0001);
    }

    #[test]
    fn interrupt_mask_round_trip() {
        let mut cpu = M68000::new();
        assert_eq!(cpu.interrupt_mask(), 7); // reset state
        cpu.set_interrupt_mask(3);
        assert_eq!(cpu.interrupt_mask(), 3);
        assert_eq!(cpu.sr & 0x0700, 0x0300);
        // Other SR bits are untouched
        assert!(cpu.flag_is_set(SrFlag::S));
        cpu.set_interrupt_mask(0);
        assert_eq!(cpu.interrupt_mask(), 0);
    }

    #[test]
    fn supervisor_to_user_swaps_active_sp() {
        let mut cpu = M68000::new(); // starts in supervisor mode
        cpu.a[7] = 0x0000_2000; // active SSP
        cpu.usp = 0x0000_8000; // parked USP

        cpu.set_supervisor(false);

        assert!(!cpu.flag_is_set(SrFlag::S));
        assert_eq!(cpu.a[7], 0x0000_8000, "USP becomes active");
        assert_eq!(cpu.ssp, 0x0000_2000, "SSP parked");
    }

    #[test]
    fn user_to_supervisor_swaps_active_sp() {
        let mut cpu = M68000::new();
        cpu.set_flag(SrFlag::S, false); // force user mode without a swap
        cpu.a[7] = 0x0000_8000; // active USP
        cpu.ssp = 0x0000_2000; // parked SSP

        cpu.set_supervisor(true);

        assert!(cpu.flag_is_set(SrFlag::S));
        assert_eq!(cpu.a[7], 0x0000_2000, "SSP becomes active");
        assert_eq!(cpu.usp, 0x0000_8000, "USP parked");
    }

    #[test]
    fn set_supervisor_is_idempotent() {
        let mut cpu = M68000::new(); // supervisor mode
        cpu.a[7] = 0x0000_2000;
        cpu.usp = 0x0000_8000;
        cpu.ssp = 0x0000_4000;

        cpu.set_supervisor(true); // no mode change: no swap

        assert_eq!(cpu.a[7], 0x0000_2000);
        assert_eq!(cpu.usp, 0x0000_8000);
        assert_eq!(cpu.ssp, 0x0000_4000);
    }

    #[test]
    fn round_trip_mode_switch_preserves_both_sps() {
        let mut cpu = M68000::new();
        cpu.a[7] = 0x0000_2000; // SSP active
        cpu.usp = 0x0000_8000;

        cpu.set_supervisor(false);
        cpu.set_supervisor(true);

        assert_eq!(cpu.a[7], 0x0000_2000);
        assert_eq!(cpu.usp, 0x0000_8000);
    }

    /// Set the CCR from individual flag bits (helper for cc_true tests).
    fn ccr(x: bool, n: bool, z: bool, v: bool, c: bool) -> u16 {
        (x as u16) << 4 | (n as u16) << 3 | (z as u16) << 2 | (v as u16) << 1 | c as u16
    }

    #[test]
    fn cc_true_constants() {
        let mut cpu = M68000::new();
        for sr in [0u16, 0x1F] {
            cpu.sr = sr;
            assert!(cpu.cc_true(0x0), "T is always true");
            assert!(!cpu.cc_true(0x1), "F is always false");
        }
    }

    #[test]
    fn cc_true_carry_and_zero_conditions() {
        let mut cpu = M68000::new();

        cpu.sr = ccr(false, false, false, false, false);
        assert!(cpu.cc_true(0x2), "HI: !C && !Z");
        assert!(!cpu.cc_true(0x3), "LS");
        assert!(cpu.cc_true(0x4), "CC");
        assert!(!cpu.cc_true(0x5), "CS");
        assert!(cpu.cc_true(0x6), "NE");
        assert!(!cpu.cc_true(0x7), "EQ");

        cpu.sr = ccr(false, false, false, false, true);
        assert!(!cpu.cc_true(0x2), "HI fails on C");
        assert!(cpu.cc_true(0x3), "LS: C || Z");
        assert!(!cpu.cc_true(0x4), "CC fails on C");
        assert!(cpu.cc_true(0x5), "CS");

        cpu.sr = ccr(false, false, true, false, false);
        assert!(!cpu.cc_true(0x2), "HI fails on Z");
        assert!(cpu.cc_true(0x3), "LS: C || Z");
        assert!(!cpu.cc_true(0x6), "NE fails on Z");
        assert!(cpu.cc_true(0x7), "EQ");
    }

    #[test]
    fn cc_true_overflow_and_sign_conditions() {
        let mut cpu = M68000::new();

        cpu.sr = ccr(false, false, false, false, false);
        assert!(cpu.cc_true(0x8), "VC");
        assert!(!cpu.cc_true(0x9), "VS");
        assert!(cpu.cc_true(0xA), "PL");
        assert!(!cpu.cc_true(0xB), "MI");

        cpu.sr = ccr(false, true, false, true, false);
        assert!(!cpu.cc_true(0x8), "VC fails on V");
        assert!(cpu.cc_true(0x9), "VS");
        assert!(!cpu.cc_true(0xA), "PL fails on N");
        assert!(cpu.cc_true(0xB), "MI");
    }

    #[test]
    fn cc_true_signed_comparisons() {
        let mut cpu = M68000::new();

        // N == V, Z clear: GE and GT hold, LT and LE fail
        for (n, v) in [(false, false), (true, true)] {
            cpu.sr = ccr(false, n, false, v, false);
            assert!(cpu.cc_true(0xC), "GE for N={n} V={v}");
            assert!(!cpu.cc_true(0xD), "LT for N={n} V={v}");
            assert!(cpu.cc_true(0xE), "GT for N={n} V={v}");
            assert!(!cpu.cc_true(0xF), "LE for N={n} V={v}");
        }

        // N != V: LT and LE hold, GE and GT fail
        for (n, v) in [(true, false), (false, true)] {
            cpu.sr = ccr(false, n, false, v, false);
            assert!(!cpu.cc_true(0xC), "GE for N={n} V={v}");
            assert!(cpu.cc_true(0xD), "LT for N={n} V={v}");
            assert!(!cpu.cc_true(0xE), "GT for N={n} V={v}");
            assert!(cpu.cc_true(0xF), "LE for N={n} V={v}");
        }

        // Z set with N == V: GT fails, LE holds (equality)
        cpu.sr = ccr(false, false, true, false, false);
        assert!(cpu.cc_true(0xC), "GE holds on equal");
        assert!(!cpu.cc_true(0xE), "GT fails on equal");
        assert!(cpu.cc_true(0xF), "LE holds on equal");
    }

    #[test]
    fn x_flag_independent_of_conditions() {
        // No condition code consults X; flipping it must not change any cc.
        let mut cpu = M68000::new();
        for cond in 0..16u8 {
            cpu.sr = ccr(false, true, false, true, true);
            let without_x = cpu.cc_true(cond);
            cpu.set_flag(SrFlag::X, true);
            assert_eq!(cpu.cc_true(cond), without_x, "cond {cond:X} depends on X");
        }
    }
}
