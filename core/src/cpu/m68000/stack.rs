//! Address computation and stack-frame instructions: LEA / PEA (line 0x4)
//! and LINK / UNLK (the 0x4E50 group).
//!
//! LEA and PEA materialize a control-mode effective address without
//! touching memory at it — LEA into An, PEA onto the stack. Like JMP/JSR
//! they receive the full 32-bit computed address (the 24-bit mask applies
//! only at the bus). LINK/UNLK build and tear down stack frames. None of
//! these alter the CCR.

use super::M68000;
use super::addressing::{Ea, Size, sext16};
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
    ) {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Control addressing only — same legality rule as JMP/JSR.
        if !(matches!(ea_mode, 2 | 5 | 6) || (ea_mode == 7 && ea_reg < 4)) {
            self.finish(4); // illegal encoding (exception lands in M5)
            return;
        }
        let Ea::Mem(addr) = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word) else {
            unreachable!("control addressing modes always resolve to memory");
        };
        if push {
            self.push_long(bus, master, addr);
        } else {
            self.a[((opcode >> 9) & 7) as usize] = addr;
        }
        self.finish(lea_cycles(ea_mode, ea_reg) + if push { 8 } else { 0 });
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
    ) {
        let reg = (opcode & 7) as usize;
        let disp = sext16(self.read_imm_word(bus, master));
        let value = if reg == 7 {
            self.a[7].wrapping_sub(4)
        } else {
            self.a[reg]
        };
        self.push_long(bus, master, value);
        self.a[reg] = self.a[7];
        self.a[7] = self.a[7].wrapping_add(disp);
        self.finish(16);
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
    ) {
        let reg = (opcode & 7) as usize;
        self.a[7] = self.a[reg];
        let value = self.pop_long(bus, master);
        self.a[reg] = value;
        self.finish(12);
    }
}
