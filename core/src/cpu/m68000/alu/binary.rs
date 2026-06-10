//! ADD / ADDA / ADDI, SUB / SUBA / SUBI, CMP / CMPA / CMPI.
//!
//! Lines 0x9 (SUB) and 0xD (ADD) share one encoding: `rrr ooo mmm RRR` with
//! opmode `ooo` selecting direction and size — 000-010 `Dn ⟵ Dn op <ea>`,
//! 100-110 `<ea> ⟵ <ea> op Dn`, 011/111 the address-register forms
//! (ADDA/SUBA word/long). Line 0xB carries CMP (opmodes 000-010) and CMPA
//! (011/111). The immediate forms live on line 0x0 with the literal fetched
//! ahead of the destination EA.
//!
//! Opmodes 100-110 with an EA mode of Dn/An encode ADDX/SUBX (line 0xB:
//! CMPM/EOR); those land in M2 and are skipped here.

use super::super::M68000;
use super::super::addressing::{Size, ea_cycles, sext16};
use super::super::flags::SrFlag;
use crate::core::{Bus, BusMaster};

/// Decode the two-bit size field used by opmodes and immediates
/// (00 = byte, 01 = word, 10 = long; 11 is never a size).
fn size_from_bits(bits: u16) -> Option<Size> {
    match bits & 3 {
        0 => Some(Size::Byte),
        1 => Some(Size::Word),
        2 => Some(Size::Long),
        _ => None,
    }
}

