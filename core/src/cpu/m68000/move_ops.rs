//! MOVE / MOVEA / MOVEQ / SWAP / EXG — data movement instructions.
//!
//! (MOVE to/from SR/CCR/USP and MOVEP live on lines 0x0/0x4 and land
//! in M5.)

use super::M68000;
use super::addressing::{Size, ea_cycles, sext8};
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
    ) {
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
        // (MOVE.b An,<ea> / MOVEA.b). Treated as a NOP until the
        // illegal-instruction exception lands in M5.
        if size == Size::Byte && (src_mode == 1 || dst_mode == 1) {
            self.finish(4);
            return;
        }

        let src = self.decode_ea(bus, master, src_mode, src_reg, size);
        let value = self.ea_read(bus, master, src, size);

        // MOVEA reuses the An write path: ea_write sign-extends word writes
        // to address registers, which is exactly the MOVEA.w rule.
        let dst = self.decode_ea(bus, master, dst_mode, dst_reg, size);
        self.ea_write(bus, master, dst, size, value);

        if dst_mode != 1 {
            self.set_flags_logical(size, value);
        }

        let cycles =
            4 + ea_cycles(src_mode, src_reg, size) + move_dest_cycles(dst_mode, dst_reg, size);
        self.finish(cycles);
    }

    /// MOVEQ #d8,Dn — line 0x7, bit 8 clear. Sign-extends the 8-bit literal
    /// to 32 bits and writes the full data register.
    ///
    /// Flags: N and Z from the 32-bit result, V and C cleared, X untouched.
    pub(crate) fn op_moveq(&mut self, opcode: u16) {
        let reg = ((opcode >> 9) & 7) as usize;
        let value = sext8(opcode as u8);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish(4);
    }

    /// SWAP Dn (0x4840): exchange the upper and lower words of a data
    /// register.
    ///
    /// Flags: N and Z from the full 32-bit result (N = new bit 31), V and C
    /// cleared, X untouched (data-movement rule).
    pub(crate) fn op_swap(&mut self, opcode: u16) {
        let reg = (opcode & 7) as usize;
        let value = self.d[reg].rotate_left(16);
        self.d[reg] = value;
        self.set_flags_logical(Size::Long, value);
        self.finish(4);
    }

    /// EXG Rx,Ry (line 0xC, opmodes 01000/01001/10001): exchange two full
    /// 32-bit registers — Dx,Dy / Ax,Ay / Dx,Ay.
    ///
    /// Flags: none. 6 cycles.
    pub(crate) fn op_exg(&mut self, opcode: u16) {
        let rx = ((opcode >> 9) & 7) as usize;
        let ry = (opcode & 7) as usize;
        match (opcode >> 3) & 0x1F {
            0x08 => self.d.swap(rx, ry),
            0x09 => self.a.swap(rx, ry),
            0x11 => std::mem::swap(&mut self.d[rx], &mut self.a[ry]),
            // 10000 (opmode 6, EA mode 0) is an unassigned encoding
            _ => {
                self.finish(4);
                return;
            }
        }
        self.finish(6);
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
