//! M68000 effective-address decoding and word-bus memory access.
//!
//! The 6-bit EA field of an opcode is `mode:3 | reg:3` and selects one of 12
//! addressing modes. [`M68000::decode_ea`] fetches any required extension
//! words (advancing PC) and resolves the operand location to an [`Ea`], so
//! read, write, and read-modify-write paths share a single decode.
//!
//! # Word-bus memory model
//!
//! The 68000 data bus is 16 bits wide: every bus transaction is one word at
//! an even address. These helpers map sized accesses onto that bus:
//!
//! - **Word**: one transaction. **Long**: two transactions (big-endian, high
//!   word first).
//! - **Byte**: the containing word is read and the high (even address / UDS)
//!   or low (odd address / LDS) byte selected. Byte *writes*
//!   read-modify-write the containing word. This is correct for RAM (and for
//!   state-comparison validation, since the other byte is preserved) but not
//!   faithful for write-only or side-effecting memory-mapped registers —
//!   revisit if a real machine needs strobe-accurate byte writes.
//! - **Odd word/long addresses** raise an address error (vector 3) on real
//!   hardware. The exception lands in M5; until then the access is flagged
//!   via `address_error` and forced even so execution stays deterministic.
//!
//! Effective addresses are computed at the full 32 bits (the 68000 ALU is
//! 32-bit internally — JMP/JSR load the unmasked value into PC, LEA into
//! An) and masked to the physical bus width (24 bits on the 68000/010)
//! only when driven onto the bus.

use super::M68000;
use crate::core::{Bus, BusMaster};

/// Operand size of an instruction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    /// Number of bytes an operand of this size occupies.
    #[inline]
    pub(crate) fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    /// Mask selecting the low `bytes()` of a u32 value.
    #[inline]
    pub(crate) fn mask(self) -> u32 {
        match self {
            Size::Byte => 0x0000_00FF,
            Size::Word => 0x0000_FFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    /// Sign bit of an operand of this size.
    #[inline]
    pub(crate) fn sign_bit(self) -> u32 {
        match self {
            Size::Byte => 0x0000_0080,
            Size::Word => 0x0000_8000,
            Size::Long => 0x8000_0000,
        }
    }
}

/// A resolved operand location.
///
/// Extension words have already been consumed and any postincrement /
/// predecrement side effects applied; reading and writing through an `Ea`
/// performs no further decoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ea {
    /// Data register Dn.
    DataReg(usize),
    /// Address register An.
    AddrReg(usize),
    /// Memory operand at a full 32-bit effective address (masked to the
    /// physical bus width on access).
    Mem(u32),
    /// Immediate value (already fetched from the instruction stream).
    Imm(u32),
}

/// Documented effective-address calculation time in clock cycles for a
/// *source* operand (M68000UM table 8-1): the extension-word fetches plus
/// the operand read. Long operands add one extra word transaction (4 cycles)
/// over byte/word for every memory mode.
pub(crate) fn ea_cycles(mode: u8, reg: u8, size: Size) -> u32 {
    let long = matches!(size, Size::Long);
    let bw_l = |bw: u32, l: u32| if long { l } else { bw };
    match mode & 7 {
        0 | 1 => 0,          // Dn / An
        2 | 3 => bw_l(4, 8), // (An) / (An)+
        4 => bw_l(6, 10),    // -(An)
        5 => bw_l(8, 12),    // d16(An)
        6 => bw_l(10, 14),   // d8(An,Xn)
        _ => match reg & 7 {
            0 => bw_l(8, 12),  // abs.w
            1 => bw_l(12, 16), // abs.l
            2 => bw_l(8, 12),  // d16(PC)
            3 => bw_l(10, 14), // d8(PC,Xn)
            _ => bw_l(4, 8),   // #imm
        },
    }
}

/// Sign-extend a byte to 32 bits.
#[inline]
pub(crate) fn sext8(v: u8) -> u32 {
    v as i8 as i32 as u32
}

/// Sign-extend a word to 32 bits.
#[inline]
pub(crate) fn sext16(v: u16) -> u32 {
    v as i16 as i32 as u32
}

impl M68000 {
    /// Flag word/long access at an odd address (address error, vector 3 —
    /// exception entry lands in M5) and force the address even so execution
    /// continues deterministically.
    #[inline]
    fn check_aligned(&mut self, addr: u32) -> u32 {
        if addr & 1 != 0 {
            self.address_error = true;
        }
        addr & !1
    }

