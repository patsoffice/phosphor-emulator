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
use crate::core::{Bus, BusMaster};

/// Documented LEA timing per control addressing mode (M68000UM table 8-1);
/// PEA is uniformly 8 cycles more for the long push.
fn lea_cycles(mode: u8, reg: u8) -> u32 {
    match mode & 7 {
        2 => 4,  // (An)
        5 => 8,  // d16(An)
        6 => 12, // d8(An,Xn)
        _ => match reg & 7 {
            0 => 8,  // abs.w
            1 => 12, // abs.l
            2 => 8,  // d16(PC)
            _ => 12, // d8(PC,Xn)
        },
    }
}

impl M68000 {
    /// LEA `<ea>`,An (line 0x4, bits 8-6 = 111) and PEA `<ea>` (0x4848-
    /// 0x487B): resolve a control-mode effective address and either load it
    /// into An or push it onto the stack.
    ///
    /// Flags: none.
    pub(crate) fn op_lea_pea<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
            self.finish(4); // illegal encoding
            return Ok(());
        }
        let Ea::Mem(addr) = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word) else {
            unreachable!("control addressing modes always resolve to memory");
        };
        if push {
            self.push_long(bus, master, addr)?;
        } else {
            self.a[((opcode >> 9) & 7) as usize] = addr;
        }
        self.finish(lea_cycles(ea_mode, ea_reg) + if push { 8 } else { 0 });
        Ok(())
    }

    /// LINK An,#disp (0x4E50): push An, point An at the new frame (the
    /// updated stack pointer), then advance SP by the sign-extended
    /// displacement (normally negative, reserving locals).
    ///
    /// LINK A7 pushes the *decremented* A7 — the register being pushed is
    /// also the stack pointer doing the pushing.
    ///
    /// Flags: none. 16 cycles.
    pub(crate) fn op_link<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        let disp = sext16(self.read_imm_word(bus, master));
        let value = if reg == 7 {
            self.a[7].wrapping_sub(4)
        } else {
            self.a[reg]
        };
        self.push_long(bus, master, value)?;
        self.a[reg] = self.a[7];
        self.a[7] = self.a[7].wrapping_add(disp);
        self.finish(16);
        Ok(())
    }

    /// UNLK An (0x4E58): collapse the frame — SP = An, then pop the saved
    /// value back into An.
    ///
    /// UNLK A7 ends with A7 holding the popped value (the post-pop
    /// increment is overwritten by the load).
    ///
    /// Flags: none. 12 cycles.
    pub(crate) fn op_unlk<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let reg = (opcode & 7) as usize;
        self.a[7] = self.a[reg];
        let value = self.pop_long(bus, master)?;
        self.a[reg] = value;
        self.finish(12);
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
    pub(crate) fn op_movem<B: Bus<Address = u32, Data = u16> + ?Sized>(
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
            self.finish(4); // illegal encoding
            return Ok(());
        }
        let mask = self.read_imm_word(bus, master);

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

        // Documented timing: a per-register transfer cost on top of a
        // per-mode setup cost (loads pay one extra read ahead of the
        // transfers).
        let per_reg = if size == Size::Long { 8 } else { 4 } * mask.count_ones();
        let base = match ea_mode {
            2..=4 => 8,
            5 => 12,
            6 => 14,
            _ => match ea_reg {
                0 => 12,
                1 => 16,
                2 => 12,
                _ => 14,
            },
        } + if to_registers { 4 } else { 0 };
        self.finish(base + per_reg);
        Ok(())
    }
}
