//! Binary ALU: ADD/SUB/CMP, AND/OR/EOR, their immediate forms, and TST.
//!
//! Lines 0x9 (SUB) and 0xD (ADD) share one encoding: `rrr ooo mmm RRR` with
//! opmode `ooo` selecting direction and size — 000-010 `Dn ⟵ Dn op <ea>`,
//! 100-110 `<ea> ⟵ <ea> op Dn`, 011/111 the address-register forms
//! (ADDA/SUBA word/long). Lines 0x8 (OR) and 0xC (AND) use the same shape
//! minus the address-register forms; line 0xB carries CMP (opmodes 000-010),
//! CMPA (011/111), and EOR (100-110, destination form only). The immediate
//! forms live on line 0x0 with the literal fetched ahead of the destination
//! EA, and TST lives on line 0x4.
//!
//! Opmodes 100-110 with an EA mode of Dn/An encode the extended-arithmetic
//! instructions (ADDX/SUBX/ABCD/SBCD, CMPM, EXG, MULx/DIVx on opmodes
//! 011/111 of lines 0x8/0xC); `execute_instruction` routes those before the
//! handlers here run.

use super::super::M68000;
use super::super::addressing::{AccessResult, Ea, Size, ea_internal, sext16};
use super::super::flags::SrFlag;
use crate::core::{Bus16, BusMaster};

/// Whether an addressing mode fetches its operand from memory.
///
/// Data and address register direct do not, and neither does an immediate: its
/// words come out of the prefetch queue, which was filled before the
/// instruction started. Everything else runs a data bus cycle to get its value.
pub(crate) fn operand_from_memory(mode: u8, reg: u8) -> bool {
    !matches!((mode & 7, reg & 7), (0, _) | (1, _) | (7, 4))
}

/// Internal time for a `Dn ⟵ Dn op <ea>` form that *stores* its result.
///
/// The ALU is 16 bits wide, so a long operation is two passes: the low word,
/// then the high word with carry. What varies is whether the high-word pass
/// gets a step of its own, and that is decided by where the operand came from.
///
/// - **Operand from memory**: the two operand reads occupy their own cycles,
///   the low pass runs free alongside them, and the high pass shares its step
///   with the prefetch. Two clocks are left over, for the step that places the
///   result.
/// - **Operand from a register or the queue**: there are no operand reads, the
///   prefetch runs on its own step, and the high pass then needs a step of its
///   own before the result can be placed on the step after. Four clocks.
///
/// A word operation has one pass and neither case costs anything.
///
/// **`CMP` is the control that makes this a mechanism rather than a fitted
/// number.** It runs the same two passes over the same operands and has no
/// result to place, so its high pass shares the prefetch's step whichever way
/// the operand arrived, and it costs two clocks either way. That is why it has
/// [`cmp_form_internal`] rather than sharing this one. `EOR`'s register
/// destination in `op_logical` reached the same four independently, and before
/// any of this was measured.
fn src_form_internal(size: Size, mode: u8, reg: u8) -> u32 {
    let alu = match (size, operand_from_memory(mode, reg)) {
        (Size::Long, true) => 2,
        (Size::Long, false) => 4,
        _ => 0,
    };
    alu + ea_internal(mode, reg)
}

/// Internal time for `CMP`'s source form, which discards its result.
///
/// Two clocks at long whatever the operand, where [`src_form_internal`] pays
/// four for an operand that is not from memory. Having no result to place,
/// the comparison folds its high-word pass into the same step as the prefetch
/// in both cases, so the step that separates them never appears. Measured as
/// well as read: `CMP.l D2,D3` is six clocks and `ADD.l D2,D3` is eight, one
/// bus transfer each.
fn cmp_form_internal(size: Size, mode: u8, reg: u8) -> u32 {
    (if size == Size::Long { 2 } else { 0 }) + ea_internal(mode, reg)
}

/// Decode the two-bit size field used by opmodes and immediates
/// (00 = byte, 01 = word, 10 = long; 11 is never a size).
pub(crate) fn size_from_bits(bits: u16) -> Option<Size> {
    match bits & 3 {
        0 => Some(Size::Byte),
        1 => Some(Size::Word),
        2 => Some(Size::Long),
        _ => None,
    }
}

