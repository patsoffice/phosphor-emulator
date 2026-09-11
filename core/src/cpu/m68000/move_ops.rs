//! MOVE / MOVEA / MOVEQ / MOVEP / SWAP / EXG and the SR/CCR/USP moves —
//! data movement instructions.

use super::M68000;
use super::addressing::{AccessResult, Ea, Size, ea_internal, sext8, sext16};
use crate::core::{Bus16, BusMaster};

impl M68000 {
    /// MOVE <ea>,<ea> and MOVEA <ea>,An — lines 0x1 (byte), 0x2 (long),
    /// 0x3 (word). Opcode layout: `size:2 | dst_reg:3 | dst_mode:3 |
    /// src_mode:3 | src_reg:3`; a destination mode of An selects MOVEA.
    ///
    /// Flags (MOVE): N and Z from the moved value, V and C cleared,
    /// X untouched (data movement never alters X). MOVEA sets no flags and
    /// sign-extends a word source to the full address register.
    pub(crate) fn op_move<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let size = match (opcode >> 12) & 3 {
            1 => Size::Byte,
            3 => Size::Word,
            _ => Size::Long, // line 0x2
        };
        let src_mode = ((opcode >> 3) & 7) as u8;
        let src_reg = (opcode & 7) as u8;
        let dst_mode = ((opcode >> 6) & 7) as u8;
        let dst_reg = ((opcode >> 9) & 7) as u8;

        // Byte access to an address register is an illegal encoding
        // (MOVE.b An,<ea> / MOVEA.b); treated as a bounded NOP.
        if size == Size::Byte && (src_mode == 1 || dst_mode == 1) {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        }

        let src = self.decode_ea(bus, master, src_mode, src_reg, size);
        let value = self.ea_read(bus, master, src, size)?;

        // MOVE sets the flags from the value before the destination write
        // (visible in the SR an aborted destination write stacks). MOVEA
        // (An destination) sets none, and reuses the An write path:
        // ea_write sign-extends word writes to address registers, which is
        // exactly the MOVEA.w rule.
        if dst_mode != 1 {
            self.set_flags_logical(size, value);
        }
        match (dst_mode, size) {
            // Postincrement destination: the increment commits only after
            // a successful write — an aborted MOVE leaves An unchanged
            // (hardware-verified, unlike postincrement source reads).
            (3, _) => {
                let reg = dst_reg as usize;
                let addr = self.a[reg];
                self.ea_write(bus, master, Ea::Mem(addr), size, value)?;
                self.a[reg] = addr.wrapping_add(self.step_for(reg, size));
            }
            // MOVE.l to -(An) writes the low word first with An stepping
            // by 2 at a time, so a fault leaves An decremented by only 2
            // (hardware-verified; same pattern as the ADDX/SUBX operands).
            // Predecrement-destination faults also stack the current PC,
            // one word later than other operand faults.
            // A predecrement destination is the one MOVE that refills *before*
            // its write rather than after it: `MOVE.w (A1)+, -(A1)` is recorded
            // as a read, a program read and a write, where the same instruction
            // to `(A4)` is a read, a write and a program read. The decrement
            // gives the prefetch a slot the other destination modes do not.
            (4, Size::Long) => {
                let reg = dst_reg as usize;
                self.refill_prefetch(bus, master);
                let lo_first = (|| {
                    self.a[reg] = self.a[reg].wrapping_sub(2);
                    self.write_word_at(bus, master, self.a[reg], value as u16)?;
                    self.a[reg] = self.a[reg].wrapping_sub(2);
                    self.write_word_at(bus, master, self.a[reg], (value >> 16) as u16)
                })();
                lo_first.map_err(|mut e| {
                    e.stacked_pc = e.stacked_pc.wrapping_add(2);
                    e
                })?;
            }
            (4, _) => {
                let dst = self.decode_ea(bus, master, dst_mode, dst_reg, size);
                self.ea_write_rmw(bus, master, dst, size, value)
                    .map_err(|mut e| {
                        e.stacked_pc = e.stacked_pc.wrapping_add(2);
                        e
                    })?;
            }
            // MOVE from a *memory* source to abs.l interleaves the write
            // with the second address-word fetch, so a faulting write
            // stacks a PC one word earlier; register and immediate sources
            // fetch both address words up front (hardware-verified).
            (7, _) if dst_reg == 1 => {
                let src_is_mem = src_mode >= 2 && !(src_mode == 7 && src_reg == 4);
                let dst = self.decode_ea(bus, master, dst_mode, dst_reg, size);
                self.ea_write(bus, master, dst, size, value)
                    .map_err(|mut e| {
                        if src_is_mem {
                            e.stacked_pc = e.stacked_pc.wrapping_sub(2);
                        }
                        e
                    })?;
            }
            _ => {
                let dst = self.decode_ea(bus, master, dst_mode, dst_reg, size);
                self.ea_write(bus, master, dst, size, value)?;
            }
        }

