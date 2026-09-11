//! Address computation, stack-frame, and register-block instructions:
//! LEA / PEA / MOVEM (line 0x4) and LINK / UNLK (the 0x4E50 group).
//!
//! LEA and PEA materialize a control-mode effective address without
//! touching memory at it — LEA into An, PEA onto the stack. Like JMP/JSR
//! they receive the full 32-bit computed address (the 24-bit mask applies
//! only at the bus). LINK/UNLK build and tear down stack frames. None of
//! these alter the CCR.

use super::M68000;
use super::addressing::{AccessResult, Ea, Size, sext16};
use crate::core::{Bus16, BusMaster};

impl M68000 {
    /// LEA `<ea>`,An (line 0x4, bits 8-6 = 111) and PEA `<ea>` (0x4848-
    /// 0x487B): resolve a control-mode effective address and either load it
    /// into An or push it onto the stack.
    ///
    /// Flags: none.
    pub(crate) fn op_lea_pea<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        push: bool,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Control addressing only — same legality rule as JMP/JSR.
        if !(matches!(ea_mode, 2 | 5 | 6) || (ea_mode == 7 && ea_reg < 4)) {
            self.finish_from_bus(bus, master, 0); // illegal encoding
            return Ok(());
        }
        let Ea::Mem(addr) = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word) else {
            unreachable!("control addressing modes always resolve to memory");
        };
        if push {
            // **PEA refills before it pushes, except from an absolute address.**
            // Read off all seven of the part's control-mode sequences rather
            // than from one of them: `(An)` is a program read and two writes;
            // the displacement and indexed modes, and both PC-relative ones,
            // are two program reads and two writes; and the two absolute modes
            // alone put their last program read *after* the writes, at
            // `(xxx).w` a read, two writes and a read, and at `(xxx).l` two
            // reads, two writes and a read.
            //
            // The other modes have address arithmetic to do and the part slots
            // the prefetch into a step it is already spending on that. An
            // absolute address arrives ready to use in its extension words, so
            // there is no such step before the push and the fetch falls through
            // to the one at the end of the instruction. That is the same
            // mechanism as the jump's leading time, seen from the other side.
            //
            // Where it goes first it is handed to the bus unit rather than
            // driven, so the address arithmetic that precedes it can be placed
            // in front of it: an indexed `PEA` spends two clocks putting the
            // address together *between* its two fetches, exactly as `LEA`
            // does, and a refill the body drives itself leaves the finish
            // nowhere to put them. Where it goes last, the finish owes it
            // anyway and issues it behind the writes with no help from here.
            let absolute = ea_mode == 7 && (ea_reg == 0 || ea_reg == 1);
            if !absolute {
                let signals = self.program_cycle(false);
                self.hand_over(bus, master, super::PendingCycle::Refill { signals });
            }
            self.push_long(bus, master, addr)?;
        } else {
            self.a[((opcode >> 9) & 7) as usize] = addr;
        }
        // LEA computes an address and reads no operand, so its extension words
        // are the whole bus cost, and PEA's long push adds two more transfers.
        // The indexed modes cost four clocks off the bus rather than the two an
        // operand mode pays: LEA has no transfer to hide the index add behind.
        //
        // Both halves of that four run before a fetch rather than after one.
        // The part indexes for two clocks, fetches the extension word, spends
        // two more placing the address in the register, and only then fetches
        // again: `LEA (d8,An,Xn),An` is recorded with its two program reads on
        // clocks two and eight, not two and six.
        let indexed = ea_mode & 7 == 6 || (ea_mode & 7 == 7 && ea_reg & 7 == 3);
        self.finish_from_bus_address_first(bus, master, if indexed { 4 } else { 0 });
        Ok(())
    }

    /// LINK An,#disp (0x4E50): push An, point An at the new frame (the
    /// updated stack pointer), then advance SP by the sign-extended
    /// displacement (normally negative, reserving locals).
    ///
    /// `LINK A7` pushes the *decremented* A7: the register being pushed is also
    /// the stack pointer doing the pushing, and the decrement lands first.
    ///
    /// **THE SOURCES DISAGREE ABOUT THIS ONE ENCODING AND THE DISAGREEMENT IS
    /// UNRESOLVED.** It is two against two, so nothing here is fitted to either
    /// pair, and the value below is the one the regression net demands:
    ///
    /// - The documentation-derived corpus pushes `A7 - 4`, on 1005 vectors, and
    ///   they are part of the state gate. So does the manual's own description,
    ///   which sequences the instruction as `SP - 4 -> SP` and *then*
    ///   `An -> (SP)`, making the two the same register at the moment of the
    ///   push.
    /// - The microcode-derived corpus pushes `A7`, on 326 cases. So does the
    ///   part's own sequence, which latches An one step before it computes the
    ///   destination address, so the value driven is the one the instruction
    ///   started with.
    ///
    /// Changing it to `A7` was tried and fails 1005 state vectors, which is why
    /// it stands. The per-cycle gate reports the 326 cases against it as a
    /// rung-4 data residual rather than hiding them, and this is the note that
    /// stops the residual being read as a defect and quietly fitted.
    ///
    /// Flags: none. 16 cycles.
    pub(crate) fn op_link<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        let disp = sext16(self.read_imm_word(bus, master));
        // See the doc comment: A7 is the one register whose pushed value the
        // sources do not agree on, and this follows the state gate.
        let value = if reg == 7 {
            self.a[7].wrapping_sub(4)
        } else {
            self.a[reg]
        };
        self.push_long(bus, master, value)?;
        self.a[reg] = self.a[7];
        self.a[7] = self.a[7].wrapping_add(disp);
        // The opcode, the displacement word and the long push are the whole
        // cost; the register shuffling happens inside them.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// UNLK An (0x4E58): collapse the frame — SP = An, then pop the saved
    /// value back into An.
    ///
    /// UNLK A7 ends with A7 holding the popped value (the post-pop
    /// increment is overwritten by the load).
    ///
    /// Flags: none. 12 cycles.
    pub(crate) fn op_unlk<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        // The frame address is read *before* A7 is moved to it, so an odd An
        // faults with the supervisor stack still where it was.
        //
        // Setting A7 first is what this did until the microcode-derived corpus
        // caught it: 530 of its UNLK cases fault on an odd An, and every one of
        // them records the group-0 frame written on the stack the instruction
        // started with. Pushing it on the odd An instead made the frame push
        // fault too, so this core halted on a double bus fault where the part
        // takes an ordinary address error. The 680x0 corpus has no such case,
        // which is why it read 100% on this instruction throughout.
        let frame = self.a[reg];
        let value = self.read_long_at(bus, master, frame)?;
        self.a[7] = frame.wrapping_add(4);
        self.a[reg] = value;
        // The long pop and the refill behind the opcode, and nothing besides.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// Register file indexed the MOVEM way: 0-7 = D0-D7, 8-15 = A0-A7.
    #[inline]
    fn movem_reg(&self, r: usize) -> u32 {
        if r < 8 { self.d[r] } else { self.a[r - 8] }
    }

    #[inline]
    fn set_movem_reg(&mut self, r: usize, value: u32) {
        if r < 8 {
            self.d[r] = value;
        } else {
            self.a[r - 8] = value;
        }
    }

    /// MOVEM `<list>,<ea>` (0x4880) / MOVEM `<ea>,<list>` (0x4C80): move
    /// multiple registers to or from memory. The register-list mask word
    /// follows the opcode, ahead of any EA extension words. Word-size loads
    /// sign-extend into the full register — address and data registers
    /// alike.
    ///
    /// The mask is bit 0 = D0 … bit 15 = A7, except the predecrement store
    /// form, which reverses it (bit 0 = A7 … bit 15 = D0) and stores
    /// descending so the block ends up in ascending register order.
    /// 68000-specific corner cases: storing the predecrement base register
    /// writes its *initial* value (the 68010+ write the decremented one),
    /// and a postincrement load that includes the base register leaves it
    /// at the final incremented address (the fetched value is discarded).
    ///
    /// Flags: none.
    pub(crate) fn op_movem<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        to_registers: bool,
    ) -> AccessResult<()> {
        let size = if opcode & 0x0040 != 0 {
            Size::Long
        } else {
            Size::Word
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Loads take control modes plus (An)+; stores take control-alterable
        // modes plus -(An).
        let valid = if to_registers {
            matches!(ea_mode, 2 | 3 | 5 | 6) || (ea_mode == 7 && ea_reg < 4)
        } else {
            matches!(ea_mode, 2 | 4 | 5 | 6) || (ea_mode == 7 && ea_reg < 2)
        };
        if !valid {
            self.finish_from_bus(bus, master, 0); // illegal encoding
            return Ok(());
        }
        let mask = self.read_imm_word(bus, master);
        // The register mask precedes the mode's extension words, so an indexed
        // MOVEM adds its index after that fetch rather than in front of the
        // instruction, and the loader has not burned it. A predecrement store
        // is the exception both ways: the part folds its first decrement into
        // the write it is already committed to, which is why the internal time
        // declared at the finish counts only an index add.
        self.spend_internal(if ea_mode == 6 || (ea_mode == 7 && ea_reg == 3) {
            2
        } else {
            0
        });

        if ea_mode == 4 {
            // Predecrement store: reversed mask, descending addresses. The
            // base register updates only after the whole list succeeds —
            // a faulting store leaves An at its initial value
            // (hardware-verified).
            let reg = ea_reg as usize;
            let initial = self.a[reg];
            let mut addr = initial;
            for i in 0..16 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                let r = 15 - i; // bit 0 = A7 … bit 15 = D0
                addr = addr.wrapping_sub(size.bytes());
                let value = if r == 8 + reg {
                    initial
                } else {
                    self.movem_reg(r)
                };
                match size {
                    Size::Word => self.write_word_at(bus, master, addr, value as u16)?,
                    _ => {
                        // Descending long stores write the low word first
                        // (a fault reports addr + 2) — hardware-verified.
                        self.write_word_at(bus, master, addr.wrapping_add(2), value as u16)?;
                        self.write_word_at(bus, master, addr, (value >> 16) as u16)?;
                    }
                }
            }
            self.a[reg] = addr;
        } else {
            let mut addr = if ea_mode == 3 {
                self.a[ea_reg as usize]
            } else {
                let Ea::Mem(base) = self.decode_ea(bus, master, ea_mode, ea_reg, size) else {
                    unreachable!("MOVEM EA modes always resolve to memory");
                };
                base
            };
            for r in 0..16 {
                if mask & (1 << r) == 0 {
                    continue;
                }
                if to_registers {
                    if ea_mode == 3 {
                        // The base register tracks one word step even
                        // through a faulting transfer (hardware-verified:
                        // an aborted first read leaves An at +2); the
                        // post-loop assignment sets the final address on
                        // success.
                        self.a[ea_reg as usize] = addr.wrapping_add(2);
                    }
                    let value = match size {
                        Size::Word => sext16(self.read_word_at(bus, master, addr)?),
                        _ => self.read_long_at(bus, master, addr)?,
                    };
                    self.set_movem_reg(r, value);
                } else {
                    let value = self.movem_reg(r);
                    match size {
                        Size::Word => self.write_word_at(bus, master, addr, value as u16)?,
                        _ => self.write_long_at(bus, master, addr, value)?,
                    }
                }
                addr = addr.wrapping_add(size.bytes());
            }
            if ea_mode == 3 {
                // Postincrement: the base ends at the final address, even
                // when it was itself in the load list
                self.a[ea_reg as usize] = addr;
            }
        }

        // The opcode, the register mask word, the mode's extension words and
        // one transfer per register half are all counted, which is everything
        // the documented per-register cost used to express.
        //
        // Two things are left. An indexed mode pays two clocks for its index
        // add, as everywhere else. And a load pays four more, because the part
        // reads one operand *beyond* the registers it transfers and discards
        // it. This core does not run that read, so the four clocks are declared
        // here rather than counted: the total is right and the transfer count
        // is one short on every MOVEM load, which is a real difference from the
        // part and shows up on the gate's count rung rather than its length one.
        let indexed = ea_mode & 7 == 6 || (ea_mode & 7 == 7 && ea_reg & 7 == 3);
        let internal = if indexed { 2 } else { 0 } + if to_registers { 4 } else { 0 };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }
}
