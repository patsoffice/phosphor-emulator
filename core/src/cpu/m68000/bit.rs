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
            self.read_imm_word(bus, master)? as u32
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
            // **TABLE 9-14 ALSO ASTERISKS `BTST`'s DYNAMIC REGISTER CELL,
            // 6(1/0)*, AND THAT IS NOT IMPLEMENTED.** Taken at face value it
            // would say `BTST` on a data register became data-dependent on the
            // 68010 having been fixed on the 68000. Three things say it is a
            // typesetting error instead: the static register cell beside it,
            // 10(2/0), is *not* asterisked in the same table, and the two
            // differ only in where the bit number comes from rather than in
            // what is done to the register; both cells are unasterisked in
            // Table 8-8; and this manual has demonstrable errata in exactly
            // this area, its `RTR` read count changing between the sections
            // with no change to the part. No 68010 oracle exists to settle it.
            // Recorded rather than guessed, in `phosphor-emulator-9zmn`.
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
            let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte)?;
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
            //
            // **BCLR TO MEMORY IS TWO CLOCKS SLOWER ON THE 68010, AND IT IS
            // THE ONLY BYTE ROW IN EITHER TABLE THAT MOVES.** Table 8-8 gives
            // 8(1/1)+ dynamic and 12(2/1)+ static; Table 9-14 gives 10(1/1)+
            // and 14(2/1)+. Same transfer counts, two more clocks, both forms.
            // BCHG, BSET and BTST byte to memory are identical in the two
            // tables, which is what makes this a BCLR rule rather than a
            // memory-destination one, and the register forms of all four are
            // identical too. The difference matches what BCLR already costs
            // over BCHG and BSET on a register, where it has to invert its
            // mask: the newer part appears to pay that in the memory form as
            // well, where the 68000 absorbed it.
            //
            // Taken as the difference between the two sections rather than as
            // Section 9's absolute, per `by_variant`. Missed by M6's delta and
            // filed as `phosphor-emulator-9zmn`.
            let bclr_memory = if op == 2 { self.by_variant(0, 2) } else { 0 };
            // **AN IMMEDIATE OPERAND COSTS TWO CLOCKS MORE, AND HERE THE
            // MANUAL IS THE ONE THAT IS WRONG.** `BTST Dn,#imm` is the only
            // bit operation that can take an immediate, and both corpora
            // record it at 10 clocks with two reads: 138 cases on the
            // documentation-derived set and 58 on the microcode-derived one,
            // every one of them 10 with no spread. Composing the manual's own
            // tables gives 8, from Table 8-8's 4(1/0)+ for a byte `BTST` to
            // memory plus Table 8-1's 4(1/0) for `#<data>`, and 8 is what this
            // core charged.
            //
            // Two independently generated traces agreeing against a composed
            // figure is the case the epic's two-source rule settles against
            // the manual, and the nine other `BTST` memory modes are the
            // control that says the composition is otherwise right: (An) 8,
            // (An)+ 8, -(An) 10, (d16,An) 12, (d8,An,Xn) 14, abs.w 12, abs.l
            // 16, (d16,PC) 12 and (d8,PC,Xn) 14, all exact. Immediate is the
            // single mode where the two disagree.
            //
            // The mechanism is one this core already models elsewhere: an
            // operand out of the prefetch queue runs no data bus cycle, so
            // there is no transfer for the test to happen inside and it needs
            // a step of its own. `src_form_internal` in `alu/binary.rs`
            // charges a long `ADD` four clocks for a register or queue operand
            // against two for one from memory, for the same reason, with `CMP`
            // as the control that made it a mechanism rather than a fitted
            // number. See `phosphor-emulator-cvux`.
            let queue_operand = if ea_mode == 7 && ea_reg == 4 { 2 } else { 0 };
            self.finish_from_bus(
                bus,
                master,
                ea_internal(ea_mode, ea_reg) + bclr_memory + queue_operand,
            );
        }
        Ok(())
    }
}
