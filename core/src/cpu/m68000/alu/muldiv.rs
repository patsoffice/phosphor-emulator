//! MULU / MULS (line 0xC, opmodes 011/111), DIVU / DIVS (line 0x8,
//! opmodes 011/111), and CHK (line 0x4, bits 8-6 = 110).
//!
//! All five take a word source operand (data addressing only) and a data
//! register. Division by zero enters the vector-5 exception, an
//! out-of-bounds CHK enters vector 6.

use super::super::M68000;
use super::super::addressing::{AccessResult, Size, ea_internal, sext16};
use super::super::flags::SrFlag;
use crate::core::{Bus16, BusMaster};

impl M68000 {
    /// Read the word source operand shared by MULx/DIVx/CHK. Returns `None`
    /// for the illegal An source mode.
    fn muldiv_operand<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<Option<(u16, u32)>> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 {
            return Ok(None);
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, ea, Size::Word)? as u16;
        // The extension words and the operand read are counted transfers; only
        // the mode's own address arithmetic is left for the caller to declare.
        Ok(Some((value, ea_internal(ea_mode, ea_reg))))
    }

    /// MULU.w / MULS.w <ea>,Dn — 16 × 16 → 32-bit product into the full Dn.
    ///
    /// Flags: N/Z from the 32-bit product, V/C cleared (a 16×16 multiply
    /// cannot overflow 32 bits), **X untouched**.
    pub(crate) fn op_mul<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        signed: bool,
    ) -> AccessResult<()> {
        let Some((src, ea_time)) = self.muldiv_operand(opcode, bus, master)? else {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
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

        // The multiply runs entirely off the bus. Documented worst case is
        // 38 + 2n internal cycles and this charges the worst case flat; the
        // data-dependent refinement is its own piece of work, and it is a
        // change to this number alone now that nothing else is folded into it.
        self.finish_from_bus(bus, master, 66 + ea_time);
        Ok(())
    }

    /// DIVU.w / DIVS.w <ea>,Dn — 32 ÷ 16 → 16-bit quotient in the low word
    /// of Dn, 16-bit remainder in the high word.
    ///
    /// Flags: N/Z from the quotient, V/C cleared. On overflow (quotient too
    /// large for 16 bits) V is set and Dn and the other flags are left
    /// unchanged. **X untouched** in every case. Division by zero leaves Dn
    /// and the flags alone and takes the vector-5 exception.
    pub(crate) fn op_div<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        signed: bool,
    ) -> AccessResult<()> {
        let Some((src, ea_time)) = self.muldiv_operand(opcode, bus, master)? else {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        };
        let dn = ((opcode >> 9) & 7) as usize;
        let dst = self.d[dn];

        if src == 0 {
            // Division by zero: N/Z/V/C are cleared (X kept) and the frame
            // PC is the *divide instruction itself*, not the next one —
            // both pinned by the suite's lone zero-divide vector
            // (80ef [DIVU (d16, A7), D0]); Dn is untouched.
            self.set_flag(SrFlag::N, false);
            self.set_flag(SrFlag::Z, false);
            self.set_flag(SrFlag::V, false);
            self.set_flag(SrFlag::C, false);
            // Exception entry pushes the frame and fetches the vector, five
            // transfers on the 68000 and six on the 68010, all counted. So the
            // longer frame now costs its four clocks by itself rather than
            // needing a variant-gated constant here.
            self.exception(bus, master, 5, self.instr_pc, 4)?;
            self.finish_from_bus(bus, master, 18 + ea_time);
            return Ok(());
        }

        if signed {
            let divisor = src as i16 as i32;
            // The one quotient that overflows the i32 division itself.
            if dst == 0x8000_0000 && divisor == -1 {
                self.d[dn] = 0;
                self.set_flags_logical(Size::Long, 0);
                self.finish_from_bus(bus, master, 154 + ea_time);
                return Ok(());
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
            self.finish_from_bus(bus, master, 154 + ea_time);
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
            self.finish_from_bus(bus, master, 136 + ea_time);
        }
        Ok(())
    }

    /// CHK.w <ea>,Dn — vector-6 exception if the signed word in Dn is
    /// negative or greater than the bound read from <ea>.
    ///
    /// Flags: Z from the checked word, V/C cleared (undefined on hardware,
    /// matching its observed behavior); N is set/cleared only on the trap
    /// paths (negative/too-large) and otherwise keeps its old value.
    /// **X untouched**.
    pub(crate) fn op_chk<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let Some((bound, ea_time)) = self.muldiv_operand(opcode, bus, master)? else {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        };
        // **CHK does not refill behind its opcode before it traps**, and the
        // two corpora disagree about that. The documentation-derived one
        // records `CHK D0, D4` trapping as a program read, three frame writes,
        // the vector and two refills at the handler, eight transfers; the
        // microcode-derived one records seven, with no read in front. The part
        // settles it: the compare runs, and if it traps the sequence goes
        // straight from the test to the frame with no fetch between. Refilling
        // there would fetch a word the trap is about to discard, which is the
        // same reason a taken branch does not.
        //
        let dn = ((opcode >> 9) & 7) as usize;
        let src = sext16(self.d[dn] as u16) as i32;
        let bound = sext16(bound) as i32;

        self.set_flag(SrFlag::Z, src as u16 == 0);
        self.set_flag(SrFlag::V, false);
        self.set_flag(SrFlag::C, false);
        // **The part decides this in two steps and the second costs two
        // clocks**, so what a `CHK` costs depends on which step caught it.
        //
        // The first step subtracts the value from the bound and traps on the
        // result's sign *or its overflow*. That is not the same as "the value
        // is above the bound": the two part company on exactly the operand
        // pairs whose 16-bit subtract overflows, and those cases are what was
        // left over when this was first written as a comparison. Only if that
        // step does not trap does a second one, two clocks later, look at the
        // value's own sign. In bounds reaches the second step too, which is why
        // it is six either way.
        //
        // Invisible to every rung but the first, because all three paths run
        // the same bus cycles; worth four clocks on a third of `CHK`'s cases
        // and two more on a twelfth of them.
        let (value, limit) = (src as u16, bound as u16);
        let diff = limit.wrapping_sub(value);
        let negative = diff & 0x8000 != 0;
        let overflowed = ((limit & !value & !diff) | (!limit & value & diff)) & 0x8000 != 0;
        let compare = if negative || overflowed { 4 } else { 6 };
        self.spend_idle(bus, master, compare);

        if src < 0 || src > bound {
            self.set_flag(SrFlag::N, src < 0);
            // Seven transfers for a register source: three frame words, the
            // two-word vector and the two refills at the handler, with no fetch
            // in front. What is left is the comparison above, four of the
            // entry's own before the frame, and two between the handler's two
            // fetches.
            self.exception(bus, master, 6, self.pc, 4)?;
            self.finish_from_bus(bus, master, compare + 6 + ea_time);
        } else {
            // In bounds: both steps, then the fetch behind the opcode. They run
            // first, which is why this finishes ahead of its refill rather than
            // behind it.
            self.finish_from_bus_address_first(bus, master, compare + ea_time);
        }
        Ok(())
    }
}
