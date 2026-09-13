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
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word)?;
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

        // **The multiply runs off the bus and its length depends on the source
        // bits, one microcode step at a time.** The part walks the source from
        // the bottom, spending two clocks on each of the sixteen bits, and two
        // more wherever it has something to add. What "something to add" means
        // is the whole difference between the two forms:
        //
        // - `MULU` looks at one bit and adds where it is set, so it pays for
        //   every one in the source.
        // - `MULS` looks at the bit *and the one below it*, adds on `01`,
        //   subtracts on `10`, and does neither on `00` or `11`. So it pays for
        //   every place the source changes value, counting an implicit zero
        //   below bit 0, which is what `src ^ (src << 1)` counts.
        //
        // Four clocks of the loop's fixed part are the fetch behind the opcode
        // and are counted as a transfer, leaving thirty-four here. Charging the
        // worst case flat, as this did, made `MULU` and `MULS` right on the one
        // source in 65,536 that has every bit set.
        let steps = if signed {
            (src ^ (src << 1)).count_ones()
        } else {
            src.count_ones()
        };
        self.finish_from_bus(bus, master, 34 + 2 * steps + ea_time);
        Ok(())
    }

    /// Clocks `DIVU` spends off the bus, walked one microcode step at a time.
    ///
    /// **This is a restoring division and its length is the loop, not a
    /// table.** The part shifts the remainder up by one, tries the divisor
    /// against it, and keeps or restores the result:
    ///
    /// - Where the remainder's top bit was already set, the shift carries it
    ///   past sixteen bits and the divisor must fit, so the part does not test:
    ///   **four clocks**.
    /// - Where it was not and the trial subtract succeeds: **six**.
    /// - Where it was not and the subtract borrows, the old remainder has to be
    ///   put back, which is a step of its own: **eight**.
    ///
    /// The last of the sixteen passes always costs six, having no next pass to
    /// choose a shift for. Six clocks of setup precede all of it, and the
    /// overflow exit is taken from that setup before the loop runs at all,
    /// which is why an overflowing `DIVU` is ten clocks and one transfer.
    ///
    /// **The worst case is the check that this is the right shape.** Fifteen
    /// passes that all restore, plus six for the last and six of setup, is 132,
    /// and 132 plus the one transfer is the 136 this core charged flat. A model
    /// off by a step anywhere would not land on that number.
    fn divu_internal(dividend: u32, divisor: u16) -> u32 {
        // dvur1, dvum2, dvum3: latch the operands, try the divisor against the
        // high half, and take the overflow exit if it fits.
        let mut clocks = 6;
        let mut rem = dividend >> 16;
        if rem >= u32::from(divisor) {
            return clocks;
        }
        let divisor = u32::from(divisor);
        let mut low = dividend as u16;
        for pass in 0..16 {
            let top_was_set = rem & 0x8000 != 0;
            rem = (rem << 1) | u32::from(low >> 15);
            low <<= 1;
            if pass == 15 {
                // The last pass places the remainder instead of choosing a
                // shift, and costs the same either way.
                clocks += 6;
                if top_was_set || rem >= divisor {
                    rem -= divisor;
                }
            } else if top_was_set {
                clocks += 4;
                rem -= divisor;
            } else if rem >= divisor {
                clocks += 6;
                rem -= divisor;
            } else {
                clocks += 8;
            }
        }
        clocks
    }

    /// Clocks `DIVS` spends off the bus, walked the same way.
    ///
    /// **The same restoring loop on the absolute values, with sign work either
    /// side of it.** The part takes the divisor's sign first and negates it if
    /// it has to, then the dividend's, which costs a step more because the
    /// negation is two halves with a borrow between them. Then the loop, which
    /// differs from `DIVU`'s only in shifting before it subtracts rather than
    /// after, so it has no case where the top bit already set makes the
    /// subtract certain: **six clocks where the subtract succeeds and eight
    /// where it has to be put back**, with the last of the sixteen always six.
    ///
    /// The tail puts the signs back on the quotient and the remainder, and what
    /// it costs depends on which signs there were:
    ///
    /// ```text
    ///   divisor +, dividend +   six     divisor -, dividend +   eight
    ///   divisor +, dividend -   ten     divisor -, dividend -   eight
    /// ```
    ///
    /// An overflow found *before* the loop skips all of it, which is the whole
    /// difference between an overflowing `DIVS` and a working one. An overflow
    /// found after it costs the same as a result, because the step that
    /// notices drives the same fetch either way.
    fn divs_internal(dividend: u32, divisor: u16) -> u32 {
        let dividend_negative = dividend & 0x8000_0000 != 0;
        let divisor_negative = divisor & 0x8000 != 0;
        // Two steps to take the divisor's sign, one to take the dividend's, and
        // one more to finish the dividend's negation, which is a long.
        let mut clocks = 10 + if dividend_negative { 4 } else { 2 };

        let magnitude = if dividend_negative {
            (dividend as i32).wrapping_neg() as u32
        } else {
            dividend
        };
        let divisor = u32::from(if divisor_negative {
            (divisor as i16).wrapping_neg() as u16
        } else {
            divisor
        });

        // The same trial against the high half, and the same early exit.
        let mut rem = magnitude >> 16;
        if rem >= divisor {
            return clocks;
        }
        clocks += 2;
        let mut low = magnitude as u16;
        for pass in 0..16 {
            rem = (rem << 1) | u32::from(low >> 15);
            low <<= 1;
            let fits = rem >= divisor;
            if fits {
                rem -= divisor;
            }
            clocks += if pass == 15 || fits { 6 } else { 8 };
        }

        clocks
            + 4
            + match (divisor_negative, dividend_negative) {
                (false, false) => 2,
                (false, true) => 6,
                (true, _) => 4,
            }
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
            // Four clocks of the divide's own before it can tell: it latches
            // the operands and tries the divisor against the high half, and the
            // zero shows up as that trial's result. Then the entry's own four
            // in front of the frame and two between the handler's fetches,
            // which is ten and not the eighteen this charged.
            self.spend_idle(bus, master, 4);
            self.exception(bus, master, 5, self.instr_pc, 4)?;
            self.finish_from_bus(bus, master, 10 + ea_time);
            return Ok(());
        }

        if signed {
            let divisor = src as i16 as i32;
            // The one quotient that overflows the i32 division itself.
            if dst == 0x8000_0000 && divisor == -1 {
                self.d[dn] = 0;
                self.set_flags_logical(Size::Long, 0);
                self.finish_from_bus_address_first(
                    bus,
                    master,
                    Self::divs_internal(dst, src) + ea_time,
                );
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
            self.finish_from_bus_address_first(
                bus,
                master,
                Self::divs_internal(dst, src) + ea_time,
            );
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
            // **A divide fetches after its loop, where a multiply fetches
            // before.** The part's multiply issues the refill behind the opcode
            // in the step that starts the loop; its divide has no such step and
            // issues it in the one that ends, so all of the loop's time runs in
            // front of that fetch.
            self.finish_from_bus_address_first(
                bus,
                master,
                Self::divu_internal(dst, src) + ea_time,
            );
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
            //
            // In bounds always reaches the second step, so the six below is the
            // `compare` this path computed; stated as a literal because it is
            // the manual's row rather than this core's arithmetic. **The 68010
            // spends two clocks fewer**: Table 9-18 gives CHK with no trap as
            // 8(1/0)+ against Table 8-12's 10(1/0)+. The trapping path is not
            // touched here: its cost is an exception-table row, and those are
            // carried as an open question.
            debug_assert_eq!(compare, 6, "in bounds reaches both decision steps");
            self.finish_from_bus_address_first(bus, master, self.by_variant(6, 4) + ea_time);
        }
        Ok(())
    }
}