/// Which bitwise operation a logical instruction performs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LogicalOp {
    And,
    Or,
    Eor,
}

impl LogicalOp {
    fn apply(self, a: u32, b: u32) -> u32 {
        match self {
            LogicalOp::And => a & b,
            LogicalOp::Or => a | b,
            LogicalOp::Eor => a ^ b,
        }
    }
}

impl M68000 {
    /// ADD (line 0xD) and SUB (line 0x9), including the ADDA/SUBA opmodes.
    ///
    /// Flags: N/Z/V/C from the sized result and **X = C** (arithmetic rule).
    /// ADDA/SUBA set no flags at all and operate on the full 32-bit address
    /// register after sign-extending a word operand.
    pub(crate) fn op_add_sub<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        is_add: bool,
    ) -> AccessResult<()> {
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
                    self.finish_from_bus(bus, master, 0);
                    return Ok(());
                }
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let b = self.ea_read(bus, master, src, size)?;
                let a = self.d[dn];
                let result = if is_add {
                    self.add_with_flags(size, a, b)
                } else {
                    self.sub_with_flags(size, a, b)
                };
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.d[dn] = (a & !size.mask()) | result;

                self.finish_from_bus(bus, master, src_form_internal(size, ea_mode, ea_reg));
            }
            // <ea> ⟵ <ea> op Dn (memory-alterable destinations only;
            // Dn/An here encode ADDX/SUBX, routed by the caller)
            4..=6 => {
                let size = size_from_bits(opmode).unwrap();
                if ea_mode < 2 || (ea_mode == 7 && ea_reg >= 2) {
                    self.finish_from_bus(bus, master, 0); // illegal destination
                    return Ok(());
                }
                let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let a = self.ea_read(bus, master, dst, size)?;
                let b = self.d[dn];
                let result = if is_add {
                    self.add_with_flags(size, a, b)
                } else {
                    self.sub_with_flags(size, a, b)
                };
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write_rmw(bus, master, dst, size, result)?;

                // The read and the write are both counted transfers, so this
                // form declares only the mode's own address arithmetic.
                self.finish_from_bus(bus, master, ea_internal(ea_mode, ea_reg));
            }
            // ADDA/SUBA: An ⟵ An op <ea> (word sign-extends, no flags)
            _ => {
                let size = if opmode == 3 { Size::Word } else { Size::Long };
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let value = self.ea_read(bus, master, src, size)?;
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

                // ADDA/SUBA hold the full 32-bit register whatever the operand
                // size, so both forms run the long two-pass add and store it.
                //
                // A word source pays four regardless: it is sign-extended to 32
                // bits before the add, and its single bus cycle has only one
                // pass to hide. A long source is [`src_form_internal`]'s rule
                // exactly, and for the same reason: two clocks when the operand
                // came from memory as two bus cycles, four when it was already
                // in a register or the queue.
                let alu = if size == Size::Long && operand_from_memory(ea_mode, ea_reg) {
                    2
                } else {
                    4
                };
                self.finish_from_bus(bus, master, alu + ea_internal(ea_mode, ea_reg));
            }
        }
        Ok(())
    }

    /// CMP (line 0xB, opmodes 000-010) and CMPA (011/111).
    ///
    /// Flags: N/Z/V/C from `dst - src`; the result is discarded and **X is
    /// never altered** (the one arithmetic op that leaves X alone). CMPA
    /// sign-extends a word operand and compares the full 32-bit An.
    /// Opmodes 100-110 encode CMPM/EOR and land in M2.
    pub(crate) fn op_cmp<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let dn = ((opcode >> 9) & 7) as usize;
        let opmode = (opcode >> 6) & 7;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        match opmode {
            0..=2 => {
                let size = size_from_bits(opmode).unwrap();
                if size == Size::Byte && ea_mode == 1 {
                    self.finish_from_bus(bus, master, 0); // byte read from An is illegal
                    return Ok(());
                }
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let b = self.ea_read(bus, master, src, size)?;
                self.sub_with_flags(size, self.d[dn], b);

                self.finish_from_bus(bus, master, cmp_form_internal(size, ea_mode, ea_reg));
            }
            3 | 7 => {
                let size = if opmode == 3 { Size::Word } else { Size::Long };
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let value = self.ea_read(bus, master, src, size)?;
                let value = if size == Size::Word {
                    sext16(value as u16)
                } else {
                    value
                };
                self.sub_with_flags(Size::Long, self.a[dn], value);

                // CMPA compares at the full width whatever the source size, and
                // pays two clocks for that pass however the operand arrived.
                self.finish_from_bus(bus, master, 2 + ea_internal(ea_mode, ea_reg));
            }
            // CMPM / EOR — routed by execute_instruction before this runs
            _ => self.finish_from_bus(bus, master, 0),
        }
        Ok(())
    }

    /// AND (line 0xC) and OR (line 0x8) in both directions, and EOR
    /// (line 0xB, destination form only — its source-form opmodes encode
    /// CMP). The caller routes the MULx/DIVx/ABCD/SBCD/EXG/CMPM encodings
    /// that share these lines before calling here.
    ///
    /// Flags: N/Z from the sized result, V/C cleared, **X untouched**
    /// (the logical rule).
    pub(crate) fn op_logical<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        op: LogicalOp,
    ) -> AccessResult<()> {
        let dn = ((opcode >> 9) & 7) as usize;
        let opmode = (opcode >> 6) & 7;
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        match opmode {
            // Dn ⟵ Dn op <ea> (data addressing only; An is illegal)
            0..=2 => {
                let size = size_from_bits(opmode).unwrap();
                if ea_mode == 1 {
                    self.finish_from_bus(bus, master, 0);
                    return Ok(());
                }
                let src = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let b = self.ea_read(bus, master, src, size)?;
                let result = op.apply(self.d[dn], b) & size.mask();
                self.set_flags_logical(size, result);
                self.d[dn] = (self.d[dn] & !size.mask()) | result;

                self.finish_from_bus(bus, master, src_form_internal(size, ea_mode, ea_reg));
            }
            // <ea> ⟵ <ea> op Dn. Only EOR allows a Dn destination here
            // (AND/OR register destinations encode ABCD/SBCD/EXG and are
            // routed away); An and PC-relative destinations are illegal.
            4..=6 => {
                let size = size_from_bits(opmode).unwrap();
                let dn_dest_ok = op == LogicalOp::Eor && ea_mode == 0;
                if (ea_mode < 2 && !dn_dest_ok) || (ea_mode == 7 && ea_reg >= 2) {
                    self.finish_from_bus(bus, master, 0);
                    return Ok(());
                }
                let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
                let a = self.ea_read(bus, master, dst, size)?;
                let result = op.apply(a, self.d[dn]) & size.mask();
                self.set_flags_logical(size, result);
                self.ea_write_rmw(bus, master, dst, size, result)?;

                // EOR is the one logical instruction with a register
                // destination, and it makes no operand transfer at all there:
                // its whole cost beyond the opcode fetch is the ALU pass, four
                // clocks at long and none at word. A memory destination reads
                // and writes, and both are counted.
                let internal = if ea_mode == 0 {
                    if size == Size::Long { 4 } else { 0 }
                } else {
                    ea_internal(ea_mode, ea_reg)
                };
                self.finish_from_bus(bus, master, internal);
            }
            // Opmodes 011/111 (MULx/DIVx) are routed by the caller
            _ => self.finish_from_bus(bus, master, 0),
        }
        Ok(())
    }

    /// ADDX (line 0xD) and SUBX (line 0x9), opmodes 100-110 with an EA mode
    /// of Dn (register form) or An (`-(Ay),-(Ax)` memory form).
    ///
    /// Flags: extended-arithmetic rule — X is consumed as carry/borrow-in
    /// and set to C on the way out; Z is cleared by a non-zero result but
    /// never set (multi-precision chains report zero only if every limb
    /// was zero).
    pub(crate) fn op_addx_subx<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        is_add: bool,
    ) -> AccessResult<()> {
        let rx = ((opcode >> 9) & 7) as u8; // destination
        let ry = (opcode & 7) as u8; // source
        let size = size_from_bits(opcode >> 6).unwrap();
        let mem = opcode & 0x0008 != 0;

        if mem {
            // Source predecrements first, then the destination.
            let b = self.addx_predec_read(bus, master, ry as usize, size)?;
            let a = self.addx_predec_read(bus, master, rx as usize, size)?;
            let result = if is_add {
                self.addx_with_flags(size, a, b)
            } else {
                self.subx_with_flags(size, a, b)
            };
            let dst = Ea::Mem(self.a[rx as usize]);
            // The refill goes between the two words of a long result here, not
            // in front of both: see [`Self::ea_write_rmw_refill_between`].
            self.ea_write_rmw_refill_between(bus, master, dst, size, result)?;
            // Two operand reads and a write, all counted. The two clocks left
            // are the predecrement, charged once however wide the operands are.
            self.finish_from_bus(bus, master, 2);
        } else {
            let a = self.d[rx as usize];
            let b = self.d[ry as usize];
            let result = if is_add {
                self.addx_with_flags(size, a, b)
            } else {
                self.subx_with_flags(size, a, b)
            };
            self.d[rx as usize] = (a & !size.mask()) | result;
            // Registers only: the opcode fetch is the whole bus cost, and a
            // long pass adds four clocks the word one does not.
            self.finish_from_bus(bus, master, if size == Size::Long { 4 } else { 0 });
        }
        Ok(())
    }

    /// Read the `-(An)` operand of the extended-arithmetic memory forms.
    /// ADDX/SUBX read long operands low word first, stepping An by 2 at a
    /// time, so a fault on an odd address leaves An decremented by only 2
    /// (hardware-verified; ordinary predecrement EAs like CLR.l move An by
    /// the full operand size before the high-first access).
    fn addx_predec_read<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        reg: usize,
        size: Size,
    ) -> AccessResult<u32> {
        if size == Size::Long {
            self.a[reg] = self.a[reg].wrapping_sub(2);
            let lo = self.read_word_at(bus, master, self.a[reg])?;
            self.a[reg] = self.a[reg].wrapping_sub(2);
            let hi = self.read_word_at(bus, master, self.a[reg])?;
            Ok(((hi as u32) << 16) | lo as u32)
        } else {
            let ea = self.decode_ea(bus, master, 4, reg as u8, size)?;
            self.ea_read(bus, master, ea, size)
        }
    }

    /// CMPM (Ay)+,(Ax)+ — line 0xB, opmodes 100-110 with EA mode An.
    ///
    /// Flags: N/Z/V/C from `dst - src`; the result is discarded and **X is
    /// never altered** (the CMP rule, not the extended rule).
    pub(crate) fn op_cmpm<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ax = ((opcode >> 9) & 7) as u8;
        let ay = (opcode & 7) as u8;
        let size = size_from_bits(opcode >> 6).unwrap();

        // Source postincrements first, then the destination.
        let src = self.decode_ea(bus, master, 3, ay, size)?;
        let b = self.ea_read(bus, master, src, size)?;
        let dst = self.decode_ea(bus, master, 3, ax, size)?;
        let a = self.ea_read(bus, master, dst, size)?;
        self.sub_with_flags(size, a, b);
        // Two postincrement reads and nothing else: no write, and no address
        // arithmetic off the bus, so the transfers are the whole cost.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// Shared ABCD core: BCD-add `src + dst + X`, modeling the hardware's
    /// per-nibble correction adder exactly (verified against the
    /// SingleStepTests vectors, including the undefined N/V/C behavior):
    ///
    /// - `bc` collects the binary carries out of bits 3 and 7, `dc` the
    ///   decimal carries (nibble > 9); each carried nibble gets a +6
    ///   correction.
    /// - C/X are set by a binary carry or by the correction overflowing
    ///   bit 7; V is set when the correction flips bit 7 from 0 to 1.
    /// - N comes from the corrected result; Z follows the multi-precision
    ///   rule (cleared by a non-zero result, never set).
    fn abcd_core(&mut self, src: u32, dst: u32) -> u32 {
        let x = self.flag_is_set(SrFlag::X) as u32;
        let (src, dst) = (src & 0xFF, dst & 0xFF);
        let simple = src + dst + x;
        let bc = ((src & dst) | (!simple & dst) | (!simple & src)) & 0x88;
        let dc = ((simple + 0x66) ^ simple) & 0x110;
        let corf = (bc | (dc >> 1)) - ((bc | (dc >> 1)) >> 2);
        let res = simple + corf;

        let carry = (bc | (simple & !res)) & 0x80 != 0;
        self.set_flag(SrFlag::C, carry);
        self.set_flag(SrFlag::X, carry);
        self.set_flag(SrFlag::V, !simple & res & 0x80 != 0);
        self.set_flag(SrFlag::N, res & 0x80 != 0);

        let res = res & 0xFF;
        if res != 0 {
            self.set_flag(SrFlag::Z, false);
        }
        res
    }

    /// Shared SBCD/NBCD core: BCD-subtract `dst - src - X`, modeling the
    /// hardware's per-nibble correction exactly (verified against the
    /// SingleStepTests vectors): each nibble that borrowed in the binary
    /// subtraction gets a -6 correction. C/X are set by a binary borrow or
    /// by the correction flipping bit 7 from 0 to 1; V is set when the
    /// correction flips bit 7 from 1 to 0. N/Z as in [`Self::abcd_core`].
    fn sbcd_core(&mut self, src: u32, dst: u32) -> u32 {
        let x = self.flag_is_set(SrFlag::X) as u32;
        let (src, dst) = (src & 0xFF, dst & 0xFF);
        let simple = dst.wrapping_sub(src).wrapping_sub(x);
        let bc = ((!dst & src) | (simple & !dst) | (simple & src)) & 0x88;
        let corf = bc - (bc >> 2);
        let res = simple.wrapping_sub(corf);

        let borrow = (bc | (!simple & res)) & 0x80 != 0;
        self.set_flag(SrFlag::C, borrow);
        self.set_flag(SrFlag::X, borrow);
        self.set_flag(SrFlag::V, simple & !res & 0x80 != 0);
        self.set_flag(SrFlag::N, res & 0x80 != 0);

        let res = res & 0xFF;
        if res != 0 {
            self.set_flag(SrFlag::Z, false);
        }
        res
    }

    /// ABCD (line 0xC) and SBCD (line 0x8), opmode 100 with an EA mode of
    /// Dn (register form) or An (`-(Ay),-(Ax)` memory form). Byte only.
    ///
    /// Flags: extended rule (X consumed and set to decimal carry/borrow,
    /// Z never set); N and V follow the hardware's undefined behavior.
    pub(crate) fn op_bcd<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        is_add: bool,
    ) -> AccessResult<()> {
        let rx = ((opcode >> 9) & 7) as u8;
        let ry = (opcode & 7) as u8;
        let mem = opcode & 0x0008 != 0;

        if mem {
            let src = self.decode_ea(bus, master, 4, ry, Size::Byte)?;
            let b = self.ea_read(bus, master, src, Size::Byte)?;
            let dst = self.decode_ea(bus, master, 4, rx, Size::Byte)?;
            let a = self.ea_read(bus, master, dst, Size::Byte)?;
            let result = if is_add {
                self.abcd_core(b, a)
            } else {
                self.sbcd_core(b, a)
            };
            self.ea_write_rmw(bus, master, dst, Size::Byte, result)?;
            // Two reads and a write, plus the predecrement's two clocks.
            self.finish_from_bus(bus, master, 2);
        } else {
            let a = self.d[rx as usize];
            let b = self.d[ry as usize];
            let result = if is_add {
                self.abcd_core(b, a)
            } else {
                self.sbcd_core(b, a)
            };
            self.d[rx as usize] = (a & !0xFF) | result;
            // Registers only: the opcode fetch, plus two clocks in the decimal
            // correction adder.
            self.finish_from_bus(bus, master, 2);
        }
        Ok(())
    }

    /// NBCD <ea> (line 0x4, 0x4800): BCD-negate the operand — `0 - dst - X`
    /// in decimal, implemented as SBCD with a zero destination. Byte only,
    /// data-alterable EA.
    ///
    /// Flags: same extended/undefined rules as SBCD.
    pub(crate) fn op_nbcd<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Byte)?;
        let operand = self.ea_read(bus, master, ea, Size::Byte)?;
        let result = self.sbcd_core(operand, 0);
        self.ea_write_rmw(bus, master, ea, Size::Byte, result)?;

        // A register destination makes no operand transfer, so its two clocks
        // in the correction adder are all that is left; a memory one reads and
        // writes, both counted, leaving only the mode's address arithmetic.
        let internal = if ea_mode == 0 {
            2
        } else {
            ea_internal(ea_mode, ea_reg)
        };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }

    /// TST <ea> (line 0x4, sub-op 0xA, sizes 00-10): read the operand and
    /// set the condition codes; nothing is written. On the 68000 the operand
    /// must be data-alterable (An, PC-relative, and immediate are illegal).
    ///
    /// Flags: N/Z from the operand, V/C cleared, **X untouched**.
    pub(crate) fn op_tst<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let Some(size) = size_from_bits(opcode >> 6) else {
            self.finish_from_bus(bus, master, 0); // TAS / ILLEGAL are routed by the caller
            return Ok(());
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            self.finish_from_bus(bus, master, 0);
            return Ok(());
        }
        let ea = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
        let value = self.ea_read(bus, master, ea, size)?;
        self.set_flags_logical(size, value);
        // TST reads and sets flags: one operand transfer, no write, nothing
        // off the bus beyond the mode's own address arithmetic.
        self.finish_from_bus(bus, master, ea_internal(ea_mode, ea_reg));
        Ok(())
    }

    /// ORI / ANDI / SUBI / ADDI / EORI / CMPI (line 0x0, sub-ops
    /// 0x0/0x2/0x4/0x6/0xA/0xC): immediate literal first, then a
    /// data-alterable destination EA.
    ///
    /// Flags: ADDI/SUBI follow the arithmetic rule (N/Z/V/C and X = C);
    /// ORI/ANDI/EORI follow the logical rule (N/Z, V/C cleared, X
    /// untouched); CMPI sets N/Z/V/C only and leaves X untouched.
    ///
    /// Returns `Ok(false)` if the opcode is not one of the immediate forms
    /// handled here (the to-CCR/to-SR variants and bit ops are routed by
    /// the caller).
    pub(crate) fn op_imm_alu<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<bool> {
        let op = opcode & 0x0F00;
        if !matches!(op, 0x0000 | 0x0200 | 0x0400 | 0x0600 | 0x0A00 | 0x0C00) {
            return Ok(false);
        }
        let Some(size) = size_from_bits(opcode >> 6) else {
            return Ok(false);
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Destination must be data-alterable: An, PC-relative, and immediate
        // destinations are illegal encodings.
        if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 2) {
            return Ok(false);
        }

        // Immediate data precedes the destination extension words, so the
        // destination's address arithmetic runs *after* that fetch rather than
        // in front of the instruction: the loader has not burned it, and an
        // aborted operand access has still spent it.
        let imm = self.decode_ea(bus, master, 7, 4, size)?;
        let b = self.ea_read(bus, master, imm, size)?;
        self.spend_internal(ea_internal(ea_mode, ea_reg));
        let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
        let a = self.ea_read(bus, master, dst, size)?;

        let mem = ea_mode != 0;
        let long = size == Size::Long;
        match op {
            0x0600 => {
                // ADDI
                let result = self.add_with_flags(size, a, b);
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write_rmw(bus, master, dst, size, result)?;
            }
            0x0400 => {
                // SUBI
                let result = self.sub_with_flags(size, a, b);
                self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
                self.ea_write_rmw(bus, master, dst, size, result)?;
            }
            0x0C00 => {
                // CMPI — result discarded, X untouched
                self.sub_with_flags(size, a, b);
            }
            _ => {
                // ORI / ANDI / EORI — logical rule, X untouched
                let logical = match op {
                    0x0000 => LogicalOp::Or,
                    0x0200 => LogicalOp::And,
                    _ => LogicalOp::Eor,
                };
                let result = logical.apply(a, b) & size.mask();
                self.set_flags_logical(size, result);
                self.ea_write_rmw(bus, master, dst, size, result)?;
            }
        }

        // The immediate's extension words and the destination's accesses are
        // all counted transfers. What is left is the ALU pass, and it shows
        // only at long size with a register destination: CMPI discards its
        // result and pays two clocks, the rest write theirs back and pay four.
        // A memory destination hides the pass inside the transfers it is
        // already making, so nothing is left but the mode's own arithmetic.
        let internal = if mem {
            ea_internal(ea_mode, ea_reg)
        } else if !long {
            0
        } else if op == 0x0C00 {
            2
        } else {
            4
        };
        self.finish_from_bus(bus, master, internal);
        Ok(true)
    }

    /// ADDQ/SUBQ #d,<ea> (line 0x5, sizes 00-10; bit 8 selects SUBQ): add
    /// or subtract a literal 1-8 (0 encodes 8) at any alterable destination.
    ///
    /// Flags: N/Z/V/C from the sized result with **X = C** (arithmetic
    /// rule). An An destination behaves like ADDA/SUBA: the full 32-bit
    /// register is adjusted regardless of the word/long size bits and no
    /// flags are set (byte size with An is an illegal encoding).
    pub(crate) fn op_addq_subq<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let size = size_from_bits(opcode >> 6).expect("size 11 is Scc/DBcc");
        let is_sub = opcode & 0x0100 != 0;
        let data = match (opcode >> 9) & 7 {
            0 => 8,
            n => n as u32,
        };
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        if ea_mode == 1 {
            if size == Size::Byte {
                self.finish_from_bus(bus, master, 0); // ADDQ.b to An is illegal
                return Ok(());
            }
            let reg = ea_reg as usize;
            self.a[reg] = if is_sub {
                self.a[reg].wrapping_sub(data)
            } else {
                self.a[reg].wrapping_add(data)
            };
            // An destination: no operand transfer, and the full-width add costs
            // four clocks off the bus whichever size the opcode names.
            //
            // The word and long encodings run the identical sequence on this
            // part: a low-word add, the prefetch, a high-word add with carry,
            // and the register write, the last two costing two clocks each.
            // Nothing in it is conditioned on the size bits, because ADDQ and
            // SUBQ to an address register always operate on the full 32 bits.
            //
            // THE TWO CORPORA DISAGREE HERE AND THIS FOLLOWS THE SEQUENCE. The
            // documentation-derived set records the long form at six clocks
            // rather than eight, on 380 ADDQ and 351 SUBQ cases, every one of
            // them short by exactly two. The microcode-derived set records
            // eight, agrees with this core on every such case, and matches the
            // part's own sequence above. Two clocks were nearly taken off this
            // line on the strength of a residual that was uniform, large and
            // one-sided, which is what a wrong constant looks like; it took the
            // third source to show the constant was right and the corpus wrong.
            self.finish_from_bus(bus, master, 4);
            return Ok(());
        }
        if ea_mode == 7 && ea_reg >= 2 {
            self.finish_from_bus(bus, master, 0); // PC-relative/immediate destinations are illegal
            return Ok(());
        }

        let dst = self.decode_ea(bus, master, ea_mode, ea_reg, size)?;
        let a = self.ea_read(bus, master, dst, size)?;
        let result = if is_sub {
            self.sub_with_flags(size, a, data)
        } else {
            self.add_with_flags(size, a, data)
        };
        self.set_flag(SrFlag::X, self.flag_is_set(SrFlag::C));
        self.ea_write_rmw(bus, master, dst, size, result)?;

        // A register destination makes no operand transfer, so only the long
        // ALU pass shows; a memory one reads and writes, both counted, leaving
        // the mode's address arithmetic.
        let internal = if ea_mode == 0 {
            if size == Size::Long { 4 } else { 0 }
        } else {
            ea_internal(ea_mode, ea_reg)
        };
        self.finish_from_bus(bus, master, internal);
        Ok(())
    }
}