    /// Read one word at `addr` (must be even; odd flags an address error).
    pub(crate) fn read_word_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> u16 {
        let addr = self.check_aligned(addr);
        bus.read(master, self.mask_addr(addr))
    }

    /// Write one word at `addr` (must be even; odd flags an address error).
    pub(crate) fn write_word_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u16,
    ) {
        let addr = self.check_aligned(addr);
        bus.write(master, self.mask_addr(addr), data);
    }

    /// Read a long word as two word transactions (big-endian, high first).
    pub(crate) fn read_long_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> u32 {
        let hi = self.read_word_at(bus, master, addr);
        let lo = self.read_word_at(bus, master, addr.wrapping_add(2));
        ((hi as u32) << 16) | lo as u32
    }

    /// Write a long word as two word transactions (big-endian, high first).
    pub(crate) fn write_long_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u32,
    ) {
        self.write_word_at(bus, master, addr, (data >> 16) as u16);
        self.write_word_at(bus, master, addr.wrapping_add(2), data as u16);
    }

    /// Read one byte: fetch the containing word and select the high byte
    /// (even address / UDS) or low byte (odd address / LDS).
    pub(crate) fn read_byte_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> u8 {
        let word = bus.read(master, self.mask_addr(addr & !1));
        if addr & 1 == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }

    /// Write one byte by read-modify-writing the containing word (see the
    /// module docs for the MMIO caveat).
    pub(crate) fn write_byte_at<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u8,
    ) {
        let word_addr = self.mask_addr(addr & !1);
        let word = bus.read(master, word_addr);
        let merged = if addr & 1 == 0 {
            (word & 0x00FF) | ((data as u16) << 8)
        } else {
            (word & 0xFF00) | data as u16
        };
        bus.write(master, word_addr, merged);
    }

    /// Push a long word onto the active stack (A7 predecrements by 4).
    pub(crate) fn push_long<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        value: u32,
    ) {
        self.a[7] = self.a[7].wrapping_sub(4);
        self.write_long_at(bus, master, self.a[7], value);
    }

    /// Pop a word from the active stack (A7 postincrements by 2).
    pub(crate) fn pop_word<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u16 {
        let value = self.read_word_at(bus, master, self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        value
    }

    /// Pop a long word from the active stack (A7 postincrements by 4).
    pub(crate) fn pop_long<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u32 {
        let value = self.read_long_at(bus, master, self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        value
    }

    /// Fetch one extension word from the instruction stream and advance PC
    /// (the prefetch primitive).
    pub(crate) fn read_imm_word<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u16 {
        let word = self.read_word_at(bus, master, self.pc);
        self.pc = self.pc.wrapping_add(2);
        word
    }

    /// The postincrement/predecrement step for `(An)+` / `-(An)`: the
    /// operand size in bytes, except byte-sized accesses through A7 step by
    /// 2 to keep the stack pointer word-aligned.
    #[inline]
    fn step_for(&self, reg: usize, size: Size) -> u32 {
        if reg == 7 && size == Size::Byte {
            2
        } else {
            size.bytes()
        }
    }

    /// Resolve the index portion of a brief extension word (`d8(An,Xn)` /
    /// `d8(PC,Xn)`): Dn or An, sign-extended word or full long, scaled on
    /// 68020+ (the 68000/010 ignore the scale field).
    fn index_value(&self, ext: u16) -> u32 {
        let reg = ((ext >> 12) & 7) as usize;
        let raw = if ext & 0x8000 != 0 {
            self.a[reg]
        } else {
            self.d[reg]
        };
        let index = if ext & 0x0800 != 0 {
            raw
        } else {
            sext16(raw as u16)
        };
        let scale = match self.variant {
            super::M68kVariant::M68000 | super::M68kVariant::M68010 => 0,
            super::M68kVariant::M68020 | super::M68kVariant::M68030 => (ext >> 9) & 3,
        };
        index << scale
    }

    /// Decode a 6-bit effective-address field (`mode`, `reg`) for an operand
    /// of `size`, fetching extension words and applying postincrement /
    /// predecrement side effects.
    pub(crate) fn decode_ea<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Ea {
        let reg = (reg & 7) as usize;
        match mode & 7 {
            // Dn — data register direct
            0 => Ea::DataReg(reg),
            // An — address register direct
            1 => Ea::AddrReg(reg),
            // (An) — address register indirect
            2 => Ea::Mem(self.a[reg]),
            // (An)+ — postincrement: use the current address, then advance
            3 => {
                let addr = self.a[reg];
                self.a[reg] = addr.wrapping_add(self.step_for(reg, size));
                Ea::Mem(addr)
            }
            // -(An) — predecrement: retreat first, then use the new address
            4 => {
                self.a[reg] = self.a[reg].wrapping_sub(self.step_for(reg, size));
                Ea::Mem(self.a[reg])
            }
            // d16(An) — indirect with 16-bit signed displacement
            5 => {
                let disp = sext16(self.read_imm_word(bus, master));
                Ea::Mem(self.a[reg].wrapping_add(disp))
            }
            // d8(An,Xn) — indirect with index register and 8-bit displacement
            6 => {
                let ext = self.read_imm_word(bus, master);
                let addr = self.a[reg]
                    .wrapping_add(sext8(ext as u8))
                    .wrapping_add(self.index_value(ext));
                Ea::Mem(addr)
            }
            // Mode 7 submodes, selected by the register field
            _ => match reg {
                // abs.w — sign-extended 16-bit absolute address
                0 => Ea::Mem(sext16(self.read_imm_word(bus, master))),
                // abs.l — full 32-bit absolute address (two words, high first)
                1 => {
                    let hi = self.read_imm_word(bus, master) as u32;
                    let lo = self.read_imm_word(bus, master) as u32;
                    Ea::Mem((hi << 16) | lo)
                }
                // d16(PC) — PC-relative; base is the extension word address
                2 => {
                    let base = self.pc;
                    let disp = sext16(self.read_imm_word(bus, master));
                    Ea::Mem(base.wrapping_add(disp))
                }
                // d8(PC,Xn) — PC-relative with index; same base convention
                3 => {
                    let base = self.pc;
                    let ext = self.read_imm_word(bus, master);
                    let addr = base
                        .wrapping_add(sext8(ext as u8))
                        .wrapping_add(self.index_value(ext));
                    Ea::Mem(addr)
                }
                // #imm — 1 extension word for byte/word, 2 for long
                4 => {
                    let value = match size {
                        Size::Byte => self.read_imm_word(bus, master) as u32 & 0xFF,
                        Size::Word => self.read_imm_word(bus, master) as u32,
                        Size::Long => {
                            let hi = self.read_imm_word(bus, master) as u32;
                            let lo = self.read_imm_word(bus, master) as u32;
                            (hi << 16) | lo
                        }
                    };
                    Ea::Imm(value)
                }
                // 7.5-7.7 are unassigned on the 68000 (illegal instruction,
                // lands in M5); decode as immediate-zero to stay deterministic
                _ => Ea::Imm(0),
            },
        }
    }

    /// Read an operand of `size` through a resolved [`Ea`]. Register and
    /// immediate operands are masked to the operand size; sign extension is
    /// the consumer's job (MOVEA/ADDA/CMPA).
    pub(crate) fn ea_read<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        ea: Ea,
        size: Size,
    ) -> u32 {
        match ea {
            Ea::DataReg(r) => self.d[r] & size.mask(),
            Ea::AddrReg(r) => {
                debug_assert!(size != Size::Byte, "byte access to An is illegal");
                self.a[r] & size.mask()
            }
            Ea::Mem(addr) => match size {
                Size::Byte => self.read_byte_at(bus, master, addr) as u32,
                Size::Word => self.read_word_at(bus, master, addr) as u32,
                Size::Long => self.read_long_at(bus, master, addr),
            },
            Ea::Imm(v) => v & size.mask(),
        }
    }

    /// Write an operand of `size` through a resolved [`Ea`].
    ///
    /// Byte/word writes to Dn preserve the upper register bits; word writes
    /// to An sign-extend to the full 32 bits (address registers have no
    /// partial-width writes).
    pub(crate) fn ea_write<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        ea: Ea,
        size: Size,
        value: u32,
    ) {
        match ea {
            Ea::DataReg(r) => {
                self.d[r] = (self.d[r] & !size.mask()) | (value & size.mask());
            }
            Ea::AddrReg(r) => {
                debug_assert!(size != Size::Byte, "byte access to An is illegal");
                self.a[r] = match size {
                    Size::Word => sext16(value as u16),
                    _ => value,
                };
            }
            Ea::Mem(addr) => match size {
                Size::Byte => self.write_byte_at(bus, master, addr, value as u8),
                Size::Word => self.write_word_at(bus, master, addr, value as u16),
                Size::Long => self.write_long_at(bus, master, addr, value),
            },
            Ea::Imm(_) => debug_assert!(false, "write to immediate operand"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::WordBus;
    use super::*;

    const M: BusMaster = BusMaster::Cpu(0);

    fn setup() -> (M68000, WordBus) {
        (M68000::new(), WordBus::new())
    }

    // --- Memory access primitives ---

    #[test]
    fn word_access_is_big_endian() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0x12, 0x34]);
        assert_eq!(cpu.read_word_at(&mut bus, M, 0x1000), 0x1234);

        cpu.write_word_at(&mut bus, M, 0x2000, 0xBEEF);
        assert_eq!(&bus.memory[0x2000..0x2002], &[0xBE, 0xEF]);
        assert!(!cpu.took_address_error());
    }

    #[test]
    fn long_access_is_two_words_high_first() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(cpu.read_long_at(&mut bus, M, 0x1000), 0x1234_5678);

        cpu.write_long_at(&mut bus, M, 0x2000, 0xDEAD_BEEF);
        assert_eq!(&bus.memory[0x2000..0x2004], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn byte_read_selects_high_or_low_byte() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0xAB, 0xCD]);
        assert_eq!(cpu.read_byte_at(&mut bus, M, 0x1000), 0xAB, "even = UDS");
        assert_eq!(cpu.read_byte_at(&mut bus, M, 0x1001), 0xCD, "odd = LDS");
        assert!(!cpu.took_address_error(), "byte access is never misaligned");
    }

    #[test]
    fn byte_write_preserves_other_byte_of_word() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0xAB, 0xCD]);
        cpu.write_byte_at(&mut bus, M, 0x1000, 0x11);
        assert_eq!(&bus.memory[0x1000..0x1002], &[0x11, 0xCD]);
        cpu.write_byte_at(&mut bus, M, 0x1001, 0x22);
        assert_eq!(&bus.memory[0x1000..0x1002], &[0x11, 0x22]);
    }

    #[test]
    fn odd_word_access_flags_address_error() {
        let (mut cpu, mut bus) = setup();
        cpu.read_word_at(&mut bus, M, 0x1001);
        assert!(cpu.took_address_error());

        let (mut cpu, mut bus) = setup();
        cpu.write_word_at(&mut bus, M, 0x1001, 0);
        assert!(cpu.took_address_error());

        let (mut cpu, mut bus) = setup();
        cpu.read_long_at(&mut bus, M, 0x1003);
        assert!(cpu.took_address_error());
    }

    #[test]
    fn read_imm_word_advances_pc() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x4E, 0x71]);
        assert_eq!(cpu.read_imm_word(&mut bus, M), 0x4E71);
        assert_eq!(cpu.pc, 0x1002);
    }

    // --- decode_ea: register and indirect modes ---

    #[test]
    fn decode_register_direct_modes() {
        let (mut cpu, mut bus) = setup();
        assert_eq!(cpu.decode_ea(&mut bus, M, 0, 3, Size::Word), Ea::DataReg(3));
        assert_eq!(cpu.decode_ea(&mut bus, M, 1, 5, Size::Long), Ea::AddrReg(5));
    }

    #[test]
    fn decode_address_indirect() {
        let (mut cpu, mut bus) = setup();
        cpu.a[2] = 0x3000;
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 2, 2, Size::Word),
            Ea::Mem(0x3000)
        );
        assert_eq!(cpu.a[2], 0x3000, "plain indirect must not adjust An");
    }

    #[test]
    fn decode_postincrement_steps_by_size() {
        let (mut cpu, mut bus) = setup();
        for (size, step) in [(Size::Byte, 1), (Size::Word, 2), (Size::Long, 4)] {
            cpu.a[1] = 0x3000;
            let ea = cpu.decode_ea(&mut bus, M, 3, 1, size);
            assert_eq!(ea, Ea::Mem(0x3000), "address is pre-increment value");
            assert_eq!(cpu.a[1], 0x3000 + step, "step for {size:?}");
        }
    }

    #[test]
    fn decode_predecrement_steps_by_size() {
        let (mut cpu, mut bus) = setup();
        for (size, step) in [(Size::Byte, 1), (Size::Word, 2), (Size::Long, 4)] {
            cpu.a[1] = 0x3000;
            let ea = cpu.decode_ea(&mut bus, M, 4, 1, size);
            assert_eq!(
                ea,
                Ea::Mem(0x3000 - step),
                "address is post-decrement value"
            );
            assert_eq!(cpu.a[1], 0x3000 - step, "step for {size:?}");
        }
    }

    #[test]
    fn a7_byte_postincrement_and_predecrement_step_by_two() {
        let (mut cpu, mut bus) = setup();
        cpu.a[7] = 0x3000;
        let ea = cpu.decode_ea(&mut bus, M, 3, 7, Size::Byte);
        assert_eq!(ea, Ea::Mem(0x3000));
        assert_eq!(cpu.a[7], 0x3002, "A7 byte (An)+ keeps SP word-aligned");

        cpu.a[7] = 0x3000;
        let ea = cpu.decode_ea(&mut bus, M, 4, 7, Size::Byte);
        assert_eq!(ea, Ea::Mem(0x2FFE));
        assert_eq!(cpu.a[7], 0x2FFE, "A7 byte -(An) keeps SP word-aligned");
    }

    #[test]
    fn decode_displacement_16() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x00, 0x10]); // +0x10
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 5, 0, Size::Word),
            Ea::Mem(0x3010)
        );
        assert_eq!(cpu.pc, 0x1002, "one extension word consumed");

        // Negative displacement
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0xFF, 0xF0]); // -0x10
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 5, 0, Size::Word),
            Ea::Mem(0x2FF0)
        );
    }

    #[test]
    fn decode_indexed_word_index_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.d[2] = 0xFFFF_FFF0; // low word = -0x10 when sign-extended
        cpu.pc = 0x1000;
        // Brief extension: D2.w index (D/A=0, reg=2, W/L=0), disp8 = +4
        bus.load(0x1000, &[0x20, 0x04]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 - 0x10 + 4)
        );
    }

    #[test]
    fn decode_indexed_long_index_uses_full_register() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x0010_0000;
        cpu.d[2] = 0x0000_1000;
        cpu.pc = 0x1000;
        // Brief extension: D2.l index (W/L=1), disp8 = 0
        bus.load(0x1000, &[0x28, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x0010_1000)
        );
    }

    #[test]
    fn decode_indexed_address_register_index() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.a[3] = 0x0000_0100;
        cpu.pc = 0x1000;
        // Brief extension: A3.l index (D/A=1, reg=3, W/L=1), disp8 = -2
        bus.load(0x1000, &[0xB8, 0xFE]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 + 0x100 - 2)
        );
    }

    #[test]
    fn decode_indexed_negative_disp8() {
        let (mut cpu, mut bus) = setup();
        cpu.a[1] = 0x3000;
        cpu.d[0] = 0;
        cpu.pc = 0x1000;
        // D0.w index = 0, disp8 = -0x80 (most negative)
        bus.load(0x1000, &[0x00, 0x80]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 1, Size::Word),
            Ea::Mem(0x3000 - 0x80)
        );
    }

    #[test]
    fn scale_field_ignored_on_68000() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.d[1] = 0x10;
        cpu.pc = 0x1000;
        // D1.l index with scale bits = 3 (×8 on 68020+): 68000 ignores scale
        bus.load(0x1000, &[0x1E, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3010)
        );

        // Same encoding on a 68020 applies the scale
        let (mut cpu, mut bus) = setup();
        cpu.variant = super::super::M68kVariant::M68020;
        cpu.a[0] = 0x3000;
        cpu.d[1] = 0x10;
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x1E, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 + (0x10 << 3))
        );
    }

    // --- decode_ea: mode 7 submodes ---

    #[test]
    fn decode_absolute_short_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x20, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 0, Size::Word),
            Ea::Mem(0x2000)
        );

        // $8000 sign-extends to the full $FFFF8000 (the bus masks to
        // $FF8000 on access; JMP/LEA would see all 32 bits)
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x80, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 0, Size::Word),
            Ea::Mem(0xFFFF_8000)
        );
    }

    #[test]
    fn decode_absolute_long() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x00, 0x12, 0x34, 0x56]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 1, Size::Word),
            Ea::Mem(0x0012_3456)
        );
        assert_eq!(cpu.pc, 0x1004, "two extension words consumed");
    }

    #[test]
    fn decode_pc_relative_base_is_extension_word_address() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000; // extension word lives here
        bus.load(0x1000, &[0x01, 0x00]); // +0x100
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 2, Size::Word),
            Ea::Mem(0x1100)
        );
    }

    #[test]
    fn decode_pc_indexed() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000;
        cpu.d[4] = 0x20;
        // Brief extension: D4.l index, disp8 = +6; base = 0x1000
        bus.load(0x1000, &[0x48, 0x06]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 3, Size::Word),
            Ea::Mem(0x1026)
        );
    }

    #[test]
    fn decode_immediate_by_size() {
        let (mut cpu, mut bus) = setup();
        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x12, 0x34]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Byte),
            Ea::Imm(0x34),
            "byte immediate is the low byte of one extension word"
        );
        assert_eq!(cpu.pc, 0x1002);

        cpu.pc = 0x1000;
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Word),
            Ea::Imm(0x1234)
        );

        cpu.pc = 0x1000;
        bus.load(0x1000, &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Long),
            Ea::Imm(0x1234_5678)
        );
        assert_eq!(cpu.pc, 0x1004);
    }

    #[test]
    fn decode_keeps_full_32_bit_address_and_bus_access_masks() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0xFF12_3456;
        let ea = cpu.decode_ea(&mut bus, M, 2, 0, Size::Word);
        assert_eq!(ea, Ea::Mem(0xFF12_3456), "EA computed at full width");
        // mask_addr (separately unit-tested) truncates to 24 bits at the
        // word/byte access layer, so reads through the full-width EA work.
        cpu.ea_write(&mut bus, M, ea, Size::Word, 0xBEEF);
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Word), 0xBEEF);
    }

    // --- ea_read / ea_write ---

    #[test]
    fn ea_read_data_register_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        cpu.d[1] = 0x8765_4321;
        assert_eq!(cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Byte), 0x21);
        assert_eq!(cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Word), 0x4321);
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Long),
            0x8765_4321
        );
    }

    #[test]
    fn ea_read_address_register_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        cpu.a[2] = 0x8765_4321;
        assert_eq!(cpu.ea_read(&mut bus, M, Ea::AddrReg(2), Size::Word), 0x4321);
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::AddrReg(2), Size::Long),
            0x8765_4321
        );
    }

    #[test]
    fn ea_write_data_register_preserves_upper_bits() {
        let (mut cpu, mut bus) = setup();
        cpu.d[3] = 0xAABB_CCDD;
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Byte, 0x11);
        assert_eq!(cpu.d[3], 0xAABB_CC11);
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Word, 0x2222);
        assert_eq!(cpu.d[3], 0xAABB_2222);
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Long, 0x3333_3333);
        assert_eq!(cpu.d[3], 0x3333_3333);
    }

    #[test]
    fn ea_write_address_register_word_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.a[4] = 0xAABB_CCDD;
        cpu.ea_write(&mut bus, M, Ea::AddrReg(4), Size::Word, 0x8000);
        assert_eq!(cpu.a[4], 0xFFFF_8000, "negative word fills upper bits");
        cpu.ea_write(&mut bus, M, Ea::AddrReg(4), Size::Word, 0x7FFF);
        assert_eq!(cpu.a[4], 0x0000_7FFF, "positive word clears upper bits");
    }

    #[test]
    fn ea_memory_round_trip_all_sizes() {
        let (mut cpu, mut bus) = setup();
        let ea = Ea::Mem(0x4000);

        cpu.ea_write(&mut bus, M, ea, Size::Long, 0x1122_3344);
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Long), 0x1122_3344);
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Word), 0x1122);
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Byte), 0x11);

        cpu.ea_write(&mut bus, M, Ea::Mem(0x4001), Size::Byte, 0xFF);
        assert_eq!(
            cpu.ea_read(&mut bus, M, ea, Size::Long),
            0x11FF_3344,
            "byte write into the middle of the long"
        );
    }

    #[test]
    fn ea_read_immediate_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::Imm(0x1234_5678), Size::Byte),
            0x78
        );
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::Imm(0x1234_5678), Size::Word),
            0x5678
        );
    }

    #[test]
    fn size_helpers() {
        assert_eq!(Size::Byte.bytes(), 1);
        assert_eq!(Size::Word.bytes(), 2);
        assert_eq!(Size::Long.bytes(), 4);
        assert_eq!(Size::Byte.mask(), 0xFF);
        assert_eq!(Size::Word.mask(), 0xFFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);
        assert_eq!(Size::Byte.sign_bit(), 0x80);
        assert_eq!(Size::Word.sign_bit(), 0x8000);
        assert_eq!(Size::Long.sign_bit(), 0x8000_0000);
    }
}
