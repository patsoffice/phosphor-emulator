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
use super::addressing::{Size, ea_cycles};
use super::flags::SrFlag;
use crate::core::{Bus, BusMaster};

impl M68000 {
    /// BTST / BCHG / BCLR / BSET `<bit>,<ea>` — `oo` bits 7-6 select the
    /// operation (00/01/10/11). BTST accepts any data source, including
    /// PC-relative and (dynamic form only) immediate; the three modifying
    /// ops need a data-alterable destination.
    pub(crate) fn op_bitop<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        dynamic: bool,
    ) {
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
            self.finish(4); // illegal encoding (exception lands in M5)
            return;
        }

        // The static bit number is an extension word ahead of the EA words.
        let bit_number = if dynamic {
            self.d[((opcode >> 9) & 7) as usize]
        } else {
            self.read_imm_word(bus, master) as u32
        };
        let static_extra = if dynamic { 0 } else { 4 };

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
            // BCLR Dn pays two extra cycles over BCHG/BSET
            let base = match op {
                0 => 6,
                2 => 10,
                _ => 8,
            };
            self.finish(base + static_extra);
        } else {
            // Memory (or immediate, for dynamic BTST): byte operation mod 8
            let mask = 1u32 << (bit_number & 7);
            let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte);
            let old = self.ea_read(bus, master, ea, Size::Byte);
            self.set_flag(SrFlag::Z, old & mask == 0);
            if !is_btst {
                let new = match op {
                    1 => old ^ mask,
                    2 => old & !mask,
                    _ => old | mask,
                };
                self.ea_write(bus, master, ea, Size::Byte, new);
            }

            let base = if is_btst { 4 } else { 8 };
            self.finish(base + static_extra + ea_cycles(ea_mode, ea_reg, Size::Byte));
        }
    }
}
