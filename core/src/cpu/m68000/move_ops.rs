//! MOVE / MOVEA / MOVEQ / MOVEP / SWAP / EXG and the SR/CCR/USP moves —
//! data movement instructions.

use super::M68000;
use super::addressing::{AccessResult, Ea, Size, ea_cycles, sext8, sext16};
use crate::core::{Bus, BusMaster};

/// Destination effective-address time for MOVE (M68000UM table 8-2). Same as
/// the source table except `-(An)`, which overlaps its decrement with the
/// write and costs the same as `(An)`.
fn move_dest_cycles(mode: u8, reg: u8, size: Size) -> u32 {
    match mode & 7 {
        4 => ea_cycles(2, reg, size),
        m => ea_cycles(m, reg, size),
    }
}

impl M68000 {
    /// MOVE <ea>,<ea> and MOVEA <ea>,An — lines 0x1 (byte), 0x2 (long),
    /// 0x3 (word). Opcode layout: `size:2 | dst_reg:3 | dst_mode:3 |
    /// src_mode:3 | src_reg:3`; a destination mode of An selects MOVEA.
    ///
    /// Flags (MOVE): N and Z from the moved value, V and C cleared,
    /// X untouched (data movement never alters X). MOVEA sets no flags and
    /// sign-extends a word source to the full address register.
    pub(crate) fn op_move<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
            self.finish(4);
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
            (4, Size::Long) => {
                let reg = dst_reg as usize;
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
                self.ea_write(bus, master, dst, size, value)
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

        let cycles =
            4 + ea_cycles(src_mode, src_reg, size) + move_dest_cycles(dst_mode, dst_reg, size);
        self.finish(cycles);
        Ok(())
    }

    /// MOVEQ #d8,Dn — line 0x7, bit 8 clear. Sign-extends the 8-bit literal
    /// to 32 bits and writes the full data register.
    ///
    /// Flags: N and Z from the 32-bit result, V and C cleared, X untouched.
    pub(crate) fn op_moveq(&mut self, opcode: u16) -> AccessResult<()> {
        let reg = ((opcode >> 9) & 7) as usize;
        let value = sext8(opcode as u8);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish(4);
        Ok(())
    }

    /// SWAP Dn (0x4840): exchange the upper and lower words of a data
    /// register.
    ///
    /// Flags: N and Z from the full 32-bit result (N = new bit 31), V and C
    /// cleared, X untouched (data-movement rule).
    pub(crate) fn op_swap(&mut self, opcode: u16) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        let value = self.d[reg].rotate_left(16);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish(4);
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
    pub(crate) fn op_movep<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
        self.finish(if long { 24 } else { 16 });
        Ok(())
    }

    /// MOVE SR,<ea> (0x40C0): write the status register to a word
    /// data-alterable destination. Unprivileged on the 68000 (the 68010
    /// made it privileged).
    ///
    /// Flags: none. 6 cycles to Dn, 8 + EA to memory.
    pub(crate) fn op_move_from_sr<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
            self.finish(4); // illegal destination
            return Ok(());
        }
        let dst = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        // The 68000 reads the destination before rewriting it (visible as
        // the R/W bit of an address-error frame) — hardware-verified.
        let _ = self.ea_read(bus, master, dst, Size::Word)?;
        self.ea_write(bus, master, dst, Size::Word, self.sr as u32)?;
        self.finish(if ea_mode == 0 {
            6
        } else {
            8 + ea_cycles(ea_mode, ea_reg, Size::Word)
        });
        Ok(())
    }

    /// MOVE <ea>,CCR (0x44C0): load the flag byte from a word data source;
    /// only the five implemented CCR bits stick.
    ///
    /// Flags: X/N/Z/V/C all loaded. 12 cycles + EA.
    pub(crate) fn op_move_to_ccr<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 {
            self.finish(4); // address-register source is illegal
            return Ok(());
        }
        let src = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, src, Size::Word)? as u16;
        self.sr = (self.sr & 0xFF00) | (value & 0x001F);
        self.finish(12 + ea_cycles(ea_mode, ea_reg, Size::Word));
        Ok(())
    }

    /// MOVE <ea>,SR (0x46C0, privileged): load the whole status register
    /// from a word data source, routing the S bit through the SP swap.
    ///
    /// Flags: the whole SR is loaded. 12 cycles + EA.
    pub(crate) fn op_move_to_sr<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
            self.finish(4); // address-register source is illegal
            return Ok(());
        }
        let src = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word);
        let value = self.ea_read(bus, master, src, Size::Word)? as u16;
        self.write_sr(value);
        self.finish(12 + ea_cycles(ea_mode, ea_reg, Size::Word));
        Ok(())
    }

    /// MOVE An,USP / MOVE USP,An (0x4E60-0x4E6F, privileged): transfer
    /// between an address register and the parked user stack pointer
    /// (bit 3 selects the direction).
    ///
    /// Flags: none. 4 cycles.
    pub(crate) fn op_move_usp<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
        self.finish(4);
        Ok(())
    }

    /// EXG Rx,Ry (line 0xC, opmodes 01000/01001/10001): exchange two full
    /// 32-bit registers — Dx,Dy / Ax,Ay / Dx,Ay.
    ///
    /// Flags: none. 6 cycles.
    pub(crate) fn op_exg(&mut self, opcode: u16) -> AccessResult<()> {
        let rx = ((opcode >> 9) & 7) as usize;
        let ry = (opcode & 7) as usize;
        match (opcode >> 3) & 0x1F {
            0x08 => self.d.swap(rx, ry),
            0x09 => self.a.swap(rx, ry),
            0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]),
            // 10000 (opmode 6, EA mode 0) is an unassigned encoding
            _ => {
                self.finish(4);
                return Ok(());
            }
        }
        self.finish(6);
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

    #[test]
    fn move_dest_predecrement_costs_same_as_indirect() {
        assert_eq!(move_dest_cycles(4, 0, Size::Word), 4);
        assert_eq!(move_dest_cycles(4, 0, Size::Long), 8);
        assert_eq!(move_dest_cycles(5, 0, Size::Word), 8);
    }
}
