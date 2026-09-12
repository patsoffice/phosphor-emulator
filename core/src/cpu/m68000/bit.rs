//! Bit operations: BTST / BCHG / BCLR / BSET (line 0x0).
//!
//! Each operation has two encodings: dynamic (`0000 rrr1 oo eeeeee`, bit
//! number in Dr) and static (`0000 1000 oo eeeeee`, bit number in an
//! extension word fetched ahead of the destination EA). A data-register
//! destination is a 32-bit operation with the bit number modulo 32; a
//! memory destination is a byte operation modulo 8.
//!
//! Flags: Z = the addressed bit was zero *before* any modification;
//! N/V/C/X are never touched.

use super::M68000;
use super::addressing::{AccessResult, Size, ea_internal};
use super::flags::SrFlag;
use crate::core::{Bus16, BusMaster};

impl M68000 {
    /// BTST / BCHG / BCLR / BSET `<bit>,<ea>` — `oo` bits 7-6 select the
    /// operation (00/01/10/11). BTST accepts any data source, including
    /// PC-relative and (dynamic form only) immediate; the three modifying
    /// ops need a data-alterable destination.
    pub(crate) fn op_bitop<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        dynamic: bool,
    ) -> AccessResult<()> {
        let op = (opcode >> 6) & 3;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let is_btst = op == 0;

        // Mode-7 submodes allowed as destination: abs.w/abs.l always;
        // d16(PC)/d8(PC,Xn) only for BTST; #imm only for dynamic BTST.
        let reg7_limit = match (is_btst, dynamic) {
            (false, _) => 2,
            (true, false) => 4,
            (true, true) => 5,
        };
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= reg7_limit) {
            self.finish_from_bus(bus, master, 0); // illegal encoding
            return Ok(());
        }

        // The static bit number is an extension word ahead of the EA words.
        let bit_number = if dynamic {
            self.d[((opcode >> 9) & 7) as usize]
        } else {
            self.read_imm_word(bus, master) as u32
        };
        // The static form's extension word used to be charged here as a
        // constant; it is a transfer now and counts itself.

        if ea_mode == 0 {
            // Dn destination: long operation, bit number mod 32
            let mask = 1u32 << (bit_number & 31);
            let reg = ea_reg as usize;
            let old = self.d[reg];
            self.set_flag(SrFlag::Z, old & mask == 0);
            self.d[reg] = match op {
                1 => old ^ mask,  // BCHG
                2 => old & !mask, // BCLR
                3 => old | mask,  // BSET
                _ => old,         // BTST
            };
            // A register destination makes no operand transfer: the opcode
            // fetch, plus the static form's extension word, are the whole bus
            // cost. What is left is the bit operation itself, and BCLR takes
            // four clocks longer than BTST because it has to invert its mask.
            //
            // **A BIT IN THE LOWER WORD COSTS TWO CLOCKS LESS, AND THE MANUAL
            // SAYS SO RATHER THAN THIS BEING FITTED TO A RESIDUAL.** Table 8-8
            // marks every register cell of `BCHG`, `BCLR` and `BSET` with an
            // asterisk, and its footnote reads "Indicates maximum value". So
            // the documented 8, 10 and 8 are the cost when the addressed bit
            // is in the upper word, and the part is quicker when it is not.
            // `BTST`'s register cells carry no asterisk and are fixed, which
            // is why it is exact either way and takes no `upper` term here.
            //
            // Both corpora agree on the predicate, at 100.00% of 3,873
            // modifying-op cases with no counterexample, and the split is
            // about even because a random bit number lands in either half.
            // Table 9-14 asterisks the same three instructions, so this is
            // the 68010's behavior too and is deliberately not variant-gated.
            // See `phosphor-emulator-4sdm`.
            //
            // The cost with the bit in the lower word, which is the documented
            // figure less the asterisked two clocks.
            let base = match op {
                0 => 2, // BTST
                2 => 4, // BCLR
                _ => 2, // BCHG / BSET
            };
            // BTST is not asterisked and pays nothing for the upper word.
            let upper_word = op != 0 && (bit_number & 31) >= 16;
            self.finish_from_bus(bus, master, base + if upper_word { 2 } else { 0 });
        } else {
            // Memory (or immediate, for dynamic BTST): byte operation mod 8
            let mask = 1u32 << (bit_number & 7);
            // The static form fetches its bit number before it resolves an
            // address, so the mode's arithmetic runs here rather than in front
            // of the instruction, and the loader has not burned it. The dynamic
            // form takes its bit number out of a register and resolves first,
            // so the loader has. Either way the clocks are spent before the
            // operand read and an aborted one has still spent them.
            if !dynamic {
                self.spend_internal(ea_internal(ea_mode, ea_reg));
            }
            let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
            let old = self.ea_read(bus, master, ea, Size::Byte)?;
            self.set_flag(SrFlag::Z, old & mask == 0);
            if !is_btst {
                let new = match op {
                    1 => old ^ mask,
                    2 => old & !mask,
                    _ => old | mask,
                };
                self.ea_write_rmw(bus, master, ea, Size::Byte, new)?;
            }

            // In memory the operation is a byte read, and a write for
            // everything but BTST. Both are counted, as is the static form's
            // extension word, leaving only the mode's address arithmetic.
            self.finish_from_bus(bus, master, ea_internal(ea_mode, ea_reg));
        }
        Ok(())
    }
}
