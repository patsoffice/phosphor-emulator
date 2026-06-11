//! MULU / MULS (line 0xC, opmodes 011/111), DIVU / DIVS (line 0x8,
//! opmodes 011/111), and CHK (line 0x4, bits 8-6 = 110).
//!
//! All five take a word source operand (data addressing only) and a data
//! register. Divide-by-zero (vector 5) and the CHK trap (vector 6) cannot
//! be entered until exception support lands in M5; until then they set
//! `trap_pending` and leave the destination register unchanged, and the
//! validation harness skips affected vectors.

use super::super::M68000;
use super::super::addressing::{Size, ea_cycles, sext16};
use super::super::flags::SrFlag;
use crate::core::{Bus, BusMaster};

impl M68000 {
    /// Read the word source operand shared by MULx/DIVx/CHK. Returns `None`
    /// for the illegal An source mode.
    fn muldiv_operand<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> Option<(u16, u32)> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 {
            return None;
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, ea, Size::Word) as u16;
        Some((value, ea_cycles(ea_mode, ea_reg, Size::Word)))
    }

    /// MULU.w / MULS.w <ea>,Dn — 16 × 16 → 32-bit product into the full Dn.
    ///
    /// Flags: N/Z from the 32-bit product, V/C cleared (a 16×16 multiply
    /// cannot overflow 32 bits), **X untouched**.
    pub(crate) fn op_mul<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        signed: bool,
    ) {
        let Some((src, ea_time)) = self.muldiv_operand(opcode, bus, master) else {
            self.finish(4);
            return;
        };
        let dn = ((opcode >> 9) & 7) as usize;
        let dst = self.d[dn] as u16;

        let product = if signed {
            (src as i16 as i32).wrapping_mul(dst as i16 as i32) as u32
        } else {
            (src as u32) * (dst as u32)
        };
        self.d[dn] = product;
        self.set_flags_logical(Size::Long, product);

        // Documented worst case is 38 + 2n internal cycles; the data-dependent
        // refinement can land with cycle-exact timing work.
        self.finish(70 + ea_time);
    }

    /// DIVU.w / DIVS.w <ea>,Dn — 32 ÷ 16 → 16-bit quotient in the low word
    /// of Dn, 16-bit remainder in the high word.
    ///
    /// Flags: N/Z from the quotient, V/C cleared. On overflow (quotient too
    /// large for 16 bits) V is set and Dn and the other flags are left
    /// unchanged. **X untouched** in every case. Division by zero leaves Dn
    /// and the flags alone and raises the vector-5 trap (M5; flagged via
    /// `trap_pending` until then).
    pub(crate) fn op_div<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        signed: bool,
    ) {
        let Some((src, ea_time)) = self.muldiv_operand(opcode, bus, master) else {
            self.finish(4);
            return;
        };
        let dn = ((opcode >> 9) & 7) as usize;
        let dst = self.d[dn];

        if src == 0 {
            self.trap_pending = true;
            self.finish(8 + ea_time);
            return;
        }

        if signed {
            let divisor = src as i16 as i32;
            // The one quotient that overflows the i32 division itself.
            if dst == 0x8000_0000 && divisor == -1 {
                self.d[dn] = 0;
                self.set_flags_logical(Size::Long, 0);
                self.finish(158 + ea_time);
                return;
            }
            let quotient = (dst as i32) / divisor;
            let remainder = (dst as i32) % divisor;
            if quotient == quotient as i16 as i32 {
                self.d[dn] = (quotient as u32 & 0xFFFF) | ((remainder as u32) << 16);
                self.set_flag(SrFlag::N, (quotient as i16) < 0);
                self.set_flag(SrFlag::Z, quotient == 0);
                self.set_flag(SrFlag::V, false);
                self.set_flag(SrFlag::C, false);
            } else {
                // Overflow: V set, C cleared, N/Z/Dn unchanged (observed
                // hardware behavior, verified against the test vectors).
                self.set_flag(SrFlag::V, true);
                self.set_flag(SrFlag::C, false);
            }
            self.finish(158 + ea_time);
        } else {
            let quotient = dst / src as u32;
            let remainder = dst % src as u32;
            if quotient < 0x10000 {
                self.d[dn] = (quotient & 0xFFFF) | (remainder << 16);
                self.set_flag(SrFlag::N, quotient & 0x8000 != 0);
                self.set_flag(SrFlag::Z, quotient == 0);
                self.set_flag(SrFlag::V, false);
                self.set_flag(SrFlag::C, false);
            } else {
                // Overflow: V set, C cleared, N/Z/Dn unchanged (observed
                // hardware behavior, verified against the test vectors).
                self.set_flag(SrFlag::V, true);
                self.set_flag(SrFlag::C, false);
            }
            self.finish(140 + ea_time);
        }
    }

    /// CHK.w <ea>,Dn — trap (vector 6, M5) if the signed word in Dn is
    /// negative or greater than the bound read from <ea>.
    ///
    /// Flags: Z from the checked word, V/C cleared (undefined on hardware,
    /// matching its observed behavior); N is set/cleared only on the trap
    /// paths (negative/too-large) and otherwise keeps its old value.
    /// **X untouched**.
    pub(crate) fn op_chk<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let Some((bound, ea_time)) = self.muldiv_operand(opcode, bus, master) else {
            self.finish(4);
            return;
        };
        let dn = ((opcode >> 9) & 7) as usize;
        let src = sext16(self.d[dn] as u16) as i32;
        let bound = sext16(bound) as i32;

        self.set_flag(SrFlag::Z, src as u16 == 0);
        self.set_flag(SrFlag::V, false);
        self.set_flag(SrFlag::C, false);
        if src < 0 || src > bound {
            self.set_flag(SrFlag::N, src < 0);
            self.trap_pending = true;
        }
        self.finish(10 + ea_time);
    }
}