        // Charged from the transfers this actually made, not from the table.
        // MOVE's own internal time is nil: the opcode fetch and every operand
        // access are bus cycles, and the only clocks it spends off the bus are
        // the address arithmetic of its two modes. A predecrement *destination*
        // is the documented exception: the part overlaps the decrement with the
        // write it is already committed to, so it costs its transfer and no
        // more, where the same mode as a *source* pays two clocks for it.
        let internal = ea_internal(src_mode, src_reg)
            + if dst_mode & 7 == 4 {
                0
            } else {
                ea_internal(dst_mode, dst_reg)
            };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }

    /// MOVEQ #d8,Dn — line 0x7, bit 8 clear. Sign-extends the 8-bit literal
    /// to 32 bits and writes the full data register.
    ///
    /// Flags: N and Z from the 32-bit result, V and C cleared, X untouched.
    pub(crate) fn op_moveq<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = ((opcode >> 9) & 7) as usize;
        let value = sext8(opcode as u8);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// SWAP Dn (0x4840): exchange the upper and lower words of a data
    /// register.
    ///
    /// Flags: N and Z from the full 32-bit result (N = new bit 31), V and C
    /// cleared, X untouched (data-movement rule).
    pub(crate) fn op_swap<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        let value = self.d[reg].rotate_left(16);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// MOVEP Dx,d16(Ay) / MOVEP d16(Ay),Dx (line 0x0, bit 8 set, EA mode
    /// 001): transfer a word or long between a data register and every
    /// *other* byte of memory — the high byte at the displaced address,
    /// then descending register bytes at addr+2, +4, +6. Built for 8-bit
    /// peripherals on one half of the 16-bit bus; byte accesses mean an odd
    /// base address is legal and nothing can address-error.
    ///
    /// Opmode bits 8-6: 100 word mem→reg, 101 long mem→reg, 110 word
    /// reg→mem, 111 long reg→mem.
    ///
    /// Flags: none. 16 cycles (word) / 24 (long).
    pub(crate) fn op_movep<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let dn = ((opcode >> 9) & 7) as usize;
        let an = (opcode & 7) as usize;
        let long = opcode & 0x0040 != 0;
        let to_memory = opcode & 0x0080 != 0;
        let disp = sext16(self.read_imm_word(bus, master));
        let base = self.a[an].wrapping_add(disp);
        let bytes: u32 = if long { 4 } else { 2 };

        if to_memory {
            for i in 0..bytes {
                let shift = 8 * (bytes - 1 - i);
                let addr = base.wrapping_add(2 * i);
                self.write_byte_at(bus, master, addr, (self.d[dn] >> shift) as u8);
            }
        } else {
            let mut value = 0u32;
            for i in 0..bytes {
                let addr = base.wrapping_add(2 * i);
                value = (value << 8) | self.read_byte_at(bus, master, addr) as u32;
            }
            let mask = if long { 0xFFFF_FFFF } else { 0x0000_FFFF };
            self.d[dn] = (self.d[dn] & !mask) | (value & mask);
        }
        // MOVEP is all bus and no thinking: the opcode, the displacement word,
        // and one byte transfer per register byte. Nothing is left over.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// MOVE SR,<ea> (0x40C0): write the status register to a word
    /// data-alterable destination. Unprivileged on the 68000 (the 68010
    /// made it privileged).
    ///
    /// Flags: none. 6 cycles to Dn, 8 + EA to memory.
    pub(crate) fn op_move_from_sr<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        // Unprivileged on the 68000; privileged on the 68010+ (where MOVE
        // from CCR became the user-mode way to read the flags). On the
        // 68010 a user-mode attempt vectors to the privilege handler.
        if self.is_68010_plus() && !self.privilege_check(bus, master)? {
            return Ok(());
        }
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish_from_bus(bus, master, 0); // illegal destination
            return Ok(());
        }
        let dst = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        // The 68000 reads the destination before rewriting it (visible as
        // the R/W bit of an address-error frame) — hardware-verified.
        let _ = self.ea_read(bus, master, dst, Size::Word)?;
        // A read-modify-write like the ALU's, and it refills where they do:
        // the trace records read, program read, write.
        self.ea_write_rmw(bus, master, dst, Size::Word, self.sr as u32)?;
        // A register destination transfers nothing, leaving two clocks to read
        // the status register out; a memory one reads and writes, both counted.
        let internal = if ea_mode == 0 {
            2
        } else {
            ea_internal(ea_mode, ea_reg)
        };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }

    /// MOVE <ea>,CCR (0x44C0): load the flag byte from a word data source;
    /// only the five implemented CCR bits stick.
    ///
    /// Flags: X/N/Z/V/C all loaded. 12 cycles + EA.
    pub(crate) fn op_move_to_ccr<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 {
            self.finish_from_bus(bus, master, 0); // address-register source is illegal
            return Ok(());
        }
        let src = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, src, Size::Word)? as u16;
        self.write_ccr(value);
        // The flag write discards the queue, so the finish refills two words
        // rather than one: `MOVE.w D3, CCR` is two program reads for the one
        // word it consumed. Four clocks are left, the part loading the
        // register and settling the mode it may just have changed.
        // Those four clocks run before the refetch, not after it: the part's
        // sequence is two two-clock steps and then its two program reads.
        self.finish_from_bus_address_first(bus, master, 4 + ea_internal(ea_mode, ea_reg));
        Ok(())
    }

    /// MOVE <ea>,SR (0x46C0, privileged): load the whole status register
    /// from a word data source, routing the S bit through the SP swap.
    ///
    /// Flags: the whole SR is loaded. 12 cycles + EA.
    pub(crate) fn op_move_to_sr<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if !self.privilege_check(bus, master)? {
            return Ok(());
        }
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 {
            self.finish_from_bus(bus, master, 0); // address-register source is illegal
            return Ok(());
        }
        let src = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, src, Size::Word)? as u16;
        self.write_sr(value);
        // As MOVE to CCR: the status-register write discards the queue and the
        // finish refills both words.
        // Those four clocks run before the refetch, not after it: the part's
        // sequence is two two-clock steps and then its two program reads.
        self.finish_from_bus_address_first(bus, master, 4 + ea_internal(ea_mode, ea_reg));
        Ok(())
    }

    /// MOVE An,USP / MOVE USP,An (0x4E60-0x4E6F, privileged): transfer
    /// between an address register and the parked user stack pointer
    /// (bit 3 selects the direction).
    ///
    /// Flags: none. 4 cycles.
    pub(crate) fn op_move_usp<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if !self.privilege_check(bus, master)? {
            return Ok(());
        }
        let reg = (opcode & 7) as usize;
        if opcode & 8 != 0 {
            self.a[reg] = self.usp;
        } else {
            self.usp = self.a[reg];
        }
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// EXG Rx,Ry (line 0xC, opmodes 01000/01001/10001): exchange two full
    /// 32-bit registers — Dx,Dy / Ax,Ay / Dx,Ay.
    ///
    /// Flags: none. 6 cycles.
    pub(crate) fn op_exg<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let rx = ((opcode >> 9) & 7) as usize;
        let ry = (opcode & 7) as usize;
        match (opcode >> 3) & 0x1F {
            0x08 => self.d.swap(rx, ry),
            0x09 => self.a.swap(rx, ry),
            0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]),
            // 10000 (opmode 6, EA mode 0) is an unassigned encoding
            _ => {
                self.finish_from_bus(bus, master, 0);
                return Ok(());
            }
        }
        // Registers only: the opcode fetch, plus two clocks to swap them.
        self.finish_from_bus(bus, master, 2);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The MOVE family is covered by integration tests in
    // core/tests/m68000_move_test.rs (every addressing mode, MOVEQ
    // sign-extension, and flag behavior). Unit tests here only pin the
    // destination-cycle quirk.
    use super::*;

    /// A predecrement *destination* costs MOVE nothing beyond its write.
    ///
    /// As a source, `-(An)` spends two clocks decrementing before it can put
    /// an address on the bus. As a destination it does not: the part overlaps
    /// the decrement with the write it is already committed to, so the mode
    /// costs exactly its one transfer. That is why the destination side of
    /// `op_move` contributes no internal time for mode 4 while the source side
    /// contributes two, and it is the one asymmetry in MOVE's timing.
    #[test]
    fn a_predecrement_destination_costs_nothing_beyond_its_write() {
        assert_eq!(
            ea_internal(4, 0),
            2,
            "as a source it pays for the decrement"
        );

        // The destination arm zeroes exactly that, and nothing else.
        let dest_internal = |mode: u8, reg: u8| {
            if mode & 7 == 4 {
                0
            } else {
                ea_internal(mode, reg)
            }
        };
        assert_eq!(dest_internal(4, 0), 0, "as a destination it does not");
        assert_eq!(dest_internal(2, 0), 0, "(An) never had internal time");
        assert_eq!(
            dest_internal(6, 0),
            2,
            "an indexed destination still pays for its index add"
        );
    }
}