impl M68000 {
    /// ADD (line 0xD) and SUB (line 0x9), including the ADDA/SUBA opmodes.
    ///
    /// Flags: N/Z/V/C from the sized result and **X = C** (arithmetic rule).
    /// ADDA/SUBA set no flags at all and operate on the full 32-bit address
    /// register after sign-extending a word operand.
    pub(crate) fn op_add_sub<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        is_add: bool,
    ) {
        let dn = ((opcode >> 9) & 7) as usize;
        let opmode = (opcode >> 6) & 7;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        match opmode {
            // Dn ⟵ Dn op <ea>
            0..=2 => {
                let size = size_from_bits(opmode).unwrap();
                // Byte reads from An are illegal encodings
                if size == Size::Byte && ea_mode == 1 {
                    self.finish(4);
                    return;
                }
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size);
                let b = self.ea_read(bus, master, src, size);
                let a = self.d[dn];
                let result = if is_add {
                    self.add_with_flags(size, a, b)
                } else {
                    self.sub_with_flags(size, a, b)
                };
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.d[dn] = (a & !size.mask()) | result;

                let base = if size == Size::Long { 6 } else { 4 };
                self.finish(base + ea_cycles(ea_mode, ea_reg, size));
            }
            // <ea> ⟵ <ea> op Dn (memory-alterable destinations only;
            // Dn/An here encode ADDX/SUBX, which land in M2)
            4..=6 => {
                let size = size_from_bits(opmode).unwrap();
                if ea_mode < 2 || (ea_mode == 7 && ea_reg >= 2) {
                    self.finish(4); // ADDX/SUBX (M2) or illegal destination
                    return;
                }
                let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size);
                let a = self.ea_read(bus, master, dst, size);
                let b = self.d[dn];
                let result = if is_add {
                    self.add_with_flags(size, a, b)
                } else {
                    self.sub_with_flags(size, a, b)
                };
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write(bus, master, dst, size, result);

                let base = if size == Size::Long { 12 } else { 8 };
                self.finish(base + ea_cycles(ea_mode, ea_reg, size));
            }
            // ADDA/SUBA: An ⟵ An op <ea> (word sign-extends, no flags)
            _ => {
                let size = if opmode == 3 { Size::Word } else { Size::Long };
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size);
                let value = self.ea_read(bus, master, src, size);
                let value = if size == Size::Word {
                    sext16(value as u16)
                } else {
                    value
                };
                let a = self.a[dn];
                self.a[dn] = if is_add {
                    a.wrapping_add(value)
                } else {
                    a.wrapping_sub(value)
                };

                let base = if size == Size::Long { 6 } else { 8 };
                self.finish(base + ea_cycles(ea_mode, ea_reg, size));
            }
        }
    }

    /// CMP (line 0xB, opmodes 000-010) and CMPA (011/111).
    ///
    /// Flags: N/Z/V/C from `dst - src`; the result is discarded and **X is
    /// never altered** (the one arithmetic op that leaves X alone). CMPA
    /// sign-extends a word operand and compares the full 32-bit An.
    /// Opmodes 100-110 encode CMPM/EOR and land in M2.
    pub(crate) fn op_cmp<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let dn = ((opcode >> 9) & 7) as usize;
        let opmode = (opcode >> 6) & 7;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        match opmode {
            0..=2 => {
                let size = size_from_bits(opmode).unwrap();
                if size == Size::Byte && ea_mode == 1 {
                    self.finish(4); // byte read from An is illegal
                    return;
                }
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size);
                let b = self.ea_read(bus, master, src, size);
                self.sub_with_flags(size, self.d[dn], b);

                let base = if size == Size::Long { 6 } else { 4 };
                self.finish(base + ea_cycles(ea_mode, ea_reg, size));
            }
            3 | 7 => {
                let size = if opmode == 3 { Size::Word } else { Size::Long };
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size);
                let value = self.ea_read(bus, master, src, size);
                let value = if size == Size::Word {
                    sext16(value as u16)
                } else {
                    value
                };
                self.sub_with_flags(Size::Long, self.a[dn], value);

                self.finish(6 + ea_cycles(ea_mode, ea_reg, size));
            }
            // CMPM / EOR (M2)
            _ => self.finish(4),
        }
    }

    /// ADDI / SUBI / CMPI (line 0x0, sub-ops 0x6/0x4/0xC): immediate
    /// literal first, then a data-alterable destination EA.
    ///
    /// Flags: ADDI/SUBI follow the arithmetic rule (N/Z/V/C and X = C);
    /// CMPI sets N/Z/V/C only and leaves X untouched.
    ///
    /// Returns false if the opcode is not one of the three immediate forms
    /// handled here (the rest of line 0x0 lands in M2/M4).
    pub(crate) fn op_imm_alu<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        let op = opcode & 0x0F00;
        if !matches!(op, 0x0400 | 0x0600 | 0x0C00) {
            return false;
        }
        let Some(size) = size_from_bits(opcode >> 6) else {
            return false;
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Destination must be data-alterable: An, PC-relative, and immediate
        // destinations are illegal encodings.
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            return false;
        }

        // Immediate data precedes the destination extension words.
        let imm = self.decode_ea(bus, master, 7, 4, size);
        let b = self.ea_read(bus, master, imm, size);
        let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size);
        let a = self.ea_read(bus, master, dst, size);

        let mem = ea_mode != 0;
        let long = size == Size::Long;
        match op {
            0x0600 => {
                // ADDI
                let result = self.add_with_flags(size, a, b);
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write(bus, master, dst, size, result);
            }
            0x0400 => {
                // SUBI
                let result = self.sub_with_flags(size, a, b);
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write(bus, master, dst, size, result);
            }
            _ => {
                // CMPI — result discarded, X untouched
                self.sub_with_flags(size, a, b);
            }
        }

        let base = match (op, mem, long) {
            (0x0C00, false, false) => 8,
            (0x0C00, false, true) => 14,
            (0x0C00, true, false) => 8,
            (0x0C00, true, true) => 12,
            (_, false, false) => 8,
            (_, false, true) => 16,
            (_, true, false) => 12,
            (_, true, true) => 20,
        };
        self.finish(
            base + if mem {
                ea_cycles(ea_mode, ea_reg, size)
            } else {
                0
            },
        );
        true
    }
}
