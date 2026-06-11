//! Intel 8088 instruction execution.
//!
//! Main opcode dispatch and instruction implementations for data transfer
//! instructions. Future steps add ALU, control flow, shifts, string ops,
//! interrupts, and I/O.

use super::addressing::Operand;
use super::alu;
use super::flags::{self, Flag};
use super::registers::SegReg;
use super::{ExecState, I8088, RepPrefix};
use crate::core::{Bus, BusMaster};

impl I8088 {
    /// Dispatch and execute a single instruction given its opcode byte.
    /// The opcode has already been fetched; IP points to the first operand
    /// byte (ModR/M, immediate, or displacement).
    // AAM/DIV/IDIV keep explicit zero checks (not checked_div) because the
    // zero branch raises the 8088 divide error and the quotient still needs
    // a separate range check, matching the datasheet structure.
    #[allow(clippy::manual_checked_ops)]
    pub(crate) fn execute<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match opcode {
            // =============================================================
            // 0x00-0x3F: ALU (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) + segment
            // push/pop + BCD adjust
            //
            // Layout per group of 8 opcodes (bits 5:3 = operation):
            //   +0: ALU r/m8, reg8    +1: ALU r/m16, reg16
            //   +2: ALU reg8, r/m8    +3: ALU reg16, r/m16
            //   +4: ALU AL, imm8      +5: ALU AX, imm16
            //   +6: PUSH/POP seg      +7: POP seg / BCD adjust
            // =============================================================
            0x00..=0x05 => self.alu_dispatch(opcode, 0, bus, master),
            0x06 => {
                let val = self.get_seg(SegReg::ES);
                self.push16(bus, master, val);
            }
            0x07 => {
                let val = self.pop16(bus, master);
                self.es = val;
            }
            0x08..=0x0D => self.alu_dispatch(opcode, 1, bus, master),
            0x0E => {
                let val = self.get_seg(SegReg::CS);
                self.push16(bus, master, val);
            }
            0x0F => {
                let val = self.pop16(bus, master);
                self.cs = val;
            }
            0x10..=0x15 => self.alu_dispatch(opcode, 2, bus, master),
            0x16 => {
                let val = self.get_seg(SegReg::SS);
                self.push16(bus, master, val);
            }
            0x17 => {
                let val = self.pop16(bus, master);
                self.ss = val;
            }
            0x18..=0x1D => self.alu_dispatch(opcode, 3, bus, master),
            0x1E => {
                let val = self.get_seg(SegReg::DS);
                self.push16(bus, master, val);
            }
            0x1F => {
                let val = self.pop16(bus, master);
                self.ds = val;
            }
            0x20..=0x25 => self.alu_dispatch(opcode, 4, bus, master),
            // 0x26 = ES: prefix (consumed by consume_prefixes)
            0x27 => {
                // DAA: Decimal Adjust AL after Addition
                let old_al = self.al();
                let old_cf = flags::get(self.flags, Flag::CF);
                let old_af = flags::get(self.flags, Flag::AF);
                flags::set(&mut self.flags, Flag::CF, false);
                let mut al = old_al;
                if (al & 0x0F) > 9 || old_af {
                    let (new_al, carry) = al.overflowing_add(6);
                    al = new_al;
                    flags::set(&mut self.flags, Flag::CF, old_cf || carry);
                    flags::set(&mut self.flags, Flag::AF, true);
                } else {
                    flags::set(&mut self.flags, Flag::AF, false);
                }
                // 8088 quirk: threshold is > 0x9F when AF was set and low nibble > 9
                if old_cf || old_al > 0x9F || (old_al > 0x99 && !old_af) {
                    al = al.wrapping_add(0x60);
                    flags::set(&mut self.flags, Flag::CF, true);
                }
                self.set_al(al);
                flags::update_szp8(&mut self.flags, al);
            }
            0x28..=0x2D => self.alu_dispatch(opcode, 5, bus, master),
            // 0x2E = CS: prefix (consumed by consume_prefixes)
            0x2F => {
                // DAS: Decimal Adjust AL after Subtraction
                let old_al = self.al();
                let old_cf = flags::get(self.flags, Flag::CF);
                let old_af = flags::get(self.flags, Flag::AF);
                let mut al = old_al;
                if (al & 0x0F) > 9 || old_af {
                    al = al.wrapping_sub(6);
                    flags::set(&mut self.flags, Flag::AF, true);
                } else {
                    flags::set(&mut self.flags, Flag::AF, false);
                }
                // CF set only by the high nibble check (borrow from low adj ignored)
                if old_cf || old_al > 0x9F || (old_al > 0x99 && !old_af) {
                    al = al.wrapping_sub(0x60);
                    flags::set(&mut self.flags, Flag::CF, true);
                } else {
                    flags::set(&mut self.flags, Flag::CF, false);
                }
                self.set_al(al);
                flags::update_szp8(&mut self.flags, al);
            }
            0x30..=0x35 => self.alu_dispatch(opcode, 6, bus, master),
            // 0x36 = SS: prefix (consumed by consume_prefixes)
            0x37 => {
                // AAA: ASCII Adjust after Addition
                if (self.al() & 0x0F) > 9 || flags::get(self.flags, Flag::AF) {
                    let al = self.al().wrapping_add(6);
                    self.set_al(al);
                    self.set_ah(self.ah().wrapping_add(1));
                    flags::set(&mut self.flags, Flag::AF, true);
                    flags::set(&mut self.flags, Flag::CF, true);
                } else {
                    flags::set(&mut self.flags, Flag::AF, false);
                    flags::set(&mut self.flags, Flag::CF, false);
                }
                self.set_al(self.al() & 0x0F);
            }
            0x38..=0x3D => self.alu_dispatch(opcode, 7, bus, master),
            // 0x3E = DS: prefix (consumed by consume_prefixes)
            0x3F => {
                // AAS: ASCII Adjust after Subtraction
                if (self.al() & 0x0F) > 9 || flags::get(self.flags, Flag::AF) {
                    let al = self.al().wrapping_sub(6);
                    self.set_al(al);
                    self.set_ah(self.ah().wrapping_sub(1));
                    flags::set(&mut self.flags, Flag::AF, true);
                    flags::set(&mut self.flags, Flag::CF, true);
                } else {
                    flags::set(&mut self.flags, Flag::AF, false);
                    flags::set(&mut self.flags, Flag::CF, false);
                }
                self.set_al(self.al() & 0x0F);
            }

            // =============================================================
            // INC reg16 (0x40-0x47) / DEC reg16 (0x48-0x4F)
            // =============================================================
            0x40..=0x47 => {
                let reg = opcode & 7;
                let val = self.get_reg16(reg);
                let result = alu::inc16(&mut self.flags, val);
                self.set_reg16(reg, result);
            }
            0x48..=0x4F => {
                let reg = opcode & 7;
                let val = self.get_reg16(reg);
                let result = alu::dec16(&mut self.flags, val);
                self.set_reg16(reg, result);
            }

            // =============================================================
            // PUSH reg16 (0x50-0x57)
            // 8088 quirk: PUSH SP pushes the already-decremented SP value.
            // =============================================================
            0x50..=0x57 => {
                let reg = opcode & 7;
                if reg == 4 {
                    // PUSH SP: decrement first, then write the new SP
                    self.sp = self.sp.wrapping_sub(2);
                    self.write_word(bus, master, self.ss, self.sp, self.sp);
                } else {
                    let val = self.get_reg16(reg);
                    self.push16(bus, master, val);
                }
            }

            // =============================================================
            // POP reg16 (0x58-0x5F)
            // =============================================================
            0x58..=0x5F => {
                let reg = opcode & 7;
                let val = self.pop16(bus, master);
                self.set_reg16(reg, val);
            }

            // =============================================================
            // Jcc — conditional jumps (0x70-0x7F)
            // All take a signed 8-bit relative displacement.
            // =============================================================
            0x70..=0x7F => {
                let disp = self.fetch_byte(bus, master) as i8;
                if self.test_condition(opcode & 0x0F) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                }
            }

            // =============================================================
            // Immediate ALU group (0x80-0x83)
            //   0x80: ALU r/m8, imm8
            //   0x81: ALU r/m16, imm16
            //   0x82: ALU r/m8, imm8 (same as 0x80)
            //   0x83: ALU r/m16, imm8 (sign-extended to 16-bit)
            // =============================================================
            0x80 | 0x82 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let imm = self.fetch_byte(bus, master);
                let val = self.read_operand8(operand, bus, master);
                let result = self.alu_op8(modrm.reg, val, imm);
                // CMP (7) and TEST are compare-only — don't store result
                if modrm.reg != 7 {
                    self.write_operand8(operand, bus, master, result);
                }
            }
            0x81 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let imm = self.fetch_word(bus, master);
                let val = self.read_operand16(operand, bus, master);
                let result = self.alu_op16(modrm.reg, val, imm);
                if modrm.reg != 7 {
                    self.write_operand16(operand, bus, master, result);
                }
            }
            0x83 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                // Sign-extend imm8 to 16-bit
                let imm = self.fetch_byte(bus, master) as i8 as u16;
                let val = self.read_operand16(operand, bus, master);
                let result = self.alu_op16(modrm.reg, val, imm);
                if modrm.reg != 7 {
                    self.write_operand16(operand, bus, master, result);
                }
            }

            // =============================================================
            // TEST r/m8, reg8 (0x84) | TEST r/m16, reg16 (0x85)
            // =============================================================
            0x84 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.read_operand8(operand, bus, master);
                let b = self.get_reg8(modrm.reg);
                alu::and8(&mut self.flags, a, b);
            }
            0x85 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.read_operand16(operand, bus, master);
                let b = self.get_reg16(modrm.reg);
                alu::and16(&mut self.flags, a, b);
            }

            // =============================================================
            // XCHG r/m8, reg8 (0x86) | XCHG r/m16, reg16 (0x87)
            // =============================================================
            0x86 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.get_reg8(modrm.reg);
                let b = self.read_operand8(operand, bus, master);
                self.set_reg8(modrm.reg, b);
                self.write_operand8(operand, bus, master, a);
            }
            0x87 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.get_reg16(modrm.reg);
                let b = self.read_operand16(operand, bus, master);
                self.set_reg16(modrm.reg, b);
                self.write_operand16(operand, bus, master, a);
            }

            // =============================================================
            // MOV r/m, reg | MOV reg, r/m (0x88-0x8B)
            //   bit 0 (w): 0=byte, 1=word
            //   bit 1 (d): 0=reg→r/m, 1=r/m→reg
            // =============================================================
            0x88..=0x8B => {
                let w = opcode & 1 != 0;
                let d = opcode & 2 != 0;
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                if w {
                    if d {
                        let val = self.read_operand16(operand, bus, master);
                        self.set_reg16(modrm.reg, val);
                    } else {
                        let val = self.get_reg16(modrm.reg);
                        self.write_operand16(operand, bus, master, val);
                    }
                } else if d {
                    let val = self.read_operand8(operand, bus, master);
                    self.set_reg8(modrm.reg, val);
                } else {
                    let val = self.get_reg8(modrm.reg);
                    self.write_operand8(operand, bus, master, val);
                }
            }

            // =============================================================
            // MOV r/m16, segreg (0x8C)
            // =============================================================
            0x8C => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let seg = I8088::decode_seg(modrm.reg & 3);
                let val = self.get_seg(seg);
                self.write_operand16(operand, bus, master, val);
            }

            // =============================================================
            // LEA reg16, mem (0x8D)
            // =============================================================
            0x8D => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                if let Operand::Memory { offset, .. } = operand {
                    self.set_reg16(modrm.reg, offset);
                }
                // LEA with mod=11 is undefined; we treat it as a NOP.
            }

            // =============================================================
            // MOV segreg, r/m16 (0x8E)
            // =============================================================
            0x8E => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let val = self.read_operand16(operand, bus, master);
                let seg = I8088::decode_seg(modrm.reg & 3);
                self.set_seg(seg, val);
                // TODO: MOV to SS inhibits interrupts until after next
                // instruction (Step 1.8)
            }

            // =============================================================
            // POP r/m16 (0x8F /0)
            // =============================================================
            0x8F => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                if modrm.reg == 0 {
                    let val = self.pop16(bus, master);
                    self.write_operand16(operand, bus, master, val);
                }
            }

            // =============================================================
            // XCHG AX, reg16 (0x90-0x97)  — 0x90 = NOP (XCHG AX, AX)
            // =============================================================
            0x90..=0x97 => {
                let reg = opcode & 7;
                let temp = self.ax;
                self.ax = self.get_reg16(reg);
                self.set_reg16(reg, temp);
            }

            // =============================================================
            // CBW (0x98): sign-extend AL → AX
            // CWD (0x99): sign-extend AX → DX:AX
            // =============================================================
            0x98 => {
                self.ax = self.al() as i8 as i16 as u16;
            }
            0x99 => {
                self.dx = if self.ax & 0x8000 != 0 {
                    0xFFFF
                } else {
                    0x0000
                };
            }

            // =============================================================
            // CALL far (0x9A): push CS, push IP, load new CS:IP
            // =============================================================
            0x9A => {
                let offset = self.fetch_word(bus, master);
                let segment = self.fetch_word(bus, master);
                self.push16(bus, master, self.cs);
                self.push16(bus, master, self.ip);
                self.cs = segment;
                self.ip = offset;
            }

            // =============================================================
            // WAIT (0x9B): wait for TEST pin — no coprocessor, so NOP
            // PUSHF (0x9C): push FLAGS register
            // POPF (0x9D): pop FLAGS register
            // SAHF (0x9E): AH → FLAGS low byte (SF, ZF, AF, PF, CF)
            // LAHF (0x9F): FLAGS low byte → AH
            // =============================================================
            0x9B => {} // WAIT: no-op
            0x9C => {
                // PUSHF: push FLAGS with always-one bits
                self.push16(bus, master, self.flags);
            }
            0x9D => {
                // POPF: pop FLAGS, normalize to enforce always-one and clear undefined
                self.flags = flags::normalize(self.pop16(bus, master));
            }
            0x9E => {
                // SAHF: load SF, ZF, AF, PF, CF from AH
                // Defined flag bits in low byte: bits 7,6,4,2,0 = mask 0xD5
                // Bit 1 is always 1
                let ah = self.ah() as u16;
                self.flags = (self.flags & 0xFF00) | (ah & 0x00D5) | 0x0002;
            }
            0x9F => {
                // LAHF: store flags low byte into AH
                self.set_ah(self.flags as u8);
            }

            // =============================================================
            // MOV AL/AX, [moffs] (0xA0-0xA1)
            // MOV [moffs], AL/AX (0xA2-0xA3)
            // =============================================================
            0xA0 => {
                let offset = self.fetch_word(bus, master);
                let seg = self.effective_segment(SegReg::DS);
                let val = self.read_byte(bus, master, seg, offset);
                self.set_al(val);
            }
            0xA1 => {
                let offset = self.fetch_word(bus, master);
                let seg = self.effective_segment(SegReg::DS);
                let val = self.read_word(bus, master, seg, offset);
                self.ax = val;
            }
            0xA2 => {
                let offset = self.fetch_word(bus, master);
                let seg = self.effective_segment(SegReg::DS);
                self.write_byte(bus, master, seg, offset, self.al());
            }
            0xA3 => {
                let offset = self.fetch_word(bus, master);
                let seg = self.effective_segment(SegReg::DS);
                self.write_word(bus, master, seg, offset, self.ax);
            }

            // =============================================================
            // String operations (0xA4-0xA7, 0xAA-0xAF)
            //
            // REP prefix handling: when rep_prefix is set, the string
            // operation loops with CX as the counter. REP is used with
            // MOVS/STOS/LODS; REPZ/REPNZ with CMPS/SCAS (early exit
            // on ZF mismatch).
            // =============================================================
            0xA4 => self.string_op_movsb(bus, master),
            0xA5 => self.string_op_movsw(bus, master),
            0xA6 => self.string_op_cmpsb(bus, master),
            0xA7 => self.string_op_cmpsw(bus, master),
            0xAA => self.string_op_stosb(bus, master),
            0xAB => self.string_op_stosw(bus, master),
            0xAC => self.string_op_lodsb(bus, master),
            0xAD => self.string_op_lodsw(bus, master),
            0xAE => self.string_op_scasb(bus, master),
            0xAF => self.string_op_scasw(bus, master),

            // =============================================================
            // TEST AL, imm8 (0xA8) | TEST AX, imm16 (0xA9)
            // =============================================================
            0xA8 => {
                let imm = self.fetch_byte(bus, master);
                let al = self.al();
                alu::and8(&mut self.flags, al, imm);
            }
            0xA9 => {
                let imm = self.fetch_word(bus, master);
                let ax = self.ax;
                alu::and16(&mut self.flags, ax, imm);
            }

            // =============================================================
            // MOV reg8, imm8 (0xB0-0xB7)
            // =============================================================
            0xB0..=0xB7 => {
                let reg = opcode & 7;
                let imm = self.fetch_byte(bus, master);
                self.set_reg8(reg, imm);
            }

            // =============================================================
            // MOV reg16, imm16 (0xB8-0xBF)
            // =============================================================
            0xB8..=0xBF => {
                let reg = opcode & 7;
                let imm = self.fetch_word(bus, master);
                self.set_reg16(reg, imm);
            }

            // =============================================================
            // RET near with imm16 (0xC2): pop IP, SP += imm16
            // RET near (0xC3): pop IP
            // =============================================================
            0xC2 => {
                let imm = self.fetch_word(bus, master);
                self.ip = self.pop16(bus, master);
                self.sp = self.sp.wrapping_add(imm);
            }
            0xC3 => {
                self.ip = self.pop16(bus, master);
            }

            // =============================================================
            // RETF near with imm16 (0xCA): pop IP, pop CS, SP += imm16
            // RETF (0xCB): pop IP, pop CS
            // =============================================================
            0xCA => {
                let imm = self.fetch_word(bus, master);
                self.ip = self.pop16(bus, master);
                self.cs = self.pop16(bus, master);
                self.sp = self.sp.wrapping_add(imm);
            }
            0xCB => {
                self.ip = self.pop16(bus, master);
                self.cs = self.pop16(bus, master);
            }

            // =============================================================
            // LES reg16, mem32 (0xC4): load far pointer into ES:reg
            // LDS reg16, mem32 (0xC5): load far pointer into DS:reg
            // =============================================================
            0xC4 | 0xC5 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                if let Operand::Memory { segment, offset } = operand {
                    let new_offset = self.read_word(bus, master, segment, offset);
                    let new_seg = self.read_word(bus, master, segment, offset.wrapping_add(2));
                    self.set_reg16(modrm.reg, new_offset);
                    if opcode == 0xC4 {
                        self.es = new_seg;
                    } else {
                        self.ds = new_seg;
                    }
                }
            }

            // =============================================================
            // MOV r/m8, imm8 (0xC6) — reg field ignored on 8088
            // =============================================================
            0xC6 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let imm = self.fetch_byte(bus, master);
                self.write_operand8(operand, bus, master, imm);
            }

            // =============================================================
            // MOV r/m16, imm16 (0xC7) — reg field ignored on 8088
            // =============================================================
            0xC7 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let imm = self.fetch_word(bus, master);
                self.write_operand16(operand, bus, master, imm);
            }

            // =============================================================
            // INT 3 (0xCC), INT imm8 (0xCD), INTO (0xCE), IRET (0xCF)
            // =============================================================
            0xCC => {
                self.interrupt(bus, master, 3);
            }
            0xCD => {
                let vector = self.fetch_byte(bus, master);
                self.interrupt(bus, master, vector);
            }
            // INTO: interrupt if OF is set (vector 4); no-op when OF is clear
            0xCE if flags::get(self.flags, Flag::OF) => {
                self.interrupt(bus, master, 4);
            }
            0xCF => {
                // IRET: pop IP, CS, FLAGS
                self.ip = self.pop16(bus, master);
                self.cs = self.pop16(bus, master);
                self.flags = flags::normalize(self.pop16(bus, master));
            }

            // =============================================================
            // Shift/Rotate group (0xD0-0xD3)
            //   0xD0: r/m8, 1    0xD1: r/m16, 1
            //   0xD2: r/m8, CL   0xD3: r/m16, CL
            // =============================================================
            0xD0 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let val = self.read_operand8(operand, bus, master);
                let result = alu::shift_rotate8(&mut self.flags, val, 1, modrm.reg);
                self.write_operand8(operand, bus, master, result);
            }
            0xD1 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let val = self.read_operand16(operand, bus, master);
                let result = alu::shift_rotate16(&mut self.flags, val, 1, modrm.reg);
                self.write_operand16(operand, bus, master, result);
            }
            0xD2 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let val = self.read_operand8(operand, bus, master);
                let cl = self.cl();
                let result = alu::shift_rotate8(&mut self.flags, val, cl, modrm.reg);
                self.write_operand8(operand, bus, master, result);
            }
            0xD3 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let val = self.read_operand16(operand, bus, master);
                let cl = self.cl();
                let result = alu::shift_rotate16(&mut self.flags, val, cl, modrm.reg);
                self.write_operand16(operand, bus, master, result);
            }

            // =============================================================
            // AAM (0xD4): AH = AL / imm8, AL = AL % imm8
            // AAD (0xD5): AL = AH * imm8 + AL, AH = 0
            // =============================================================
            0xD4 => {
                let base = self.fetch_byte(bus, master);
                if base != 0 {
                    let al = self.al();
                    self.set_ah(al / base);
                    self.set_al(al % base);
                    let result_al = self.al();
                    flags::update_szp8(&mut self.flags, result_al);
                } else {
                    // AAM with base=0: divide error (INT 0)
                    // The 8088 updates SZP flags as if the result were 0.
                    flags::update_szp8(&mut self.flags, 0);
                    self.divide_error(bus, master);
                }
            }
            0xD5 => {
                let base = self.fetch_byte(bus, master);
                let result = self.ah().wrapping_mul(base).wrapping_add(self.al());
                self.set_al(result);
                self.set_ah(0);
                flags::update_szp8(&mut self.flags, result);
            }

            // =============================================================
            // XLAT (0xD7): AL = [DS:BX + unsigned AL]
            // =============================================================
            0xD7 => {
                let seg = self.effective_segment(SegReg::DS);
                let offset = self.bx.wrapping_add(self.al() as u16);
                let val = self.read_byte(bus, master, seg, offset);
                self.set_al(val);
            }

            // =============================================================
            // LOOP/LOOPZ/LOOPNZ/JCXZ (0xE0-0xE3)
            // =============================================================
            0xE0 => {
                // LOOPNZ/LOOPNE: CX -= 1; jump if CX != 0 AND ZF == 0
                let disp = self.fetch_byte(bus, master) as i8;
                self.cx = self.cx.wrapping_sub(1);
                if self.cx != 0 && !flags::get(self.flags, Flag::ZF) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                }
            }
            0xE1 => {
                // LOOPZ/LOOPE: CX -= 1; jump if CX != 0 AND ZF == 1
                let disp = self.fetch_byte(bus, master) as i8;
                self.cx = self.cx.wrapping_sub(1);
                if self.cx != 0 && flags::get(self.flags, Flag::ZF) {
                    self.ip = self.ip.wrapping_add(disp as u16);
                }
            }
            0xE2 => {
                // LOOP: CX -= 1; jump if CX != 0
                let disp = self.fetch_byte(bus, master) as i8;
                self.cx = self.cx.wrapping_sub(1);
                if self.cx != 0 {
                    self.ip = self.ip.wrapping_add(disp as u16);
                }
            }
            0xE3 => {
                // JCXZ: jump if CX == 0
                let disp = self.fetch_byte(bus, master) as i8;
                if self.cx == 0 {
                    self.ip = self.ip.wrapping_add(disp as u16);
                }
            }

            // =============================================================
            // IN/OUT with immediate port (0xE4-0xE7)
            // =============================================================
            0xE4 => {
                // IN AL, imm8
                let port = self.fetch_byte(bus, master) as u32;
                self.set_al(bus.io_read(master, port));
            }
            0xE5 => {
                // IN AX, imm8
                let port = self.fetch_byte(bus, master) as u32;
                self.set_al(bus.io_read(master, port));
                self.set_ah(bus.io_read(master, port.wrapping_add(1)));
            }
            0xE6 => {
                // OUT imm8, AL
                let port = self.fetch_byte(bus, master) as u32;
                bus.io_write(master, port, self.al());
            }
            0xE7 => {
                // OUT imm8, AX
                let port = self.fetch_byte(bus, master) as u32;
                bus.io_write(master, port, self.al());
                bus.io_write(master, port.wrapping_add(1), self.ah());
            }

            // =============================================================
            // CALL near (0xE8): push IP, IP += disp16
            // =============================================================
            0xE8 => {
                let disp = self.fetch_word(bus, master);
                self.push16(bus, master, self.ip);
                self.ip = self.ip.wrapping_add(disp);
            }

            // =============================================================
            // JMP near (0xE9): IP += disp16
            // =============================================================
            0xE9 => {
                let disp = self.fetch_word(bus, master);
                self.ip = self.ip.wrapping_add(disp);
            }

            // =============================================================
            // JMP far (0xEA): IP = offset, CS = segment
            // =============================================================
            0xEA => {
                let offset = self.fetch_word(bus, master);
                let segment = self.fetch_word(bus, master);
                self.ip = offset;
                self.cs = segment;
            }

            // =============================================================
            // JMP short (0xEB): IP += sign-extended disp8
            // =============================================================
            0xEB => {
                let disp = self.fetch_byte(bus, master) as i8;
                self.ip = self.ip.wrapping_add(disp as u16);
            }

            // =============================================================
            // IN/OUT with DX port (0xEC-0xEF)
            // =============================================================
            0xEC => {
                // IN AL, DX
                let port = self.dx as u32;
                self.set_al(bus.io_read(master, port));
            }
            0xED => {
                // IN AX, DX
                let port = self.dx as u32;
                self.set_al(bus.io_read(master, port));
                self.set_ah(bus.io_read(master, port.wrapping_add(1)));
            }
            0xEE => {
                // OUT DX, AL
                let port = self.dx as u32;
                bus.io_write(master, port, self.al());
            }
            0xEF => {
                // OUT DX, AX
                let port = self.dx as u32;
                bus.io_write(master, port, self.al());
                bus.io_write(master, port.wrapping_add(1), self.ah());
            }

            // =============================================================
            // HLT (0xF4): halt until interrupt
            // CMC (0xF5): complement carry flag
            // =============================================================
            0xF4 => {
                self.state = ExecState::Halted;
            }
            0xF5 => {
                let cf = flags::get(self.flags, Flag::CF);
                flags::set(&mut self.flags, Flag::CF, !cf);
            }

            // =============================================================
            // Unary group 0xF6 (byte)
            // =============================================================
            0xF6 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                match modrm.reg {
                    0 => {
                        // TEST r/m8, imm8
                        let imm = self.fetch_byte(bus, master);
                        let val = self.read_operand8(operand, bus, master);
                        alu::and8(&mut self.flags, val, imm);
                    }
                    2 => {
                        // NOT r/m8
                        let val = self.read_operand8(operand, bus, master);
                        self.write_operand8(operand, bus, master, alu::not8(val));
                    }
                    3 => {
                        // NEG r/m8
                        let val = self.read_operand8(operand, bus, master);
                        let result = alu::neg8(&mut self.flags, val);
                        self.write_operand8(operand, bus, master, result);
                    }
                    4 => {
                        // MUL r/m8: AX = AL * r/m8 (unsigned)
                        let val = self.read_operand8(operand, bus, master);
                        let result = self.al() as u16 * val as u16;
                        self.ax = result;
                        let of_cf = self.ah() != 0;
                        flags::set(&mut self.flags, Flag::CF, of_cf);
                        flags::set(&mut self.flags, Flag::OF, of_cf);
                    }
                    5 => {
                        // IMUL r/m8: AX = AL * r/m8 (signed)
                        let val = self.read_operand8(operand, bus, master);
                        let result = (self.al() as i8 as i16) * (val as i8 as i16);
                        self.ax = result as u16;
                        // CF=OF=1 if result doesn't fit in signed 8-bit
                        let of_cf = result != self.al() as i8 as i16;
                        flags::set(&mut self.flags, Flag::CF, of_cf);
                        flags::set(&mut self.flags, Flag::OF, of_cf);
                    }
                    6 => {
                        // DIV r/m8: AL = AX / r/m8, AH = AX % r/m8
                        let val = self.read_operand8(operand, bus, master) as u16;
                        if val != 0 {
                            let quotient = self.ax / val;
                            if quotient <= 0xFF {
                                let remainder = self.ax % val;
                                self.set_al(quotient as u8);
                                self.set_ah(remainder as u8);
                            } else {
                                self.divide_error(bus, master);
                            }
                        } else {
                            self.divide_error(bus, master);
                        }
                    }
                    7 => {
                        // IDIV r/m8: AL = AX / r/m8, AH = AX % r/m8 (signed)
                        // 8088 quirk: REP/REPNE prefix negates the quotient.
                        // 8088 quirk: quotient = -128 triggers divide error
                        // (valid range is -127..=127, not -128..=127).
                        let val = self.read_operand8(operand, bus, master) as i8;
                        if val != 0 {
                            let dividend = self.ax as i16;
                            let mut quotient = dividend / val as i16;
                            if self.rep_prefix.is_some() {
                                quotient = quotient.wrapping_neg();
                            }
                            if (-127..=127).contains(&quotient) {
                                let remainder = dividend % val as i16;
                                self.set_al(quotient as u8);
                                self.set_ah(remainder as u8);
                            } else {
                                self.divide_error(bus, master);
                            }
                        } else {
                            self.divide_error(bus, master);
                        }
                    }
                    _ => {}
                }
            }

            // =============================================================
            // Unary group 0xF7 (word)
            // =============================================================
            0xF7 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                match modrm.reg {
                    0 => {
                        // TEST r/m16, imm16
                        let imm = self.fetch_word(bus, master);
                        let val = self.read_operand16(operand, bus, master);
                        alu::and16(&mut self.flags, val, imm);
                    }
                    2 => {
                        // NOT r/m16
                        let val = self.read_operand16(operand, bus, master);
                        self.write_operand16(operand, bus, master, alu::not16(val));
                    }
                    3 => {
                        // NEG r/m16
                        let val = self.read_operand16(operand, bus, master);
                        let result = alu::neg16(&mut self.flags, val);
                        self.write_operand16(operand, bus, master, result);
                    }
                    4 => {
                        // MUL r/m16: DX:AX = AX * r/m16 (unsigned)
                        let val = self.read_operand16(operand, bus, master);
                        let result = self.ax as u32 * val as u32;
                        self.ax = result as u16;
                        self.dx = (result >> 16) as u16;
                        let of_cf = self.dx != 0;
                        flags::set(&mut self.flags, Flag::CF, of_cf);
                        flags::set(&mut self.flags, Flag::OF, of_cf);
                    }
                    5 => {
                        // IMUL r/m16: DX:AX = AX * r/m16 (signed)
                        let val = self.read_operand16(operand, bus, master);
                        let result = (self.ax as i16 as i32) * (val as i16 as i32);
                        self.ax = result as u16;
                        self.dx = (result >> 16) as u16;
                        // CF=OF=1 if result doesn't fit in signed 16-bit
                        let of_cf = result != self.ax as i16 as i32;
                        flags::set(&mut self.flags, Flag::CF, of_cf);
                        flags::set(&mut self.flags, Flag::OF, of_cf);
                    }
                    6 => {
                        // DIV r/m16: AX = DX:AX / r/m16, DX = DX:AX % r/m16
                        let val = self.read_operand16(operand, bus, master) as u32;
                        if val != 0 {
                            let dividend = ((self.dx as u32) << 16) | self.ax as u32;
                            let quotient = dividend / val;
                            if quotient <= 0xFFFF {
                                let remainder = dividend % val;
                                self.ax = quotient as u16;
                                self.dx = remainder as u16;
                            } else {
                                self.divide_error(bus, master);
                            }
                        } else {
                            self.divide_error(bus, master);
                        }
                    }
                    7 => {
                        // IDIV r/m16: AX = DX:AX / r/m16, DX = DX:AX % r/m16 (signed)
                        // 8088 quirk: REP/REPNE prefix negates the quotient.
                        // 8088 quirk: quotient = -32768 triggers divide error.
                        let val = self.read_operand16(operand, bus, master) as i16;
                        if val != 0 {
                            let dividend = (((self.dx as u32) << 16) | self.ax as u32) as i32;
                            let mut quotient = dividend / val as i32;
                            if self.rep_prefix.is_some() {
                                quotient = quotient.wrapping_neg();
                            }
                            if (-32767..=32767).contains(&quotient) {
                                let remainder = dividend % val as i32;
                                self.ax = quotient as u16;
                                self.dx = remainder as u16;
                            } else {
                                self.divide_error(bus, master);
                            }
                        } else {
                            self.divide_error(bus, master);
                        }
                    }
                    _ => {}
                }
            }

            // =============================================================
            // 0xFE group (byte): /0=INC r/m8, /1=DEC r/m8
            // =============================================================
            0xFE => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                match modrm.reg {
                    0 => {
                        let val = self.read_operand8(operand, bus, master);
                        let result = alu::inc8(&mut self.flags, val);
                        self.write_operand8(operand, bus, master, result);
                    }
                    1 => {
                        let val = self.read_operand8(operand, bus, master);
                        let result = alu::dec8(&mut self.flags, val);
                        self.write_operand8(operand, bus, master, result);
                    }
                    _ => {}
                }
            }

            // =============================================================
            // 0xFF group: /0=INC, /1=DEC, /2=CALL near indirect,
            // /3=CALL far indirect, /4=JMP near indirect,
            // /5=JMP far indirect, /6=PUSH r/m16
            // =============================================================
            0xFF => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                match modrm.reg {
                    0 => {
                        let val = self.read_operand16(operand, bus, master);
                        let result = alu::inc16(&mut self.flags, val);
                        self.write_operand16(operand, bus, master, result);
                    }
                    1 => {
                        let val = self.read_operand16(operand, bus, master);
                        let result = alu::dec16(&mut self.flags, val);
                        self.write_operand16(operand, bus, master, result);
                    }
                    2 => {
                        // CALL near indirect: push IP, IP = r/m16
                        let target = self.read_operand16(operand, bus, master);
                        self.push16(bus, master, self.ip);
                        self.ip = target;
                    }
                    3 => {
                        // CALL far indirect: push CS, push IP, load CS:IP from m32
                        if let Operand::Memory { segment, offset } = operand {
                            let new_ip = self.read_word(bus, master, segment, offset);
                            let new_cs =
                                self.read_word(bus, master, segment, offset.wrapping_add(2));
                            self.push16(bus, master, self.cs);
                            self.push16(bus, master, self.ip);
                            self.ip = new_ip;
                            self.cs = new_cs;
                        }
                    }
                    4 => {
                        // JMP near indirect: IP = r/m16
                        let target = self.read_operand16(operand, bus, master);
                        self.ip = target;
                    }
                    5 => {
                        // JMP far indirect: load CS:IP from m32
                        if let Operand::Memory { segment, offset } = operand {
                            let new_ip = self.read_word(bus, master, segment, offset);
                            let new_cs =
                                self.read_word(bus, master, segment, offset.wrapping_add(2));
                            self.ip = new_ip;
                            self.cs = new_cs;
                        }
                    }
                    6 => {
                        // PUSH r/m16
                        // 8088 quirk: if operand is SP, push the decremented value
                        if operand == Operand::Register(4) {
                            self.sp = self.sp.wrapping_sub(2);
                            self.write_word(bus, master, self.ss, self.sp, self.sp);
                        } else {
                            let val = self.read_operand16(operand, bus, master);
                            self.push16(bus, master, val);
                        }
                    }
                    _ => {}
                }
            }

            // =============================================================
            // Flag control (0xF8-0xFD)
            // =============================================================
            0xF8 => flags::set(&mut self.flags, Flag::CF, false), // CLC
            0xF9 => flags::set(&mut self.flags, Flag::CF, true),  // STC
            0xFA => flags::set(&mut self.flags, Flag::IF, false), // CLI
            0xFB => flags::set(&mut self.flags, Flag::IF, true),  // STI
            0xFC => flags::set(&mut self.flags, Flag::DF, false), // CLD
            0xFD => flags::set(&mut self.flags, Flag::DF, true),  // STD

            // =============================================================
            // Unimplemented opcode — silently skip
            // =============================================================
            _ => {}
        }
    }

    // -----------------------------------------------------------------
    // Interrupt helper
    // -----------------------------------------------------------------

    /// Execute an interrupt to the given vector number.
    /// Pushes FLAGS, CS, IP; clears IF and TF; loads CS:IP from IVT at 0000:vector*4.
    pub(crate) fn interrupt<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        vector: u8,
    ) {
        self.push16(bus, master, self.flags);
        flags::set(&mut self.flags, Flag::IF, false);
        flags::set(&mut self.flags, Flag::TF, false);
        self.push16(bus, master, self.cs);
        self.push16(bus, master, self.ip);
        // IVT: 4 bytes per vector at physical 0000:(vector*4)
        let ivt_addr = (vector as u16).wrapping_mul(4);
        self.ip = self.read_word(bus, master, 0, ivt_addr);
        self.cs = self.read_word(bus, master, 0, ivt_addr.wrapping_add(2));
    }

    /// Trigger a divide-error exception (INT 0).
    /// On the 8088, the pushed IP is the current IP (past the instruction),
    /// not the faulting instruction address. Registers are not modified.
    fn divide_error<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.interrupt(bus, master, 0);
    }

    // -----------------------------------------------------------------
    // ALU helpers
    // -----------------------------------------------------------------

    /// Dispatch a standard ALU opcode from the 0x00-0x3D range.
    /// `op` is the ALU operation (0=ADD, 1=OR, 2=ADC, 3=SBB, 4=AND, 5=SUB,
    /// 6=XOR, 7=CMP). The low 3 bits of `opcode` select the operand form.
    fn alu_dispatch<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        op: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let sub = opcode & 7;
        match sub {
            // r/m8, reg8
            0 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.read_operand8(operand, bus, master);
                let b = self.get_reg8(modrm.reg);
                let result = self.alu_op8(op, a, b);
                if op != 7 {
                    // not CMP
                    self.write_operand8(operand, bus, master, result);
                }
            }
            // r/m16, reg16
            1 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.read_operand16(operand, bus, master);
                let b = self.get_reg16(modrm.reg);
                let result = self.alu_op16(op, a, b);
                if op != 7 {
                    self.write_operand16(operand, bus, master, result);
                }
            }
            // reg8, r/m8
            2 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.get_reg8(modrm.reg);
                let b = self.read_operand8(operand, bus, master);
                let result = self.alu_op8(op, a, b);
                if op != 7 {
                    self.set_reg8(modrm.reg, result);
                }
            }
            // reg16, r/m16
            3 => {
                let modrm = self.fetch_modrm(bus, master);
                let operand = self.resolve_modrm(modrm, bus, master);
                let a = self.get_reg16(modrm.reg);
                let b = self.read_operand16(operand, bus, master);
                let result = self.alu_op16(op, a, b);
                if op != 7 {
                    self.set_reg16(modrm.reg, result);
                }
            }
            // AL, imm8
            4 => {
                let imm = self.fetch_byte(bus, master);
                let result = self.alu_op8(op, self.al(), imm);
                if op != 7 {
                    self.set_al(result);
                }
            }
            // AX, imm16
            5 => {
                let imm = self.fetch_word(bus, master);
                let result = self.alu_op16(op, self.ax, imm);
                if op != 7 {
                    self.ax = result;
                }
            }
            _ => unreachable!(),
        }
    }

    /// Execute an 8-bit ALU operation by op code (0-7).
    #[inline]
    fn alu_op8(&mut self, op: u8, a: u8, b: u8) -> u8 {
        let cf = flags::get(self.flags, Flag::CF);
        match op & 7 {
            0 => alu::add8(&mut self.flags, a, b, false),
            1 => alu::or8(&mut self.flags, a, b),
            2 => alu::add8(&mut self.flags, a, b, cf),
            3 => alu::sub8(&mut self.flags, a, b, cf),
            4 => alu::and8(&mut self.flags, a, b),
            5 => alu::sub8(&mut self.flags, a, b, false),
            6 => alu::xor8(&mut self.flags, a, b),
            7 => alu::sub8(&mut self.flags, a, b, false), // CMP
            _ => unreachable!(),
        }
    }

    /// Execute a 16-bit ALU operation by op code (0-7).
    #[inline]
    fn alu_op16(&mut self, op: u8, a: u16, b: u16) -> u16 {
        let cf = flags::get(self.flags, Flag::CF);
        match op & 7 {
            0 => alu::add16(&mut self.flags, a, b, false),
            1 => alu::or16(&mut self.flags, a, b),
            2 => alu::add16(&mut self.flags, a, b, cf),
            3 => alu::sub16(&mut self.flags, a, b, cf),
            4 => alu::and16(&mut self.flags, a, b),
            5 => alu::sub16(&mut self.flags, a, b, false),
            6 => alu::xor16(&mut self.flags, a, b),
            7 => alu::sub16(&mut self.flags, a, b, false), // CMP
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------
    // Condition code testing (for Jcc 0x70-0x7F)
    // -----------------------------------------------------------------

    /// Test a condition code (0x0-0xF) against the current FLAGS register.
    /// Condition codes come in pairs: even = condition, odd = NOT condition.
    #[inline]
    fn test_condition(&self, cc: u8) -> bool {
        let f = self.flags;
        let cf = flags::get(f, Flag::CF);
        let zf = flags::get(f, Flag::ZF);
        let sf = flags::get(f, Flag::SF);
        let of = flags::get(f, Flag::OF);
        let pf = flags::get(f, Flag::PF);
        match cc & 0x0F {
            0x0 => of,                // JO
            0x1 => !of,               // JNO
            0x2 => cf,                // JB / JNAE / JC
            0x3 => !cf,               // JNB / JAE / JNC
            0x4 => zf,                // JZ / JE
            0x5 => !zf,               // JNZ / JNE
            0x6 => cf || zf,          // JBE / JNA
            0x7 => !cf && !zf,        // JA / JNBE
            0x8 => sf,                // JS
            0x9 => !sf,               // JNS
            0xA => pf,                // JP / JPE
            0xB => !pf,               // JNP / JPO
            0xC => sf != of,          // JL / JNGE
            0xD => sf == of,          // JGE / JNL
            0xE => zf || (sf != of),  // JLE / JNG
            0xF => !zf && (sf == of), // JG / JNLE
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------
    // String operations
    // -----------------------------------------------------------------

    /// SI/DI step size: +1 or -1 for byte ops, +2 or -2 for word ops.
    #[inline]
    fn string_step(&self, word: bool) -> u16 {
        let size: u16 = if word { 2 } else { 1 };
        if flags::get(self.flags, Flag::DF) {
            size.wrapping_neg() // decrement
        } else {
            size // increment
        }
    }

    /// Source segment for string operations: DS (or segment override).
    #[inline]
    fn string_src_seg(&self) -> u16 {
        self.effective_segment(SegReg::DS)
    }

    /// MOVSB: [ES:DI] <- [DS:SI], SI += step, DI += step
    fn string_op_movsb<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(false);
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let val = self.read_byte(bus, master, seg, self.si);
                    self.write_byte(bus, master, self.es, self.di, val);
                    self.si = self.si.wrapping_add(step);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                let seg = self.string_src_seg();
                let val = self.read_byte(bus, master, seg, self.si);
                self.write_byte(bus, master, self.es, self.di, val);
                self.si = self.si.wrapping_add(step);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// MOVSW: [ES:DI] <- [DS:SI], SI += step*2, DI += step*2
    fn string_op_movsw<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(true);
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let val = self.read_word(bus, master, seg, self.si);
                    self.write_word(bus, master, self.es, self.di, val);
                    self.si = self.si.wrapping_add(step);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                let seg = self.string_src_seg();
                let val = self.read_word(bus, master, seg, self.si);
                self.write_word(bus, master, self.es, self.di, val);
                self.si = self.si.wrapping_add(step);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// CMPSB: [DS:SI] - [ES:DI] (set flags), SI += step, DI += step
    fn string_op_cmpsb<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(false);
        match rep {
            Some(prefix) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let a = self.read_byte(bus, master, seg, self.si);
                    let b = self.read_byte(bus, master, self.es, self.di);
                    alu::sub8(&mut self.flags, a, b, false);
                    self.si = self.si.wrapping_add(step);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                    // REPZ: continue while ZF=1; REPNZ: continue while ZF=0
                    let zf = flags::get(self.flags, Flag::ZF);
                    match prefix {
                        RepPrefix::Rep => {
                            if !zf {
                                break;
                            }
                        }
                        RepPrefix::Repnz => {
                            if zf {
                                break;
                            }
                        }
                    }
                }
            }
            None => {
                let seg = self.string_src_seg();
                let a = self.read_byte(bus, master, seg, self.si);
                let b = self.read_byte(bus, master, self.es, self.di);
                alu::sub8(&mut self.flags, a, b, false);
                self.si = self.si.wrapping_add(step);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// CMPSW: [DS:SI] - [ES:DI] (set flags, 16-bit), SI += step, DI += step
    fn string_op_cmpsw<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(true);
        match rep {
            Some(prefix) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let a = self.read_word(bus, master, seg, self.si);
                    let b = self.read_word(bus, master, self.es, self.di);
                    alu::sub16(&mut self.flags, a, b, false);
                    self.si = self.si.wrapping_add(step);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                    let zf = flags::get(self.flags, Flag::ZF);
                    match prefix {
                        RepPrefix::Rep => {
                            if !zf {
                                break;
                            }
                        }
                        RepPrefix::Repnz => {
                            if zf {
                                break;
                            }
                        }
                    }
                }
            }
            None => {
                let seg = self.string_src_seg();
                let a = self.read_word(bus, master, seg, self.si);
                let b = self.read_word(bus, master, self.es, self.di);
                alu::sub16(&mut self.flags, a, b, false);
                self.si = self.si.wrapping_add(step);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// STOSB: [ES:DI] <- AL, DI += step
    fn string_op_stosb<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(false);
        let al = self.al();
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    self.write_byte(bus, master, self.es, self.di, al);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                self.write_byte(bus, master, self.es, self.di, al);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// STOSW: [ES:DI] <- AX, DI += step
    fn string_op_stosw<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(true);
        let ax = self.ax;
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    self.write_word(bus, master, self.es, self.di, ax);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                self.write_word(bus, master, self.es, self.di, ax);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// LODSB: AL <- [DS:SI], SI += step
    fn string_op_lodsb<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(false);
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let val = self.read_byte(bus, master, seg, self.si);
                    self.set_al(val);
                    self.si = self.si.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                let seg = self.string_src_seg();
                let val = self.read_byte(bus, master, seg, self.si);
                self.set_al(val);
                self.si = self.si.wrapping_add(step);
            }
        }
    }

    /// LODSW: AX <- [DS:SI], SI += step
    fn string_op_lodsw<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(true);
        match rep {
            Some(_) => {
                while self.cx != 0 {
                    let seg = self.string_src_seg();
                    let val = self.read_word(bus, master, seg, self.si);
                    self.ax = val;
                    self.si = self.si.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                }
            }
            None => {
                let seg = self.string_src_seg();
                let val = self.read_word(bus, master, seg, self.si);
                self.ax = val;
                self.si = self.si.wrapping_add(step);
            }
        }
    }

    /// SCASB: AL - [ES:DI] (set flags), DI += step
    fn string_op_scasb<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(false);
        match rep {
            Some(prefix) => {
                while self.cx != 0 {
                    let al = self.al();
                    let val = self.read_byte(bus, master, self.es, self.di);
                    alu::sub8(&mut self.flags, al, val, false);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                    let zf = flags::get(self.flags, Flag::ZF);
                    match prefix {
                        RepPrefix::Rep => {
                            if !zf {
                                break;
                            }
                        }
                        RepPrefix::Repnz => {
                            if zf {
                                break;
                            }
                        }
                    }
                }
            }
            None => {
                let al = self.al();
                let val = self.read_byte(bus, master, self.es, self.di);
                alu::sub8(&mut self.flags, al, val, false);
                self.di = self.di.wrapping_add(step);
            }
        }
    }

    /// SCASW: AX - [ES:DI] (set flags, 16-bit), DI += step
    fn string_op_scasw<B: Bus<Address = u32, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rep = self.rep_prefix.take();
        let step = self.string_step(true);
        match rep {
            Some(prefix) => {
                while self.cx != 0 {
                    let val = self.read_word(bus, master, self.es, self.di);
                    alu::sub16(&mut self.flags, self.ax, val, false);
                    self.di = self.di.wrapping_add(step);
                    self.cx = self.cx.wrapping_sub(1);
                    let zf = flags::get(self.flags, Flag::ZF);
                    match prefix {
                        RepPrefix::Rep => {
                            if !zf {
                                break;
                            }
                        }
                        RepPrefix::Repnz => {
                            if zf {
                                break;
                            }
                        }
                    }
                }
            }
            None => {
                let val = self.read_word(bus, master, self.es, self.di);
                alu::sub16(&mut self.flags, self.ax, val, false);
                self.di = self.di.wrapping_add(step);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::InterruptState;

    /// 1 MB test bus (heap-allocated to avoid stack overflow).
    struct TestBus {
        mem: Box<[u8; 0x10_0000]>,
    }

    impl TestBus {
        fn new() -> Self {
            Self {
                mem: Box::new([0; 0x10_0000]),
            }
        }
    }

    impl Bus for TestBus {
        type Address = u32;
        type Data = u8;

        fn read(&mut self, _master: BusMaster, addr: u32) -> u8 {
            self.mem[(addr & 0xF_FFFF) as usize]
        }

        fn write(&mut self, _master: BusMaster, addr: u32, data: u8) {
            self.mem[(addr & 0xF_FFFF) as usize] = data;
        }

        fn is_halted_for(&self, _master: BusMaster) -> bool {
            false
        }

        fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
            InterruptState::default()
        }
    }

    const M: BusMaster = BusMaster::Cpu(0);

    /// Create a CPU ready for testing: CS=0, IP=0x100, DS=0x2000,
    /// SS=0x3000, ES=0x4000, SP=0x0200.
    fn setup() -> (I8088, TestBus) {
        let mut cpu = I8088::new();
        cpu.cs = 0x0000;
        cpu.ip = 0x0100;
        cpu.ds = 0x2000;
        cpu.ss = 0x3000;
        cpu.es = 0x4000;
        cpu.sp = 0x0200;
        (cpu, TestBus::new())
    }

    // =====================================================================
    // MOV reg8, imm8 (0xB0-0xB7)
    // =====================================================================

    #[test]
    fn mov_al_imm8() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x42; // imm8
        cpu.execute(0xB0, &mut bus, M);
        assert_eq!(cpu.al(), 0x42);
        assert_eq!(cpu.ip, 0x101);
    }

    #[test]
    fn mov_all_reg8_imm8() {
        let (mut cpu, mut bus) = setup();
        // Test all 8 registers: AL,CL,DL,BL,AH,CH,DH,BH
        for reg in 0..8u8 {
            cpu.ip = 0x100;
            bus.mem[0x100] = 0x10 + reg; // distinct value per register
            cpu.execute(0xB0 + reg, &mut bus, M);
            assert_eq!(cpu.get_reg8(reg), 0x10 + reg);
        }
    }

    // =====================================================================
    // MOV reg16, imm16 (0xB8-0xBF)
    // =====================================================================

    #[test]
    fn mov_ax_imm16() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x34; // low byte
        bus.mem[0x101] = 0x12; // high byte
        cpu.execute(0xB8, &mut bus, M); // MOV AX, 0x1234
        assert_eq!(cpu.ax, 0x1234);
        assert_eq!(cpu.ip, 0x102);
    }

    #[test]
    fn mov_all_reg16_imm16() {
        let (mut cpu, mut bus) = setup();
        for reg in 0..8u8 {
            cpu.ip = 0x100;
            let val = 0x1000 + reg as u16;
            bus.mem[0x100] = val as u8;
            bus.mem[0x101] = (val >> 8) as u8;
            cpu.execute(0xB8 + reg, &mut bus, M);
            assert_eq!(cpu.get_reg16(reg), val);
        }
    }

    // =====================================================================
    // MOV r/m, reg | MOV reg, r/m (0x88-0x8B)
    // =====================================================================

    #[test]
    fn mov_rm8_reg8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xAB);
        cpu.bx = 0x0050;
        // ModR/M: mod=00 reg=000(AL) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x88, &mut bus, M); // MOV [BX], AL
        // DS:BX = 0x20000 + 0x50 = 0x20050
        assert_eq!(bus.mem[0x20050], 0xAB);
    }

    #[test]
    fn mov_reg8_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0050;
        bus.mem[0x20050] = 0xCD; // value at DS:BX
        // ModR/M: mod=00 reg=001(CL) rm=111([BX]) = 0x0F
        bus.mem[0x100] = 0x0F;
        cpu.execute(0x8A, &mut bus, M); // MOV CL, [BX]
        assert_eq!(cpu.cl(), 0xCD);
    }

    #[test]
    fn mov_rm16_reg16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        cpu.si = 0x0080;
        // ModR/M: mod=00 reg=000(AX) rm=100([SI]) = 0x04
        bus.mem[0x100] = 0x04;
        cpu.execute(0x89, &mut bus, M); // MOV [SI], AX
        // DS:SI = 0x20080
        assert_eq!(bus.mem[0x20080], 0x34);
        assert_eq!(bus.mem[0x20081], 0x12);
    }

    #[test]
    fn mov_reg16_rm16() {
        let (mut cpu, mut bus) = setup();
        cpu.di = 0x0060;
        bus.mem[0x20060] = 0x78;
        bus.mem[0x20061] = 0x56;
        // ModR/M: mod=00 reg=011(BX) rm=101([DI]) = 0x1D
        bus.mem[0x100] = 0x1D;
        cpu.execute(0x8B, &mut bus, M); // MOV BX, [DI]
        assert_eq!(cpu.bx, 0x5678);
    }

    #[test]
    fn mov_reg_reg() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xAABB;
        // MOV CX, AX: 0x89 ModR/M=0xC1 (mod=11 reg=000(AX) rm=001(CX))
        bus.mem[0x100] = 0xC1;
        cpu.execute(0x89, &mut bus, M);
        assert_eq!(cpu.cx, 0xAABB);
    }

    // =====================================================================
    // MOV r/m16, segreg (0x8C) | MOV segreg, r/m16 (0x8E)
    // =====================================================================

    #[test]
    fn mov_rm16_segreg() {
        let (mut cpu, mut bus) = setup();
        cpu.ds = 0x1234;
        // ModR/M: mod=11 reg=011(DS) rm=000(AX) = 0xD8
        bus.mem[0x100] = 0xD8;
        cpu.execute(0x8C, &mut bus, M); // MOV AX, DS
        assert_eq!(cpu.ax, 0x1234);
    }

    #[test]
    fn mov_segreg_rm16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x5000;
        // ModR/M: mod=11 reg=000(ES) rm=000(AX) = 0xC0
        bus.mem[0x100] = 0xC0;
        cpu.execute(0x8E, &mut bus, M); // MOV ES, AX
        assert_eq!(cpu.es, 0x5000);
    }

    // =====================================================================
    // MOV AL/AX, [moffs] (0xA0-0xA1)
    // MOV [moffs], AL/AX (0xA2-0xA3)
    // =====================================================================

    #[test]
    fn mov_al_moffs() {
        let (mut cpu, mut bus) = setup();
        // moffs = 0x0050
        bus.mem[0x100] = 0x50;
        bus.mem[0x101] = 0x00;
        // Value at DS:0x0050 = 0x20050
        bus.mem[0x20050] = 0xEF;
        cpu.execute(0xA0, &mut bus, M); // MOV AL, [0x0050]
        assert_eq!(cpu.al(), 0xEF);
    }

    #[test]
    fn mov_ax_moffs() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x60;
        bus.mem[0x101] = 0x00;
        bus.mem[0x20060] = 0x34;
        bus.mem[0x20061] = 0x12;
        cpu.execute(0xA1, &mut bus, M); // MOV AX, [0x0060]
        assert_eq!(cpu.ax, 0x1234);
    }

    #[test]
    fn mov_moffs_al() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x42);
        bus.mem[0x100] = 0x70;
        bus.mem[0x101] = 0x00;
        cpu.execute(0xA2, &mut bus, M); // MOV [0x0070], AL
        assert_eq!(bus.mem[0x20070], 0x42);
    }

    #[test]
    fn mov_moffs_ax() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xABCD;
        bus.mem[0x100] = 0x80;
        bus.mem[0x101] = 0x00;
        cpu.execute(0xA3, &mut bus, M); // MOV [0x0080], AX
        assert_eq!(bus.mem[0x20080], 0xCD);
        assert_eq!(bus.mem[0x20081], 0xAB);
    }

    // =====================================================================
    // MOV r/m, imm (0xC6, 0xC7)
    // =====================================================================

    #[test]
    fn mov_rm8_imm8() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0040;
        // ModR/M: mod=00 reg=000 rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        bus.mem[0x101] = 0xFF; // imm8
        cpu.execute(0xC6, &mut bus, M); // MOV BYTE [BX], 0xFF
        assert_eq!(bus.mem[0x20040], 0xFF);
    }

    #[test]
    fn mov_rm16_imm16() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0040;
        // ModR/M: mod=00 reg=000 rm=100([SI]) = 0x04
        bus.mem[0x100] = 0x04;
        bus.mem[0x101] = 0x34; // imm16 low
        bus.mem[0x102] = 0x12; // imm16 high
        cpu.execute(0xC7, &mut bus, M); // MOV WORD [SI], 0x1234
        assert_eq!(bus.mem[0x20040], 0x34);
        assert_eq!(bus.mem[0x20041], 0x12);
    }

    // =====================================================================
    // PUSH/POP reg16 (0x50-0x5F)
    // =====================================================================

    #[test]
    fn push_pop_reg16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        cpu.bx = 0x5678;

        cpu.execute(0x50, &mut bus, M); // PUSH AX
        assert_eq!(cpu.sp, 0x01FE);
        cpu.execute(0x53, &mut bus, M); // PUSH BX
        assert_eq!(cpu.sp, 0x01FC);

        // Pop in reverse order (LIFO)
        cpu.execute(0x59, &mut bus, M); // POP CX
        assert_eq!(cpu.cx, 0x5678);
        assert_eq!(cpu.sp, 0x01FE);
        cpu.execute(0x5A, &mut bus, M); // POP DX
        assert_eq!(cpu.dx, 0x1234);
        assert_eq!(cpu.sp, 0x0200);
    }

    #[test]
    fn push_sp_pushes_decremented_value() {
        let (mut cpu, mut bus) = setup();
        // 8088 quirk: PUSH SP pushes the already-decremented SP value
        let old_sp = cpu.sp;
        cpu.execute(0x54, &mut bus, M); // PUSH SP
        let pushed_val = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        assert_eq!(cpu.sp, old_sp.wrapping_sub(2));
        assert_eq!(pushed_val, cpu.sp);
    }

    // =====================================================================
    // PUSH/POP segment registers
    // =====================================================================

    #[test]
    fn push_pop_es() {
        let (mut cpu, mut bus) = setup();
        cpu.es = 0xABCD;
        cpu.execute(0x06, &mut bus, M); // PUSH ES
        cpu.es = 0x0000; // Clear it
        cpu.execute(0x07, &mut bus, M); // POP ES
        assert_eq!(cpu.es, 0xABCD);
    }

    #[test]
    fn push_pop_ds() {
        let (mut cpu, mut bus) = setup();
        cpu.ds = 0x1234;
        cpu.execute(0x1E, &mut bus, M); // PUSH DS
        cpu.ds = 0x0000;
        cpu.execute(0x1F, &mut bus, M); // POP DS
        assert_eq!(cpu.ds, 0x1234);
    }

    #[test]
    fn push_cs() {
        let (mut cpu, mut bus) = setup();
        cpu.cs = 0xF000;
        cpu.execute(0x0E, &mut bus, M); // PUSH CS
        let val = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        assert_eq!(val, 0xF000);
    }

    // =====================================================================
    // PUSH/POP r/m16 (0xFF /6, 0x8F /0)
    // =====================================================================

    #[test]
    fn push_rm16_mem() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0020;
        // Store 0xBEEF at DS:BX
        bus.mem[0x20020] = 0xEF;
        bus.mem[0x20021] = 0xBE;
        // ModR/M: mod=00 reg=110(/6) rm=111([BX]) = 0x37
        bus.mem[0x100] = 0x37;
        cpu.execute(0xFF, &mut bus, M); // PUSH WORD [BX]
        let val = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        assert_eq!(val, 0xBEEF);
    }

    #[test]
    fn pop_rm16_mem() {
        let (mut cpu, mut bus) = setup();
        // Push a value first
        cpu.push16(&mut bus, M, 0xDEAD);
        cpu.bx = 0x0030;
        // ModR/M: mod=00 reg=000(/0) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x8F, &mut bus, M); // POP WORD [BX]
        assert_eq!(bus.mem[0x20030], 0xAD);
        assert_eq!(bus.mem[0x20031], 0xDE);
    }

    // =====================================================================
    // XCHG AX, reg16 (0x90-0x97)
    // =====================================================================

    #[test]
    fn nop() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        let old_ip = cpu.ip;
        cpu.execute(0x90, &mut bus, M); // NOP = XCHG AX, AX
        assert_eq!(cpu.ax, 0x1234);
        assert_eq!(cpu.ip, old_ip); // No operand bytes consumed
    }

    #[test]
    fn xchg_ax_cx() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1111;
        cpu.cx = 0x2222;
        cpu.execute(0x91, &mut bus, M); // XCHG AX, CX
        assert_eq!(cpu.ax, 0x2222);
        assert_eq!(cpu.cx, 0x1111);
    }

    // =====================================================================
    // XCHG r/m, reg (0x86-0x87)
    // =====================================================================

    #[test]
    fn xchg_rm8_reg8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xAA);
        cpu.set_bl(0xBB);
        // ModR/M: mod=11 reg=000(AL) rm=011(BL) = 0xC3
        bus.mem[0x100] = 0xC3;
        cpu.execute(0x86, &mut bus, M); // XCHG AL, BL
        assert_eq!(cpu.al(), 0xBB);
        assert_eq!(cpu.bl(), 0xAA);
    }

    #[test]
    fn xchg_rm16_reg16_mem() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        cpu.bx = 0x0050;
        bus.mem[0x20050] = 0x78;
        bus.mem[0x20051] = 0x56;
        // ModR/M: mod=00 reg=000(AX) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x87, &mut bus, M); // XCHG AX, [BX]
        assert_eq!(cpu.ax, 0x5678);
        assert_eq!(bus.mem[0x20050], 0x34);
        assert_eq!(bus.mem[0x20051], 0x12);
    }

    // =====================================================================
    // LEA (0x8D)
    // =====================================================================

    #[test]
    fn lea_reg16_bx_si() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x1000;
        cpu.si = 0x0234;
        // ModR/M: mod=00 reg=001(CX) rm=000([BX+SI]) = 0x08
        bus.mem[0x100] = 0x08;
        cpu.execute(0x8D, &mut bus, M); // LEA CX, [BX+SI]
        assert_eq!(cpu.cx, 0x1234);
    }

    #[test]
    fn lea_reg16_bp_disp8() {
        let (mut cpu, mut bus) = setup();
        cpu.bp = 0x0100;
        // ModR/M: mod=01 reg=010(DX) rm=110([BP+disp8]) = 0x56
        bus.mem[0x100] = 0x56;
        bus.mem[0x101] = 0x10; // disp8 = +16
        cpu.execute(0x8D, &mut bus, M); // LEA DX, [BP+16]
        assert_eq!(cpu.dx, 0x0110);
    }

    #[test]
    fn lea_direct() {
        let (mut cpu, mut bus) = setup();
        // ModR/M: mod=00 reg=000(AX) rm=110(direct) = 0x06
        bus.mem[0x100] = 0x06;
        bus.mem[0x101] = 0x00; // disp16 low
        bus.mem[0x102] = 0x80; // disp16 high = 0x8000
        cpu.execute(0x8D, &mut bus, M); // LEA AX, [0x8000]
        assert_eq!(cpu.ax, 0x8000);
    }

    // =====================================================================
    // LES (0xC4) / LDS (0xC5)
    // =====================================================================

    #[test]
    fn les_reg16() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0040;
        // Far pointer at DS:BX = 0x20040: offset=0x1234, segment=0x5000
        bus.mem[0x20040] = 0x34;
        bus.mem[0x20041] = 0x12;
        bus.mem[0x20042] = 0x00;
        bus.mem[0x20043] = 0x50;
        // ModR/M: mod=00 reg=001(CX) rm=111([BX]) = 0x0F
        bus.mem[0x100] = 0x0F;
        cpu.execute(0xC4, &mut bus, M); // LES CX, [BX]
        assert_eq!(cpu.cx, 0x1234);
        assert_eq!(cpu.es, 0x5000);
    }

    #[test]
    fn lds_reg16() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0060;
        // Far pointer at DS:SI = 0x20060: offset=0xABCD, segment=0x6000
        bus.mem[0x20060] = 0xCD;
        bus.mem[0x20061] = 0xAB;
        bus.mem[0x20062] = 0x00;
        bus.mem[0x20063] = 0x60;
        // ModR/M: mod=00 reg=010(DX) rm=100([SI]) = 0x14
        bus.mem[0x100] = 0x14;
        cpu.execute(0xC5, &mut bus, M); // LDS DX, [SI]
        assert_eq!(cpu.dx, 0xABCD);
        assert_eq!(cpu.ds, 0x6000);
    }

    // =====================================================================
    // SAHF (0x9E) / LAHF (0x9F)
    // =====================================================================

    #[test]
    fn sahf_loads_flags_from_ah() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self, Flag};
        // Set AH with CF=1, PF=1, ZF=1, SF=1 (bits 0,2,6,7 = 0xC5)
        cpu.set_ah(0xC5);
        cpu.execute(0x9E, &mut bus, M); // SAHF
        assert!(flags::get(cpu.flags, Flag::CF));
        assert!(flags::get(cpu.flags, Flag::PF));
        assert!(flags::get(cpu.flags, Flag::ZF));
        assert!(flags::get(cpu.flags, Flag::SF));
        assert!(!flags::get(cpu.flags, Flag::AF)); // bit 4 not set in 0xC5
        // Bit 1 always 1
        assert_ne!(cpu.flags & 0x0002, 0);
    }

    #[test]
    fn lahf_stores_flags_to_ah() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self, Flag};
        flags::set(&mut cpu.flags, Flag::CF, true);
        flags::set(&mut cpu.flags, Flag::ZF, true);
        cpu.execute(0x9F, &mut bus, M); // LAHF
        let ah = cpu.ah();
        assert_ne!(ah & 0x01, 0); // CF
        assert_ne!(ah & 0x40, 0); // ZF
    }

    // =====================================================================
    // XLAT (0xD7)
    // =====================================================================

    #[test]
    fn xlat_table_lookup() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0100;
        cpu.set_al(0x05);
        // Translation table at DS:BX = 0x20100
        bus.mem[0x20105] = 0x42; // table[5] = 0x42
        cpu.execute(0xD7, &mut bus, M); // XLAT
        assert_eq!(cpu.al(), 0x42);
    }

    // =====================================================================
    // Segment override integration
    // =====================================================================

    #[test]
    fn mov_with_segment_override() {
        let (mut cpu, mut bus) = setup();
        cpu.segment_override = Some(SegReg::ES);
        cpu.es = 0x5000;
        cpu.bx = 0x0010;
        bus.mem[0x50010] = 0xAB; // Value at ES:BX
        // ModR/M: mod=00 reg=000(AL) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x8A, &mut bus, M); // MOV AL, ES:[BX]
        assert_eq!(cpu.al(), 0xAB);
    }

    // =====================================================================
    // MOV with displacement addressing modes
    // =====================================================================

    #[test]
    fn mov_reg_mem_disp8() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0100;
        // Value at DS:(BX+0x10) = DS:0x0110 = 0x20110
        bus.mem[0x20110] = 0x99;
        // ModR/M: mod=01 reg=000(AL) rm=111([BX+disp8]) = 0x47
        bus.mem[0x100] = 0x47;
        bus.mem[0x101] = 0x10; // disp8
        cpu.execute(0x8A, &mut bus, M); // MOV AL, [BX+0x10]
        assert_eq!(cpu.al(), 0x99);
    }

    #[test]
    fn mov_reg_mem_disp16() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0100;
        // Value at DS:(BX+0x1000) = DS:0x1100 = 0x21100
        bus.mem[0x21100] = 0x77;
        // ModR/M: mod=10 reg=001(CL) rm=111([BX+disp16]) = 0x8F
        bus.mem[0x100] = 0x8F;
        bus.mem[0x101] = 0x00; // disp16 low
        bus.mem[0x102] = 0x10; // disp16 high = 0x1000
        cpu.execute(0x8A, &mut bus, M); // MOV CL, [BX+0x1000]
        assert_eq!(cpu.cl(), 0x77);
    }

    #[test]
    fn mov_mem_direct_imm16() {
        let (mut cpu, mut bus) = setup();
        // ModR/M: mod=00 reg=000 rm=110(direct) = 0x06
        bus.mem[0x100] = 0x06;
        bus.mem[0x101] = 0x50; // address low
        bus.mem[0x102] = 0x00; // address high = 0x0050
        bus.mem[0x103] = 0xEF; // imm16 low
        bus.mem[0x104] = 0xBE; // imm16 high = 0xBEEF
        cpu.execute(0xC7, &mut bus, M); // MOV WORD [0x0050], 0xBEEF
        assert_eq!(bus.mem[0x20050], 0xEF);
        assert_eq!(bus.mem[0x20051], 0xBE);
    }

    // =====================================================================
    // ADD r/m8, reg8 (0x00) and ADD reg8, r/m8 (0x02)
    // =====================================================================

    #[test]
    fn add_rm8_reg8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x10);
        cpu.set_bl(0x20);
        // ModR/M: mod=11 reg=000(AL) rm=011(BL) = 0xC3
        bus.mem[0x100] = 0xC3;
        cpu.execute(0x00, &mut bus, M); // ADD BL, AL
        assert_eq!(cpu.bl(), 0x30);
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert!(!fl::get(cpu.flags, Flag::OF));
    }

    #[test]
    fn add_reg8_rm8_carry() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xFF);
        cpu.set_cl(0x01);
        // ModR/M: mod=11 reg=000(AL) rm=001(CL) = 0xC1
        bus.mem[0x100] = 0xC1;
        cpu.execute(0x02, &mut bus, M); // ADD AL, CL
        assert_eq!(cpu.al(), 0x00);
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    // =====================================================================
    // ADD r/m16, reg16 (0x01) and ADD reg16, r/m16 (0x03)
    // =====================================================================

    #[test]
    fn add_rm16_reg16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1000;
        cpu.bx = 0x0050;
        cpu.cx = 0x2000;
        bus.mem[0x20050] = 0x34;
        bus.mem[0x20051] = 0x12;
        // ModR/M: mod=00 reg=010(DX) rm=111([BX]) = 0x17; actually we use AX
        // ModR/M: mod=00 reg=000(AX) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x01, &mut bus, M); // ADD [BX], AX
        assert_eq!(bus.mem[0x20050], 0x34); // 0x1234 + 0x1000 = 0x2234
        // Read back the result
        let lo = bus.mem[0x20050] as u16;
        let hi = bus.mem[0x20051] as u16;
        assert_eq!((hi << 8) | lo, 0x2234);
    }

    // =====================================================================
    // ADD AL, imm8 (0x04) and ADD AX, imm16 (0x05)
    // =====================================================================

    #[test]
    fn add_al_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x40);
        bus.mem[0x100] = 0x02; // imm8
        cpu.execute(0x04, &mut bus, M); // ADD AL, 0x02
        assert_eq!(cpu.al(), 0x42);
        assert!(!fl::get(cpu.flags, Flag::CF));
    }

    #[test]
    fn add_ax_imm16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1000;
        bus.mem[0x100] = 0x34;
        bus.mem[0x101] = 0x02;
        cpu.execute(0x05, &mut bus, M); // ADD AX, 0x0234
        assert_eq!(cpu.ax, 0x1234);
    }

    // =====================================================================
    // ADC (0x10-0x15)
    // =====================================================================

    #[test]
    fn adc_al_imm8_with_carry() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.set_al(0x10);
        bus.mem[0x100] = 0x20;
        cpu.execute(0x14, &mut bus, M); // ADC AL, 0x20
        assert_eq!(cpu.al(), 0x31); // 0x10 + 0x20 + CF(1) = 0x31
    }

    // =====================================================================
    // SUB (0x28-0x2D)
    // =====================================================================

    #[test]
    fn sub_al_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x30);
        bus.mem[0x100] = 0x10;
        cpu.execute(0x2C, &mut bus, M); // SUB AL, 0x10
        assert_eq!(cpu.al(), 0x20);
        assert!(!fl::get(cpu.flags, Flag::CF));
    }

    #[test]
    fn sub_al_imm8_borrow() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x00);
        bus.mem[0x100] = 0x01;
        cpu.execute(0x2C, &mut bus, M); // SUB AL, 0x01
        assert_eq!(cpu.al(), 0xFF);
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::SF));
    }

    // =====================================================================
    // SBB (0x18-0x1D)
    // =====================================================================

    #[test]
    fn sbb_al_imm8_with_borrow() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.set_al(0x30);
        bus.mem[0x100] = 0x10;
        cpu.execute(0x1C, &mut bus, M); // SBB AL, 0x10
        assert_eq!(cpu.al(), 0x1F); // 0x30 - 0x10 - CF(1) = 0x1F
    }

    // =====================================================================
    // CMP (0x38-0x3D)
    // =====================================================================

    #[test]
    fn cmp_al_imm8_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x42);
        bus.mem[0x100] = 0x42;
        cpu.execute(0x3C, &mut bus, M); // CMP AL, 0x42
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert_eq!(cpu.al(), 0x42); // AL unchanged
    }

    #[test]
    fn cmp_al_imm8_less() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x10);
        bus.mem[0x100] = 0x20;
        cpu.execute(0x3C, &mut bus, M); // CMP AL, 0x20
        assert!(!fl::get(cpu.flags, Flag::ZF));
        assert!(fl::get(cpu.flags, Flag::CF)); // borrow
        assert_eq!(cpu.al(), 0x10); // unchanged
    }

    #[test]
    fn cmp_ax_imm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x1234;
        bus.mem[0x100] = 0x34;
        bus.mem[0x101] = 0x12;
        cpu.execute(0x3D, &mut bus, M); // CMP AX, 0x1234
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.ax, 0x1234);
    }

    // =====================================================================
    // AND (0x20-0x25)
    // =====================================================================

    #[test]
    fn and_al_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xFF);
        bus.mem[0x100] = 0x0F;
        cpu.execute(0x24, &mut bus, M); // AND AL, 0x0F
        assert_eq!(cpu.al(), 0x0F);
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert!(!fl::get(cpu.flags, Flag::OF));
    }

    // =====================================================================
    // OR (0x08-0x0D)
    // =====================================================================

    #[test]
    fn or_al_imm8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xF0);
        bus.mem[0x100] = 0x0F;
        cpu.execute(0x0C, &mut bus, M); // OR AL, 0x0F
        assert_eq!(cpu.al(), 0xFF);
    }

    // =====================================================================
    // XOR (0x30-0x35)
    // =====================================================================

    #[test]
    fn xor_al_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xFF);
        bus.mem[0x100] = 0xFF;
        cpu.execute(0x34, &mut bus, M); // XOR AL, 0xFF
        assert_eq!(cpu.al(), 0x00);
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    // =====================================================================
    // INC reg16 (0x40-0x47) / DEC reg16 (0x48-0x4F)
    // =====================================================================

    #[test]
    fn inc_reg16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x00FF;
        cpu.execute(0x40, &mut bus, M); // INC AX
        assert_eq!(cpu.ax, 0x0100);
        assert!(!fl::get(cpu.flags, Flag::ZF));
    }

    #[test]
    fn inc_reg16_overflow() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x7FFF;
        cpu.execute(0x40, &mut bus, M); // INC AX
        assert_eq!(cpu.ax, 0x8000);
        assert!(fl::get(cpu.flags, Flag::OF));
        assert!(fl::get(cpu.flags, Flag::SF));
    }

    #[test]
    fn inc_preserves_cf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.ax = 0x0001;
        cpu.execute(0x40, &mut bus, M); // INC AX
        assert_eq!(cpu.ax, 0x0002);
        assert!(fl::get(cpu.flags, Flag::CF)); // CF preserved
    }

    #[test]
    fn dec_reg16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.cx = 0x0001;
        cpu.execute(0x49, &mut bus, M); // DEC CX
        assert_eq!(cpu.cx, 0x0000);
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    #[test]
    fn dec_all_reg16() {
        let (mut cpu, mut bus) = setup();
        for reg in 0..8u8 {
            cpu.set_reg16(reg, 0x1000);
            cpu.execute(0x48 + reg, &mut bus, M);
            assert_eq!(cpu.get_reg16(reg), 0x0FFF);
        }
    }

    // =====================================================================
    // Immediate ALU group (0x80-0x83)
    // =====================================================================

    #[test]
    fn imm_add_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x10);
        // ModR/M: mod=11 reg=000(/0=ADD) rm=000(AL) = 0xC0
        bus.mem[0x100] = 0xC0;
        bus.mem[0x101] = 0x20; // imm8
        cpu.execute(0x80, &mut bus, M); // ADD AL, 0x20
        assert_eq!(cpu.al(), 0x30);
    }

    #[test]
    fn imm_sub_rm16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        // ModR/M: mod=11 reg=101(/5=SUB) rm=000(AX) = 0xE8
        bus.mem[0x100] = 0xE8;
        bus.mem[0x101] = 0x34;
        bus.mem[0x102] = 0x02; // imm16 = 0x0234
        cpu.execute(0x81, &mut bus, M); // SUB AX, 0x0234
        assert_eq!(cpu.ax, 0x1000);
    }

    #[test]
    fn imm_cmp_rm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_bl(0x42);
        // ModR/M: mod=11 reg=111(/7=CMP) rm=011(BL) = 0xFB
        bus.mem[0x100] = 0xFB;
        bus.mem[0x101] = 0x42; // imm8
        cpu.execute(0x80, &mut bus, M); // CMP BL, 0x42
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.bl(), 0x42); // unchanged
    }

    #[test]
    fn imm_add_rm16_sign_ext() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x0100;
        // ModR/M: mod=11 reg=000(/0=ADD) rm=000(AX) = 0xC0
        bus.mem[0x100] = 0xC0;
        bus.mem[0x101] = 0xFE; // imm8 = -2 sign-extended = 0xFFFE
        cpu.execute(0x83, &mut bus, M); // ADD AX, -2
        assert_eq!(cpu.ax, 0x00FE);
    }

    #[test]
    fn imm_sub_rm16_sign_ext() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x0100;
        // ModR/M: mod=11 reg=101(/5=SUB) rm=000(AX) = 0xE8
        bus.mem[0x100] = 0xE8;
        bus.mem[0x101] = 0x02; // imm8 = +2 sign-extended = 0x0002
        cpu.execute(0x83, &mut bus, M); // SUB AX, 2
        assert_eq!(cpu.ax, 0x00FE);
    }

    // =====================================================================
    // TEST AL/AX, imm (0xA8-0xA9)
    // =====================================================================

    #[test]
    fn test_al_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xF0);
        bus.mem[0x100] = 0x0F;
        cpu.execute(0xA8, &mut bus, M); // TEST AL, 0x0F
        assert!(fl::get(cpu.flags, Flag::ZF)); // 0xF0 & 0x0F = 0
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert_eq!(cpu.al(), 0xF0); // unchanged
    }

    #[test]
    fn test_ax_imm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0xFF00;
        bus.mem[0x100] = 0x00;
        bus.mem[0x101] = 0xFF;
        cpu.execute(0xA9, &mut bus, M); // TEST AX, 0xFF00
        assert!(!fl::get(cpu.flags, Flag::ZF)); // 0xFF00 & 0xFF00 != 0
        assert!(fl::get(cpu.flags, Flag::SF));
        assert_eq!(cpu.ax, 0xFF00); // unchanged
    }

    // =====================================================================
    // TEST r/m, imm (0xF6 /0, 0xF7 /0)
    // =====================================================================

    #[test]
    fn test_rm8_imm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xAA);
        // ModR/M: mod=11 reg=000(/0=TEST) rm=000(AL) = 0xC0
        bus.mem[0x100] = 0xC0;
        bus.mem[0x101] = 0x55; // imm8
        cpu.execute(0xF6, &mut bus, M); // TEST AL, 0x55
        assert!(fl::get(cpu.flags, Flag::ZF)); // 0xAA & 0x55 = 0
        assert_eq!(cpu.al(), 0xAA); // unchanged
    }

    // =====================================================================
    // NOT (0xF6 /2, 0xF7 /2)
    // =====================================================================

    #[test]
    fn not_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xA5);
        // ModR/M: mod=11 reg=010(/2=NOT) rm=000(AL) = 0xD0
        bus.mem[0x100] = 0xD0;
        cpu.execute(0xF6, &mut bus, M); // NOT AL
        assert_eq!(cpu.al(), 0x5A);
    }

    #[test]
    fn not_rm16() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xFF00;
        // ModR/M: mod=11 reg=010(/2=NOT) rm=000(AX) = 0xD0
        bus.mem[0x100] = 0xD0;
        cpu.execute(0xF7, &mut bus, M); // NOT AX
        assert_eq!(cpu.ax, 0x00FF);
    }

    // =====================================================================
    // NEG (0xF6 /3, 0xF7 /3)
    // =====================================================================

    #[test]
    fn neg_rm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x01);
        // ModR/M: mod=11 reg=011(/3=NEG) rm=000(AL) = 0xD8
        bus.mem[0x100] = 0xD8;
        cpu.execute(0xF6, &mut bus, M); // NEG AL
        assert_eq!(cpu.al(), 0xFF);
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::SF));
    }

    #[test]
    fn neg_rm8_zero() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x00);
        bus.mem[0x100] = 0xD8;
        cpu.execute(0xF6, &mut bus, M); // NEG AL
        assert_eq!(cpu.al(), 0x00);
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    #[test]
    fn neg_rm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x0001;
        bus.mem[0x100] = 0xD8;
        cpu.execute(0xF7, &mut bus, M); // NEG AX
        assert_eq!(cpu.ax, 0xFFFF);
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // INC/DEC r/m8 (0xFE /0, /1)
    // =====================================================================

    #[test]
    fn inc_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x7F);
        // ModR/M: mod=11 reg=000(/0=INC) rm=000(AL) = 0xC0
        bus.mem[0x100] = 0xC0;
        cpu.execute(0xFE, &mut bus, M); // INC AL
        assert_eq!(cpu.al(), 0x80);
    }

    #[test]
    fn dec_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x01);
        // ModR/M: mod=11 reg=001(/1=DEC) rm=000(AL) = 0xC8
        bus.mem[0x100] = 0xC8;
        cpu.execute(0xFE, &mut bus, M); // DEC AL
        assert_eq!(cpu.al(), 0x00);
    }

    // =====================================================================
    // INC/DEC r/m16 (0xFF /0, /1)
    // =====================================================================

    #[test]
    fn inc_rm16_mem() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0050;
        bus.mem[0x20050] = 0xFF;
        bus.mem[0x20051] = 0x00; // [BX] = 0x00FF
        // ModR/M: mod=00 reg=000(/0=INC) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0xFF, &mut bus, M); // INC WORD [BX]
        assert_eq!(bus.mem[0x20050], 0x00);
        assert_eq!(bus.mem[0x20051], 0x01); // 0x00FF → 0x0100
    }

    #[test]
    fn dec_rm16_reg() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x0100;
        // ModR/M: mod=11 reg=001(/1=DEC) rm=000(AX) = 0xC8
        bus.mem[0x100] = 0xC8;
        cpu.execute(0xFF, &mut bus, M); // DEC AX
        assert_eq!(cpu.ax, 0x00FF);
    }

    // =====================================================================
    // JMP short (0xEB)
    // =====================================================================

    #[test]
    fn jmp_short_forward() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x10; // disp8 = +16
        cpu.execute(0xEB, &mut bus, M);
        // IP was 0x100, fetch consumed 1 byte → IP=0x101, then +16 = 0x111
        assert_eq!(cpu.ip, 0x0111);
    }

    #[test]
    fn jmp_short_backward() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0xFE_u8; // disp8 = -2 (signed)
        cpu.execute(0xEB, &mut bus, M);
        // IP was 0x100, fetch consumed 1 byte → IP=0x101, then -2 = 0xFF
        assert_eq!(cpu.ip, 0x00FF);
    }

    // =====================================================================
    // JMP near (0xE9)
    // =====================================================================

    #[test]
    fn jmp_near() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x00; // disp16 low
        bus.mem[0x101] = 0x10; // disp16 high = 0x1000
        cpu.execute(0xE9, &mut bus, M);
        // IP=0x100, fetch word consumed 2 → IP=0x102, then +0x1000 = 0x1102
        assert_eq!(cpu.ip, 0x1102);
    }

    #[test]
    fn jmp_near_backward() {
        let (mut cpu, mut bus) = setup();
        // disp16 = -0x0050 = 0xFFB0
        bus.mem[0x100] = 0xB0;
        bus.mem[0x101] = 0xFF;
        cpu.execute(0xE9, &mut bus, M);
        // IP=0x102 + 0xFFB0 = 0x00B2 (wrapping 16-bit)
        assert_eq!(cpu.ip, 0x00B2);
    }

    // =====================================================================
    // JMP far (0xEA)
    // =====================================================================

    #[test]
    fn jmp_far() {
        let (mut cpu, mut bus) = setup();
        bus.mem[0x100] = 0x00; // offset low
        bus.mem[0x101] = 0x01; // offset high = 0x0100
        bus.mem[0x102] = 0x00; // segment low
        bus.mem[0x103] = 0xF0; // segment high = 0xF000
        cpu.execute(0xEA, &mut bus, M);
        assert_eq!(cpu.ip, 0x0100);
        assert_eq!(cpu.cs, 0xF000);
    }

    // =====================================================================
    // JMP near indirect (0xFF /4), JMP far indirect (0xFF /5)
    // =====================================================================

    #[test]
    fn jmp_near_indirect_reg() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x5678;
        // ModR/M: mod=11 reg=100(/4=JMP near) rm=000(AX) = 0xE0
        bus.mem[0x100] = 0xE0;
        cpu.execute(0xFF, &mut bus, M);
        assert_eq!(cpu.ip, 0x5678);
    }

    #[test]
    fn jmp_near_indirect_mem() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0050;
        // Target address stored at DS:BX = 0x20050
        bus.mem[0x20050] = 0x34;
        bus.mem[0x20051] = 0x12; // target = 0x1234
        // ModR/M: mod=00 reg=100(/4) rm=111([BX]) = 0x27
        bus.mem[0x100] = 0x27;
        cpu.execute(0xFF, &mut bus, M);
        assert_eq!(cpu.ip, 0x1234);
    }

    #[test]
    fn jmp_far_indirect_mem() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0060;
        // Far pointer at DS:BX = 0x20060
        bus.mem[0x20060] = 0x00; // offset low
        bus.mem[0x20061] = 0x02; // offset high = 0x0200
        bus.mem[0x20062] = 0x00; // segment low
        bus.mem[0x20063] = 0xA0; // segment high = 0xA000
        // ModR/M: mod=00 reg=101(/5) rm=111([BX]) = 0x2F
        bus.mem[0x100] = 0x2F;
        cpu.execute(0xFF, &mut bus, M);
        assert_eq!(cpu.ip, 0x0200);
        assert_eq!(cpu.cs, 0xA000);
    }

    // =====================================================================
    // CALL near (0xE8)
    // =====================================================================

    #[test]
    fn call_near() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        bus.mem[0x100] = 0x00; // disp16 low
        bus.mem[0x101] = 0x05; // disp16 high = 0x0500
        cpu.execute(0xE8, &mut bus, M);
        // IP=0x100, fetch word → IP=0x102, push 0x102, then IP = 0x102 + 0x0500 = 0x0602
        assert_eq!(cpu.ip, 0x0602);
        assert_eq!(cpu.sp, old_sp.wrapping_sub(2));
        // Verify return address on stack
        let ret_addr = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        assert_eq!(ret_addr, 0x0102);
    }

    // =====================================================================
    // CALL far (0x9A)
    // =====================================================================

    #[test]
    fn call_far() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        let old_cs = cpu.cs;
        bus.mem[0x100] = 0x00; // offset low
        bus.mem[0x101] = 0x02; // offset high = 0x0200
        bus.mem[0x102] = 0x00; // segment low
        bus.mem[0x103] = 0xB0; // segment high = 0xB000
        cpu.execute(0x9A, &mut bus, M);
        assert_eq!(cpu.ip, 0x0200);
        assert_eq!(cpu.cs, 0xB000);
        assert_eq!(cpu.sp, old_sp.wrapping_sub(4));
        // Check stack: first CS, then IP (both pushed)
        let stacked_ip = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        let stacked_cs = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp.wrapping_add(2));
        assert_eq!(stacked_ip, 0x0104); // IP after fetching 4 bytes
        assert_eq!(stacked_cs, old_cs);
    }

    // =====================================================================
    // CALL near indirect (0xFF /2), CALL far indirect (0xFF /3)
    // =====================================================================

    #[test]
    fn call_near_indirect_reg() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x4000;
        // ModR/M: mod=11 reg=010(/2=CALL near) rm=000(AX) = 0xD0
        bus.mem[0x100] = 0xD0;
        cpu.execute(0xFF, &mut bus, M);
        assert_eq!(cpu.ip, 0x4000);
        // Return address pushed
        let ret_addr = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        assert_eq!(ret_addr, 0x0101); // IP after ModR/M
    }

    #[test]
    fn call_far_indirect_mem() {
        let (mut cpu, mut bus) = setup();
        let old_cs = cpu.cs;
        cpu.bx = 0x0070;
        // Far pointer at DS:BX = 0x20070
        bus.mem[0x20070] = 0x00;
        bus.mem[0x20071] = 0x03; // new IP = 0x0300
        bus.mem[0x20072] = 0x00;
        bus.mem[0x20073] = 0xC0; // new CS = 0xC000
        // ModR/M: mod=00 reg=011(/3=CALL far) rm=111([BX]) = 0x1F
        bus.mem[0x100] = 0x1F;
        cpu.execute(0xFF, &mut bus, M);
        assert_eq!(cpu.ip, 0x0300);
        assert_eq!(cpu.cs, 0xC000);
        // Verify stacked CS:IP
        let stacked_ip = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        let stacked_cs = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp.wrapping_add(2));
        assert_eq!(stacked_ip, 0x0101); // IP after ModR/M
        assert_eq!(stacked_cs, old_cs);
    }

    // =====================================================================
    // RET near (0xC3) / RET near with imm16 (0xC2)
    // =====================================================================

    #[test]
    fn ret_near() {
        let (mut cpu, mut bus) = setup();
        // Simulate a CALL: push return address
        cpu.push16(&mut bus, M, 0x0200);
        cpu.execute(0xC3, &mut bus, M);
        assert_eq!(cpu.ip, 0x0200);
    }

    #[test]
    fn ret_near_imm16() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        cpu.push16(&mut bus, M, 0x0300);
        bus.mem[0x100] = 0x04; // imm16 low
        bus.mem[0x101] = 0x00; // imm16 high = 4
        cpu.execute(0xC2, &mut bus, M);
        assert_eq!(cpu.ip, 0x0300);
        // SP = old_sp (pushed 2, popped 2) + 4 = old_sp + 4
        assert_eq!(cpu.sp, old_sp.wrapping_add(4));
    }

    // =====================================================================
    // RETF (0xCB) / RETF with imm16 (0xCA)
    // =====================================================================

    #[test]
    fn retf() {
        let (mut cpu, mut bus) = setup();
        // Simulate a far CALL: push CS then IP
        cpu.push16(&mut bus, M, 0xF000); // CS
        cpu.push16(&mut bus, M, 0x1234); // IP
        cpu.execute(0xCB, &mut bus, M);
        assert_eq!(cpu.ip, 0x1234);
        assert_eq!(cpu.cs, 0xF000);
    }

    #[test]
    fn retf_imm16() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        cpu.push16(&mut bus, M, 0xA000); // CS
        cpu.push16(&mut bus, M, 0x5678); // IP
        bus.mem[0x100] = 0x06; // imm16 low
        bus.mem[0x101] = 0x00; // imm16 high = 6
        cpu.execute(0xCA, &mut bus, M);
        assert_eq!(cpu.ip, 0x5678);
        assert_eq!(cpu.cs, 0xA000);
        // SP: pushed 4, popped 4, then +6
        assert_eq!(cpu.sp, old_sp.wrapping_add(6));
    }

    // =====================================================================
    // CALL/RET round-trip
    // =====================================================================

    #[test]
    fn call_ret_near_roundtrip() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        // CALL near: disp = 0x0100
        bus.mem[0x100] = 0x00;
        bus.mem[0x101] = 0x01;
        cpu.execute(0xE8, &mut bus, M); // CALL 0x0100
        assert_eq!(cpu.ip, 0x0202); // 0x102 + 0x100
        // Now RET
        cpu.execute(0xC3, &mut bus, M);
        assert_eq!(cpu.ip, 0x0102); // return address
        assert_eq!(cpu.sp, old_sp);
    }

    #[test]
    fn call_ret_far_roundtrip() {
        let (mut cpu, mut bus) = setup();
        let old_sp = cpu.sp;
        let old_cs = cpu.cs;
        // CALL far: offset=0x0300, segment=0xD000
        bus.mem[0x100] = 0x00;
        bus.mem[0x101] = 0x03;
        bus.mem[0x102] = 0x00;
        bus.mem[0x103] = 0xD0;
        cpu.execute(0x9A, &mut bus, M);
        assert_eq!(cpu.ip, 0x0300);
        assert_eq!(cpu.cs, 0xD000);
        // Now RETF
        cpu.execute(0xCB, &mut bus, M);
        assert_eq!(cpu.ip, 0x0104);
        assert_eq!(cpu.cs, old_cs);
        assert_eq!(cpu.sp, old_sp);
    }

    // =====================================================================
    // Jcc conditional jumps (0x70-0x7F)
    // =====================================================================

    #[test]
    fn jo_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::OF, true);
        bus.mem[0x100] = 0x10; // disp8 = +16
        cpu.execute(0x70, &mut bus, M); // JO
        assert_eq!(cpu.ip, 0x0111);
    }

    #[test]
    fn jo_not_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x10;
        cpu.execute(0x70, &mut bus, M); // JO
        assert_eq!(cpu.ip, 0x0101); // only the disp byte consumed
    }

    #[test]
    fn jno_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x10;
        cpu.execute(0x71, &mut bus, M); // JNO
        assert_eq!(cpu.ip, 0x0111);
    }

    #[test]
    fn jb_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        bus.mem[0x100] = 0x20;
        cpu.execute(0x72, &mut bus, M); // JB/JC
        assert_eq!(cpu.ip, 0x0121);
    }

    #[test]
    fn jnb_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, false);
        bus.mem[0x100] = 0x20;
        cpu.execute(0x73, &mut bus, M); // JNB/JNC
        assert_eq!(cpu.ip, 0x0121);
    }

    #[test]
    fn jz_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::ZF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x74, &mut bus, M); // JZ/JE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jnz_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x75, &mut bus, M); // JNZ/JNE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jbe_taken_zf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::ZF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x76, &mut bus, M); // JBE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jbe_taken_cf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x76, &mut bus, M); // JBE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn ja_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, false);
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x77, &mut bus, M); // JA/JNBE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn ja_not_taken_cf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x77, &mut bus, M); // JA not taken (CF=1)
        assert_eq!(cpu.ip, 0x0101);
    }

    #[test]
    fn js_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::SF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x78, &mut bus, M); // JS
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jns_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::SF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x79, &mut bus, M); // JNS
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jp_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::PF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7A, &mut bus, M); // JP/JPE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jnp_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::PF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7B, &mut bus, M); // JNP/JPO
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jl_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // JL: SF != OF
        fl::set(&mut cpu.flags, Flag::SF, true);
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7C, &mut bus, M); // JL
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jl_not_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // JL not taken when SF == OF
        fl::set(&mut cpu.flags, Flag::SF, true);
        fl::set(&mut cpu.flags, Flag::OF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7C, &mut bus, M);
        assert_eq!(cpu.ip, 0x0101);
    }

    #[test]
    fn jge_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // JGE: SF == OF
        fl::set(&mut cpu.flags, Flag::SF, false);
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7D, &mut bus, M); // JGE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jle_taken_zf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::ZF, true);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7E, &mut bus, M); // JLE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jle_taken_sf_ne_of() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::SF, true);
        fl::set(&mut cpu.flags, Flag::OF, false);
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7E, &mut bus, M); // JLE
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jg_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // JG: ZF=0 and SF==OF
        fl::set(&mut cpu.flags, Flag::ZF, false);
        fl::set(&mut cpu.flags, Flag::SF, false);
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7F, &mut bus, M); // JG
        assert_eq!(cpu.ip, 0x0106);
    }

    #[test]
    fn jg_not_taken_zf() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // JG not taken when ZF=1
        fl::set(&mut cpu.flags, Flag::ZF, true);
        fl::set(&mut cpu.flags, Flag::SF, false);
        fl::set(&mut cpu.flags, Flag::OF, false);
        bus.mem[0x100] = 0x05;
        cpu.execute(0x7F, &mut bus, M);
        assert_eq!(cpu.ip, 0x0101);
    }

    // =====================================================================
    // LOOP (0xE2), LOOPZ (0xE1), LOOPNZ (0xE0), JCXZ (0xE3)
    // =====================================================================

    #[test]
    fn loop_basic() {
        let (mut cpu, mut bus) = setup();
        cpu.cx = 3;
        bus.mem[0x100] = 0xFE_u8; // disp8 = -2
        cpu.execute(0xE2, &mut bus, M); // LOOP
        assert_eq!(cpu.cx, 2);
        assert_eq!(cpu.ip, 0x00FF); // 0x101 - 2
    }

    #[test]
    fn loop_falls_through_when_cx_zero() {
        let (mut cpu, mut bus) = setup();
        cpu.cx = 1; // will decrement to 0
        bus.mem[0x100] = 0xFE_u8;
        cpu.execute(0xE2, &mut bus, M);
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.ip, 0x0101); // no jump
    }

    #[test]
    fn loopz_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.cx = 5;
        fl::set(&mut cpu.flags, Flag::ZF, true);
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE1, &mut bus, M); // LOOPZ
        assert_eq!(cpu.cx, 4);
        assert_eq!(cpu.ip, 0x0111);
    }

    #[test]
    fn loopz_not_taken_zf_clear() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.cx = 5;
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE1, &mut bus, M); // LOOPZ
        assert_eq!(cpu.cx, 4);
        assert_eq!(cpu.ip, 0x0101); // no jump
    }

    #[test]
    fn loopnz_taken() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.cx = 5;
        fl::set(&mut cpu.flags, Flag::ZF, false);
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE0, &mut bus, M); // LOOPNZ
        assert_eq!(cpu.cx, 4);
        assert_eq!(cpu.ip, 0x0111);
    }

    #[test]
    fn loopnz_not_taken_zf_set() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.cx = 5;
        fl::set(&mut cpu.flags, Flag::ZF, true);
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE0, &mut bus, M); // LOOPNZ
        assert_eq!(cpu.cx, 4);
        assert_eq!(cpu.ip, 0x0101); // no jump
    }

    #[test]
    fn jcxz_taken() {
        let (mut cpu, mut bus) = setup();
        cpu.cx = 0;
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE3, &mut bus, M); // JCXZ
        assert_eq!(cpu.ip, 0x0111);
        assert_eq!(cpu.cx, 0); // unchanged
    }

    #[test]
    fn jcxz_not_taken() {
        let (mut cpu, mut bus) = setup();
        cpu.cx = 1;
        bus.mem[0x100] = 0x10;
        cpu.execute(0xE3, &mut bus, M); // JCXZ
        assert_eq!(cpu.ip, 0x0101); // no jump
        assert_eq!(cpu.cx, 1); // unchanged
    }

    // =====================================================================
    // ALU with memory operand
    // =====================================================================

    #[test]
    fn add_mem_reg8() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0050;
        cpu.set_al(0x10);
        bus.mem[0x20050] = 0x20; // [DS:BX] = 0x20
        // ModR/M: mod=00 reg=000(AL) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x00, &mut bus, M); // ADD [BX], AL
        assert_eq!(bus.mem[0x20050], 0x30);
    }

    #[test]
    fn cmp_reg16_rm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x5000;
        cpu.bx = 0x0060;
        bus.mem[0x20060] = 0x00;
        bus.mem[0x20061] = 0x50; // [BX] = 0x5000
        // ModR/M: mod=00 reg=000(AX) rm=111([BX]) = 0x07
        bus.mem[0x100] = 0x07;
        cpu.execute(0x3B, &mut bus, M); // CMP AX, [BX]
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.ax, 0x5000); // unchanged
    }

    // =====================================================================
    // ADD overflow edge case: signed boundary
    // =====================================================================

    #[test]
    fn add8_signed_overflow_positive() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // 0x7F + 0x01 = 0x80 (127 + 1 = -128 in signed)
        cpu.set_al(0x7F);
        bus.mem[0x100] = 0x01;
        cpu.execute(0x04, &mut bus, M); // ADD AL, 0x01
        assert_eq!(cpu.al(), 0x80);
        assert!(fl::get(cpu.flags, Flag::OF));
        assert!(fl::get(cpu.flags, Flag::SF));
        assert!(fl::get(cpu.flags, Flag::AF));
    }

    #[test]
    fn sub8_signed_overflow_negative() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // 0x80 - 0x01 = 0x7F (-128 - 1 = 127 in signed)
        cpu.set_al(0x80);
        bus.mem[0x100] = 0x01;
        cpu.execute(0x2C, &mut bus, M); // SUB AL, 0x01
        assert_eq!(cpu.al(), 0x7F);
        assert!(fl::get(cpu.flags, Flag::OF));
        assert!(fl::get(cpu.flags, Flag::AF));
    }

    // =====================================================================
    // SHL r/m8, 1 (0xD0 /4) and SHL r/m8, CL (0xD2 /4)
    // =====================================================================

    #[test]
    fn shl_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x80);
        // ModR/M: mod=11 reg=100(/4=SHL) rm=000(AL) = 0xE0
        bus.mem[0x100] = 0xE0;
        cpu.execute(0xD0, &mut bus, M); // SHL AL, 1
        assert_eq!(cpu.al(), 0x00);
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    #[test]
    fn shl_rm8_by_cl() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x01);
        cpu.set_cl(4);
        bus.mem[0x100] = 0xE0;
        cpu.execute(0xD2, &mut bus, M); // SHL AL, CL
        assert_eq!(cpu.al(), 0x10);
    }

    // =====================================================================
    // SHR r/m8, 1 (0xD0 /5)
    // =====================================================================

    #[test]
    fn shr_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x03);
        // ModR/M: mod=11 reg=101(/5=SHR) rm=000(AL) = 0xE8
        bus.mem[0x100] = 0xE8;
        cpu.execute(0xD0, &mut bus, M); // SHR AL, 1
        assert_eq!(cpu.al(), 0x01);
        assert!(fl::get(cpu.flags, Flag::CF)); // bit 0 was 1
    }

    // =====================================================================
    // SAR r/m8, 1 (0xD0 /7)
    // =====================================================================

    #[test]
    fn sar_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x80); // -128
        // ModR/M: mod=11 reg=111(/7=SAR) rm=000(AL) = 0xF8
        bus.mem[0x100] = 0xF8;
        cpu.execute(0xD0, &mut bus, M); // SAR AL, 1
        assert_eq!(cpu.al(), 0xC0); // -64, sign preserved
        assert!(!fl::get(cpu.flags, Flag::CF));
        assert!(!fl::get(cpu.flags, Flag::OF));
        assert!(fl::get(cpu.flags, Flag::SF));
    }

    // =====================================================================
    // ROL r/m8, 1 (0xD0 /0)
    // =====================================================================

    #[test]
    fn rol_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x80);
        // ModR/M: mod=11 reg=000(/0=ROL) rm=000(AL) = 0xC0
        bus.mem[0x100] = 0xC0;
        cpu.execute(0xD0, &mut bus, M); // ROL AL, 1
        assert_eq!(cpu.al(), 0x01);
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // ROR r/m8, 1 (0xD0 /1)
    // =====================================================================

    #[test]
    fn ror_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x01);
        // ModR/M: mod=11 reg=001(/1=ROR) rm=000(AL) = 0xC8
        bus.mem[0x100] = 0xC8;
        cpu.execute(0xD0, &mut bus, M); // ROR AL, 1
        assert_eq!(cpu.al(), 0x80);
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // RCL r/m8, 1 (0xD0 /2)
    // =====================================================================

    #[test]
    fn rcl_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.set_al(0x00);
        // ModR/M: mod=11 reg=010(/2=RCL) rm=000(AL) = 0xD0
        bus.mem[0x100] = 0xD0;
        cpu.execute(0xD0, &mut bus, M); // RCL AL, 1
        assert_eq!(cpu.al(), 0x01); // CF rotated in
        assert!(!fl::get(cpu.flags, Flag::CF)); // old bit 7 (0) → CF
    }

    // =====================================================================
    // RCR r/m8, 1 (0xD0 /3)
    // =====================================================================

    #[test]
    fn rcr_rm8_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.set_al(0x00);
        // ModR/M: mod=11 reg=011(/3=RCR) rm=000(AL) = 0xD8
        bus.mem[0x100] = 0xD8;
        cpu.execute(0xD0, &mut bus, M); // RCR AL, 1
        assert_eq!(cpu.al(), 0x80); // CF rotated into MSB
        assert!(!fl::get(cpu.flags, Flag::CF)); // old bit 0 (0) → CF
    }

    // =====================================================================
    // Shift/rotate 16-bit (0xD1, 0xD3)
    // =====================================================================

    #[test]
    fn shl_rm16_by_1() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x8000;
        // ModR/M: mod=11 reg=100(/4=SHL) rm=000(AX) = 0xE0
        bus.mem[0x100] = 0xE0;
        cpu.execute(0xD1, &mut bus, M); // SHL AX, 1
        assert_eq!(cpu.ax, 0x0000);
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    #[test]
    fn shr_rm16_by_cl() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xFF00;
        cpu.set_cl(8);
        bus.mem[0x100] = 0xE8; // mod=11 reg=101(/5=SHR) rm=000(AX)
        cpu.execute(0xD3, &mut bus, M); // SHR AX, CL
        assert_eq!(cpu.ax, 0x00FF);
    }

    // =====================================================================
    // MUL r/m8 (0xF6 /4)
    // =====================================================================

    #[test]
    fn mul_rm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x10);
        cpu.set_bl(0x10);
        // ModR/M: mod=11 reg=100(/4=MUL) rm=011(BL) = 0xE3
        bus.mem[0x100] = 0xE3;
        cpu.execute(0xF6, &mut bus, M); // MUL BL
        assert_eq!(cpu.ax, 0x0100); // 16 * 16 = 256
        assert!(fl::get(cpu.flags, Flag::CF)); // AH != 0
        assert!(fl::get(cpu.flags, Flag::OF));
    }

    #[test]
    fn mul_rm8_no_overflow() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x03);
        cpu.set_bl(0x04);
        bus.mem[0x100] = 0xE3;
        cpu.execute(0xF6, &mut bus, M); // MUL BL
        assert_eq!(cpu.ax, 0x000C); // 3 * 4 = 12
        assert!(!fl::get(cpu.flags, Flag::CF)); // AH == 0
    }

    // =====================================================================
    // IMUL r/m8 (0xF6 /5)
    // =====================================================================

    #[test]
    fn imul_rm8() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xFF); // -1 signed
        cpu.set_bl(0x02); // +2
        // ModR/M: mod=11 reg=101(/5=IMUL) rm=011(BL) = 0xEB
        bus.mem[0x100] = 0xEB;
        cpu.execute(0xF6, &mut bus, M); // IMUL BL
        assert_eq!(cpu.ax, 0xFFFE); // -2 as u16
        assert!(!fl::get(cpu.flags, Flag::CF)); // fits in 8-bit signed
    }

    #[test]
    fn imul_rm8_overflow() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x40); // +64
        cpu.set_bl(0x04); // +4
        bus.mem[0x100] = 0xEB;
        cpu.execute(0xF6, &mut bus, M); // IMUL BL
        assert_eq!(cpu.ax, 0x0100); // 64*4 = 256, doesn't fit in i8
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::OF));
    }

    // =====================================================================
    // MUL r/m16 (0xF7 /4)
    // =====================================================================

    #[test]
    fn mul_rm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x0100;
        cpu.bx = 0x0100;
        // ModR/M: mod=11 reg=100(/4=MUL) rm=011(BX) = 0xE3
        bus.mem[0x100] = 0xE3;
        cpu.execute(0xF7, &mut bus, M); // MUL BX
        assert_eq!(cpu.ax, 0x0000);
        assert_eq!(cpu.dx, 0x0001); // 256 * 256 = 65536
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::OF));
    }

    // =====================================================================
    // DIV r/m8 (0xF6 /6)
    // =====================================================================

    #[test]
    fn div_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x0064; // 100
        cpu.set_bl(0x0A); // 10
        // ModR/M: mod=11 reg=110(/6=DIV) rm=011(BL) = 0xF3
        bus.mem[0x100] = 0xF3;
        cpu.execute(0xF6, &mut bus, M); // DIV BL
        assert_eq!(cpu.al(), 0x0A); // 100 / 10 = 10
        assert_eq!(cpu.ah(), 0x00); // 100 % 10 = 0
    }

    #[test]
    fn div_rm8_with_remainder() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x000B; // 11
        cpu.set_bl(0x03); // 3
        bus.mem[0x100] = 0xF3;
        cpu.execute(0xF6, &mut bus, M);
        assert_eq!(cpu.al(), 0x03); // 11 / 3 = 3
        assert_eq!(cpu.ah(), 0x02); // 11 % 3 = 2
    }

    // =====================================================================
    // IDIV r/m8 (0xF6 /7)
    // =====================================================================

    #[test]
    fn idiv_rm8() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xFFF6; // -10 as i16
        cpu.set_bl(0x03); // +3
        // ModR/M: mod=11 reg=111(/7=IDIV) rm=011(BL) = 0xFB
        bus.mem[0x100] = 0xFB;
        cpu.execute(0xF6, &mut bus, M); // IDIV BL
        assert_eq!(cpu.al() as i8, -3); // -10 / 3 = -3
        assert_eq!(cpu.ah() as i8, -1); // -10 % 3 = -1
    }

    // =====================================================================
    // DIV r/m16 (0xF7 /6)
    // =====================================================================

    #[test]
    fn div_rm16() {
        let (mut cpu, mut bus) = setup();
        cpu.dx = 0x0000;
        cpu.ax = 0x03E8; // 1000
        cpu.bx = 0x000A; // 10
        // ModR/M: mod=11 reg=110(/6=DIV) rm=011(BX) = 0xF3
        bus.mem[0x100] = 0xF3;
        cpu.execute(0xF7, &mut bus, M); // DIV BX
        assert_eq!(cpu.ax, 0x0064); // 1000 / 10 = 100
        assert_eq!(cpu.dx, 0x0000); // 1000 % 10 = 0
    }

    #[test]
    fn div_rm16_large_dividend() {
        let (mut cpu, mut bus) = setup();
        cpu.dx = 0x0001;
        cpu.ax = 0x0000; // DX:AX = 0x10000 = 65536
        cpu.bx = 0x0100; // 256
        bus.mem[0x100] = 0xF3;
        cpu.execute(0xF7, &mut bus, M); // DIV BX
        assert_eq!(cpu.ax, 0x0100); // 65536 / 256 = 256
        assert_eq!(cpu.dx, 0x0000);
    }

    // =====================================================================
    // IMUL r/m16 (0xF7 /5)
    // =====================================================================

    #[test]
    fn imul_rm16() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0xFFFF; // -1 signed
        cpu.bx = 0x0002; // +2
        bus.mem[0x100] = 0xEB; // mod=11 reg=101(/5=IMUL) rm=011(BX)
        cpu.execute(0xF7, &mut bus, M); // IMUL BX
        // -1 * 2 = -2 → DX:AX = 0xFFFF:0xFFFE
        assert_eq!(cpu.ax, 0xFFFE);
        assert_eq!(cpu.dx, 0xFFFF);
        assert!(!fl::get(cpu.flags, Flag::CF)); // fits in i16
    }

    // =====================================================================
    // CBW (0x98) / CWD (0x99)
    // =====================================================================

    #[test]
    fn cbw_positive() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xFF42; // AL = 0x42 (positive)
        cpu.execute(0x98, &mut bus, M);
        assert_eq!(cpu.ax, 0x0042); // AH = 0x00
    }

    #[test]
    fn cbw_negative() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x0080; // AL = 0x80 (-128)
        cpu.execute(0x98, &mut bus, M);
        assert_eq!(cpu.ax, 0xFF80); // AH = 0xFF
    }

    #[test]
    fn cwd_positive() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        cpu.dx = 0xFFFF; // should be cleared
        cpu.execute(0x99, &mut bus, M);
        assert_eq!(cpu.dx, 0x0000);
    }

    #[test]
    fn cwd_negative() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x8000;
        cpu.dx = 0x0000; // should be set
        cpu.execute(0x99, &mut bus, M);
        assert_eq!(cpu.dx, 0xFFFF);
    }

    // =====================================================================
    // DAA (0x27)
    // =====================================================================

    #[test]
    fn daa_basic() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // 0x15 + 0x27 = 0x3C → DAA → 0x42 (BCD 15 + 27 = 42)
        cpu.set_al(0x15);
        bus.mem[0x100] = 0x27; // imm8 for ADD AL
        cpu.execute(0x04, &mut bus, M); // ADD AL, 0x27
        assert_eq!(cpu.al(), 0x3C);
        cpu.execute(0x27, &mut bus, M); // DAA
        assert_eq!(cpu.al(), 0x42);
        assert!(!fl::get(cpu.flags, Flag::CF));
    }

    #[test]
    fn daa_with_carry() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // BCD 99 + 1 = 100: 0x99 + 0x01 = 0x9A → DAA → 0x00, CF=1
        cpu.set_al(0x9A);
        cpu.execute(0x27, &mut bus, M); // DAA
        assert_eq!(cpu.al(), 0x00);
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // DAS (0x2F)
    // =====================================================================

    #[test]
    fn das_basic() {
        let (mut cpu, mut bus) = setup();
        // BCD 42 - 15 = 27: 0x42 - 0x15 = 0x2D → DAS → 0x27
        cpu.set_al(0x2D);
        cpu.execute(0x2F, &mut bus, M); // DAS
        assert_eq!(cpu.al(), 0x27);
    }

    // =====================================================================
    // AAA (0x37)
    // =====================================================================

    #[test]
    fn aaa_adjust() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // 9 + 5 = 14 = 0x0E → AAA → AH+=1, AL=4
        cpu.set_al(0x0E);
        cpu.set_ah(0x00);
        cpu.execute(0x37, &mut bus, M); // AAA
        assert_eq!(cpu.al(), 0x04);
        assert_eq!(cpu.ah(), 0x01);
        assert!(fl::get(cpu.flags, Flag::CF));
        assert!(fl::get(cpu.flags, Flag::AF));
    }

    #[test]
    fn aaa_no_adjust() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x05);
        cpu.set_ah(0x00);
        cpu.execute(0x37, &mut bus, M); // AAA
        assert_eq!(cpu.al(), 0x05);
        assert_eq!(cpu.ah(), 0x00);
        assert!(!fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // AAS (0x3F)
    // =====================================================================

    #[test]
    fn aas_adjust() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        // 10 - 5 = 5 but let's do 2 - 5 = -3 → borrow situation
        // After SUB: AL = 0xFD (if we did raw sub)
        // For AAS: AL & 0x0F = 0x0D > 9 → adjust
        cpu.set_al(0xFD);
        cpu.set_ah(0x02);
        cpu.execute(0x3F, &mut bus, M); // AAS
        assert_eq!(cpu.al(), 0x07); // 0xFD - 6 = 0xF7, & 0x0F = 0x07
        assert_eq!(cpu.ah(), 0x01); // 2 - 1 = 1
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    // =====================================================================
    // AAM (0xD4)
    // =====================================================================

    #[test]
    fn aam_basic() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0x2A); // 42 decimal
        bus.mem[0x100] = 0x0A; // base 10
        cpu.execute(0xD4, &mut bus, M); // AAM
        assert_eq!(cpu.ah(), 0x04); // 42 / 10 = 4
        assert_eq!(cpu.al(), 0x02); // 42 % 10 = 2
    }

    // =====================================================================
    // AAD (0xD5)
    // =====================================================================

    #[test]
    fn aad_basic() {
        let (mut cpu, mut bus) = setup();
        cpu.set_ah(0x04);
        cpu.set_al(0x02);
        bus.mem[0x100] = 0x0A; // base 10
        cpu.execute(0xD5, &mut bus, M); // AAD
        assert_eq!(cpu.al(), 0x2A); // 4*10 + 2 = 42
        assert_eq!(cpu.ah(), 0x00);
    }

    // =====================================================================
    // Shift with memory operand
    // =====================================================================

    #[test]
    fn shl_mem_by_1() {
        let (mut cpu, mut bus) = setup();
        cpu.bx = 0x0050;
        bus.mem[0x20050] = 0x42;
        // ModR/M: mod=00 reg=100(/4=SHL) rm=111([BX]) = 0x27
        bus.mem[0x100] = 0x27;
        cpu.execute(0xD0, &mut bus, M); // SHL BYTE [BX], 1
        assert_eq!(bus.mem[0x20050], 0x84);
    }

    #[test]
    fn shift_count_zero_unchanged() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.set_al(0xFF);
        cpu.set_cl(0); // count = 0 → no change
        bus.mem[0x100] = 0xE0;
        cpu.execute(0xD2, &mut bus, M); // SHL AL, CL
        assert_eq!(cpu.al(), 0xFF);
        assert!(fl::get(cpu.flags, Flag::CF)); // unchanged
    }

    // =====================================================================
    // String operations: MOVSB/MOVSW (0xA4-0xA5)
    // =====================================================================

    #[test]
    fn movsb_forward() {
        let (mut cpu, mut bus) = setup();
        // Source: DS:SI = 0x2000:0x0010 → physical 0x20010
        // Dest:   ES:DI = 0x4000:0x0020 → physical 0x40020
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0xAB;
        cpu.execute(0xA4, &mut bus, M); // MOVSB
        assert_eq!(bus.mem[0x40020], 0xAB);
        assert_eq!(cpu.si, 0x0011); // incremented
        assert_eq!(cpu.di, 0x0021); // incremented
    }

    #[test]
    fn movsb_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true); // direction = decrement
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0xCD;
        cpu.execute(0xA4, &mut bus, M); // MOVSB
        assert_eq!(bus.mem[0x40020], 0xCD);
        assert_eq!(cpu.si, 0x000F); // decremented
        assert_eq!(cpu.di, 0x001F); // decremented
    }

    #[test]
    fn movsw_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0x34; // low byte
        bus.mem[0x20011] = 0x12; // high byte
        cpu.execute(0xA5, &mut bus, M); // MOVSW
        assert_eq!(bus.mem[0x40020], 0x34);
        assert_eq!(bus.mem[0x40021], 0x12);
        assert_eq!(cpu.si, 0x0012); // +2
        assert_eq!(cpu.di, 0x0022); // +2
    }

    #[test]
    fn movsw_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0x34;
        bus.mem[0x20011] = 0x12;
        cpu.execute(0xA5, &mut bus, M); // MOVSW
        assert_eq!(bus.mem[0x40020], 0x34);
        assert_eq!(bus.mem[0x40021], 0x12);
        assert_eq!(cpu.si, 0x000E); // -2
        assert_eq!(cpu.di, 0x001E); // -2
    }

    // =====================================================================
    // REP MOVSB (block copy)
    // =====================================================================

    #[test]
    fn rep_movsb_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 5;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        // Write source data at DS:0 = phys 0x20000
        for i in 0..5u8 {
            bus.mem[0x20000 + i as usize] = 0x10 + i;
        }
        cpu.execute(0xA4, &mut bus, M); // REP MOVSB
        // Check destination at ES:0 = phys 0x40000
        for i in 0..5u8 {
            assert_eq!(bus.mem[0x40000 + i as usize], 0x10 + i);
        }
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.si, 0x0005);
        assert_eq!(cpu.di, 0x0005);
    }

    #[test]
    fn rep_movsb_cx_zero() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        cpu.cx = 0; // should do nothing
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        bus.mem[0x20010] = 0xFF;
        cpu.execute(0xA4, &mut bus, M); // REP MOVSB
        assert_eq!(bus.mem[0x40020], 0x00); // unchanged
        assert_eq!(cpu.si, 0x0010); // unchanged
        assert_eq!(cpu.di, 0x0020); // unchanged
    }

    #[test]
    fn rep_movsw_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 3;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        // 3 words at DS:0
        for i in 0..3u16 {
            let base = 0x20000 + (i as usize) * 2;
            bus.mem[base] = (0x1000 + i) as u8;
            bus.mem[base + 1] = ((0x1000 + i) >> 8) as u8;
        }
        cpu.execute(0xA5, &mut bus, M); // REP MOVSW
        for i in 0..3u16 {
            let base = 0x40000 + (i as usize) * 2;
            let val = bus.mem[base] as u16 | ((bus.mem[base + 1] as u16) << 8);
            assert_eq!(val, 0x1000 + i);
        }
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.si, 6);
        assert_eq!(cpu.di, 6);
    }

    #[test]
    fn rep_movsb_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.si = 0x0004;
        cpu.di = 0x0004;
        cpu.cx = 5;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        for i in 0..5u8 {
            bus.mem[0x20000 + i as usize] = 0xA0 + i;
        }
        cpu.execute(0xA4, &mut bus, M); // REP MOVSB backward
        for i in 0..5u8 {
            assert_eq!(bus.mem[0x40000 + i as usize], 0xA0 + i);
        }
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.si, 0xFFFF); // wrapped from 0x0004 - 5
        assert_eq!(cpu.di, 0xFFFF);
    }

    // =====================================================================
    // CMPSB/CMPSW (0xA6-0xA7)
    // =====================================================================

    #[test]
    fn cmpsb_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0x42;
        bus.mem[0x40020] = 0x42;
        cpu.execute(0xA6, &mut bus, M); // CMPSB
        assert!(fl::get(cpu.flags, Flag::ZF)); // equal
        assert_eq!(cpu.si, 0x0011);
        assert_eq!(cpu.di, 0x0021);
    }

    #[test]
    fn cmpsb_not_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        bus.mem[0x20010] = 0x50;
        bus.mem[0x40020] = 0x30;
        cpu.execute(0xA6, &mut bus, M); // CMPSB
        assert!(!fl::get(cpu.flags, Flag::ZF)); // not equal
        assert!(!fl::get(cpu.flags, Flag::CF)); // 0x50 > 0x30, no borrow
    }

    #[test]
    fn cmpsw_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        // Write 0x1234 at DS:SI
        bus.mem[0x20010] = 0x34;
        bus.mem[0x20011] = 0x12;
        // Write 0x1234 at ES:DI
        bus.mem[0x40020] = 0x34;
        bus.mem[0x40021] = 0x12;
        cpu.execute(0xA7, &mut bus, M); // CMPSW
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.si, 0x0012);
        assert_eq!(cpu.di, 0x0022);
    }

    // =====================================================================
    // REPZ CMPSB (block compare, stop on mismatch)
    // =====================================================================

    #[test]
    fn repz_cmpsb_all_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 4;
        cpu.rep_prefix = Some(super::RepPrefix::Rep); // REPZ
        // Same data in both regions
        for i in 0..4u8 {
            bus.mem[0x20000 + i as usize] = 0x41 + i;
            bus.mem[0x40000 + i as usize] = 0x41 + i;
        }
        cpu.execute(0xA6, &mut bus, M); // REPZ CMPSB
        assert_eq!(cpu.cx, 0); // exhausted all
        assert!(fl::get(cpu.flags, Flag::ZF)); // last compare was equal
    }

    #[test]
    fn repz_cmpsb_mismatch_at_3() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 5;
        cpu.rep_prefix = Some(super::RepPrefix::Rep); // REPZ
        for i in 0..5u8 {
            bus.mem[0x20000 + i as usize] = 0x41 + i;
            bus.mem[0x40000 + i as usize] = 0x41 + i;
        }
        bus.mem[0x40002] = 0xFF; // mismatch at index 2
        cpu.execute(0xA6, &mut bus, M);
        assert_eq!(cpu.cx, 2); // stopped after 3 iterations (5-3=2)
        assert!(!fl::get(cpu.flags, Flag::ZF)); // mismatch
        assert_eq!(cpu.si, 3);
        assert_eq!(cpu.di, 3);
    }

    #[test]
    fn repnz_cmpsb_find_match() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 5;
        cpu.rep_prefix = Some(super::RepPrefix::Repnz); // REPNZ
        for i in 0..5u8 {
            bus.mem[0x20000 + i as usize] = 0x00;
            bus.mem[0x40000 + i as usize] = i + 1; // all different from 0
        }
        bus.mem[0x40003] = 0x00; // match at index 3
        cpu.execute(0xA6, &mut bus, M);
        assert_eq!(cpu.cx, 1); // stopped after 4 iterations (5-4=1)
        assert!(fl::get(cpu.flags, Flag::ZF)); // match found
    }

    // =====================================================================
    // STOSB/STOSW (0xAA-0xAB)
    // =====================================================================

    #[test]
    fn stosb_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xEE);
        cpu.di = 0x0050;
        cpu.execute(0xAA, &mut bus, M); // STOSB
        assert_eq!(bus.mem[0x40050], 0xEE);
        assert_eq!(cpu.di, 0x0051);
    }

    #[test]
    fn stosw_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0xBEEF;
        cpu.di = 0x0050;
        cpu.execute(0xAB, &mut bus, M); // STOSW
        assert_eq!(bus.mem[0x40050], 0xEF); // low byte
        assert_eq!(bus.mem[0x40051], 0xBE); // high byte
        assert_eq!(cpu.di, 0x0052);
    }

    #[test]
    fn rep_stosb_fill() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xFF);
        cpu.di = 0x0000;
        cpu.cx = 8;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        cpu.execute(0xAA, &mut bus, M); // REP STOSB
        for i in 0..8 {
            assert_eq!(bus.mem[0x40000 + i], 0xFF);
        }
        assert_eq!(bus.mem[0x40008], 0x00); // untouched
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.di, 8);
    }

    #[test]
    fn rep_stosw_fill() {
        let (mut cpu, mut bus) = setup();
        cpu.ax = 0x1234;
        cpu.di = 0x0000;
        cpu.cx = 4;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        cpu.execute(0xAB, &mut bus, M); // REP STOSW
        for i in 0..4 {
            let base = 0x40000 + i * 2;
            assert_eq!(bus.mem[base], 0x34);
            assert_eq!(bus.mem[base + 1], 0x12);
        }
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.di, 8);
    }

    // =====================================================================
    // LODSB/LODSW (0xAC-0xAD)
    // =====================================================================

    #[test]
    fn lodsb_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0030;
        bus.mem[0x20030] = 0x99;
        cpu.execute(0xAC, &mut bus, M); // LODSB
        assert_eq!(cpu.al(), 0x99);
        assert_eq!(cpu.si, 0x0031);
    }

    #[test]
    fn lodsw_forward() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0030;
        bus.mem[0x20030] = 0xCD;
        bus.mem[0x20031] = 0xAB;
        cpu.execute(0xAD, &mut bus, M); // LODSW
        assert_eq!(cpu.ax, 0xABCD);
        assert_eq!(cpu.si, 0x0032);
    }

    #[test]
    fn lodsb_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.si = 0x0030;
        bus.mem[0x20030] = 0x77;
        cpu.execute(0xAC, &mut bus, M); // LODSB
        assert_eq!(cpu.al(), 0x77);
        assert_eq!(cpu.si, 0x002F);
    }

    #[test]
    fn rep_lodsb() {
        let (mut cpu, mut bus) = setup();
        cpu.si = 0x0000;
        cpu.cx = 3;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        bus.mem[0x20000] = 0x11;
        bus.mem[0x20001] = 0x22;
        bus.mem[0x20002] = 0x33;
        cpu.execute(0xAC, &mut bus, M); // REP LODSB
        assert_eq!(cpu.al(), 0x33); // last loaded value
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.si, 3);
    }

    // =====================================================================
    // SCASB/SCASW (0xAE-0xAF)
    // =====================================================================

    #[test]
    fn scasb_match() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x42);
        cpu.di = 0x0010;
        bus.mem[0x40010] = 0x42;
        cpu.execute(0xAE, &mut bus, M); // SCASB
        assert!(fl::get(cpu.flags, Flag::ZF)); // match
        assert_eq!(cpu.di, 0x0011);
    }

    #[test]
    fn scasb_no_match() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x42);
        cpu.di = 0x0010;
        bus.mem[0x40010] = 0x99;
        cpu.execute(0xAE, &mut bus, M); // SCASB
        assert!(!fl::get(cpu.flags, Flag::ZF)); // mismatch
    }

    #[test]
    fn scasw_match() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0x1234;
        cpu.di = 0x0010;
        bus.mem[0x40010] = 0x34;
        bus.mem[0x40011] = 0x12;
        cpu.execute(0xAF, &mut bus, M); // SCASW
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.di, 0x0012);
    }

    // =====================================================================
    // REPZ SCASB (scan for mismatch)
    // =====================================================================

    #[test]
    fn repz_scasb_all_match() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xAA);
        cpu.di = 0x0000;
        cpu.cx = 4;
        cpu.rep_prefix = Some(super::RepPrefix::Rep); // REPZ
        for i in 0..4 {
            bus.mem[0x40000 + i] = 0xAA;
        }
        cpu.execute(0xAE, &mut bus, M); // REPZ SCASB
        assert_eq!(cpu.cx, 0);
        assert!(fl::get(cpu.flags, Flag::ZF));
    }

    #[test]
    fn repz_scasb_mismatch() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0xAA);
        cpu.di = 0x0000;
        cpu.cx = 6;
        cpu.rep_prefix = Some(super::RepPrefix::Rep); // REPZ
        for i in 0..6 {
            bus.mem[0x40000 + i] = 0xAA;
        }
        bus.mem[0x40003] = 0xBB; // mismatch at index 3
        cpu.execute(0xAE, &mut bus, M);
        assert_eq!(cpu.cx, 2); // 6 - 4 iterations = 2
        assert!(!fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.di, 4);
    }

    // =====================================================================
    // REPNZ SCASB (scan for match, like memchr)
    // =====================================================================

    #[test]
    fn repnz_scasb_find() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x00); // searching for null terminator
        cpu.di = 0x0000;
        cpu.cx = 10;
        cpu.rep_prefix = Some(super::RepPrefix::Repnz);
        // "Hello\0..."
        let data = b"Hello\0xxxx";
        for (i, &b) in data.iter().enumerate() {
            bus.mem[0x40000 + i] = b;
        }
        cpu.execute(0xAE, &mut bus, M); // REPNZ SCASB
        assert!(fl::get(cpu.flags, Flag::ZF)); // found the null
        assert_eq!(cpu.cx, 4); // 10 - 6 iterations = 4
        assert_eq!(cpu.di, 6); // points past the null
    }

    #[test]
    fn repnz_scasb_not_found() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x00);
        cpu.di = 0x0000;
        cpu.cx = 3;
        cpu.rep_prefix = Some(super::RepPrefix::Repnz);
        bus.mem[0x40000] = 0x41;
        bus.mem[0x40001] = 0x42;
        bus.mem[0x40002] = 0x43;
        cpu.execute(0xAE, &mut bus, M);
        assert_eq!(cpu.cx, 0); // exhausted
        assert!(!fl::get(cpu.flags, Flag::ZF)); // not found
    }

    // =====================================================================
    // REPNZ SCASW (scan for 16-bit match)
    // =====================================================================

    #[test]
    fn repnz_scasw_find() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.ax = 0xDEAD;
        cpu.di = 0x0000;
        cpu.cx = 4;
        cpu.rep_prefix = Some(super::RepPrefix::Repnz);
        // Words: 0x0001, 0x0002, 0xDEAD, 0x0004
        let words: &[u16] = &[0x0001, 0x0002, 0xDEAD, 0x0004];
        for (i, &w) in words.iter().enumerate() {
            bus.mem[0x40000 + i * 2] = w as u8;
            bus.mem[0x40000 + i * 2 + 1] = (w >> 8) as u8;
        }
        cpu.execute(0xAF, &mut bus, M); // REPNZ SCASW
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.cx, 1); // found at 3rd word
        assert_eq!(cpu.di, 6); // past the match
    }

    // =====================================================================
    // REPZ CMPSW (block compare, 16-bit)
    // =====================================================================

    #[test]
    fn repz_cmpsw_all_equal() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.si = 0x0000;
        cpu.di = 0x0000;
        cpu.cx = 3;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        let words: &[u16] = &[0x1111, 0x2222, 0x3333];
        for (i, &w) in words.iter().enumerate() {
            bus.mem[0x20000 + i * 2] = w as u8;
            bus.mem[0x20000 + i * 2 + 1] = (w >> 8) as u8;
            bus.mem[0x40000 + i * 2] = w as u8;
            bus.mem[0x40000 + i * 2 + 1] = (w >> 8) as u8;
        }
        cpu.execute(0xA7, &mut bus, M); // REPZ CMPSW
        assert_eq!(cpu.cx, 0);
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.si, 6);
        assert_eq!(cpu.di, 6);
    }

    // =====================================================================
    // Segment override with string ops
    // =====================================================================

    #[test]
    fn movsb_with_segment_override() {
        let (mut cpu, mut bus) = setup();
        // Override source segment from DS to ES
        cpu.segment_override = Some(SegReg::ES);
        cpu.si = 0x0010;
        cpu.di = 0x0020;
        // Source at ES:SI = 0x4000:0x0010 = phys 0x40010
        bus.mem[0x40010] = 0xBB;
        cpu.execute(0xA4, &mut bus, M); // MOVSB
        // Dest at ES:DI = phys 0x40020
        assert_eq!(bus.mem[0x40020], 0xBB);
    }

    #[test]
    fn lodsb_with_segment_override() {
        let (mut cpu, mut bus) = setup();
        cpu.segment_override = Some(SegReg::SS);
        cpu.si = 0x0010;
        // Source at SS:SI = 0x3000:0x0010 = phys 0x30010
        bus.mem[0x30010] = 0x77;
        cpu.execute(0xAC, &mut bus, M); // LODSB
        assert_eq!(cpu.al(), 0x77);
    }

    // =====================================================================
    // CX=0 with REP (should not execute body)
    // =====================================================================

    #[test]
    fn rep_stosb_cx_zero() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xFF);
        cpu.di = 0x0000;
        cpu.cx = 0;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        cpu.execute(0xAA, &mut bus, M); // REP STOSB
        assert_eq!(bus.mem[0x40000], 0x00); // untouched
        assert_eq!(cpu.di, 0x0000); // unchanged
    }

    #[test]
    fn repz_scasb_cx_zero() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        cpu.set_al(0x42);
        cpu.di = 0x0010;
        cpu.cx = 0;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        // Should not compare anything
        fl::set(&mut cpu.flags, Flag::ZF, false);
        cpu.execute(0xAE, &mut bus, M);
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.di, 0x0010);
        // ZF should be unmodified (no comparison performed)
        assert!(!fl::get(cpu.flags, Flag::ZF));
    }

    // =====================================================================
    // STOSB backward with DF=1
    // =====================================================================

    #[test]
    fn rep_stosb_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.set_al(0xCC);
        cpu.di = 0x0003;
        cpu.cx = 4;
        cpu.rep_prefix = Some(super::RepPrefix::Rep);
        cpu.execute(0xAA, &mut bus, M); // REP STOSB backward
        for i in 0..4 {
            assert_eq!(bus.mem[0x40000 + i], 0xCC);
        }
        assert_eq!(cpu.cx, 0);
        assert_eq!(cpu.di, 0xFFFF); // wrapped: 0x0003 - 4
    }

    // =====================================================================
    // SCASB backward
    // =====================================================================

    #[test]
    fn scasb_backward() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};
        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.set_al(0x55);
        cpu.di = 0x0010;
        bus.mem[0x40010] = 0x55;
        cpu.execute(0xAE, &mut bus, M); // SCASB
        assert!(fl::get(cpu.flags, Flag::ZF));
        assert_eq!(cpu.di, 0x000F); // decremented
    }

    // =====================================================================
    // Flag control instructions
    // =====================================================================

    #[test]
    fn flag_control_clc_stc_cmc() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        fl::set(&mut cpu.flags, Flag::CF, true);
        cpu.execute(0xF8, &mut bus, M); // CLC
        assert!(!fl::get(cpu.flags, Flag::CF));

        cpu.execute(0xF9, &mut bus, M); // STC
        assert!(fl::get(cpu.flags, Flag::CF));

        cpu.execute(0xF5, &mut bus, M); // CMC
        assert!(!fl::get(cpu.flags, Flag::CF));
        cpu.execute(0xF5, &mut bus, M); // CMC again
        assert!(fl::get(cpu.flags, Flag::CF));
    }

    #[test]
    fn flag_control_cli_sti() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        fl::set(&mut cpu.flags, Flag::IF, true);
        cpu.execute(0xFA, &mut bus, M); // CLI
        assert!(!fl::get(cpu.flags, Flag::IF));

        cpu.execute(0xFB, &mut bus, M); // STI
        assert!(fl::get(cpu.flags, Flag::IF));
    }

    #[test]
    fn flag_control_cld_std() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        fl::set(&mut cpu.flags, Flag::DF, true);
        cpu.execute(0xFC, &mut bus, M); // CLD
        assert!(!fl::get(cpu.flags, Flag::DF));

        cpu.execute(0xFD, &mut bus, M); // STD
        assert!(fl::get(cpu.flags, Flag::DF));
    }

    // =====================================================================
    // PUSHF / POPF
    // =====================================================================

    #[test]
    fn pushf_popf_round_trip() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        // Set some flags
        fl::set(&mut cpu.flags, Flag::CF, true);
        fl::set(&mut cpu.flags, Flag::ZF, true);
        fl::set(&mut cpu.flags, Flag::SF, true);
        fl::set(&mut cpu.flags, Flag::IF, true);
        let saved_flags = cpu.flags;

        cpu.execute(0x9C, &mut bus, M); // PUSHF

        // Clear the flags
        fl::set(&mut cpu.flags, Flag::CF, false);
        fl::set(&mut cpu.flags, Flag::ZF, false);
        fl::set(&mut cpu.flags, Flag::SF, false);
        fl::set(&mut cpu.flags, Flag::IF, false);

        cpu.execute(0x9D, &mut bus, M); // POPF
        assert_eq!(cpu.flags, saved_flags);
    }

    // =====================================================================
    // INT / IRET
    // =====================================================================

    #[test]
    fn int_n_vectors_correctly() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        // Set up IVT for vector 0x21 at physical 0x0084 (0x21 * 4)
        bus.mem[0x0084] = 0x00; // IP low
        bus.mem[0x0085] = 0x10; // IP high = 0x1000
        bus.mem[0x0086] = 0x00; // CS low
        bus.mem[0x0087] = 0xF0; // CS high = 0xF000

        let old_cs = cpu.cs;
        fl::set(&mut cpu.flags, Flag::IF, true);
        fl::set(&mut cpu.flags, Flag::TF, true);
        let flags_before = cpu.flags;

        // INT 0x21
        bus.mem[0x100] = 0x21; // vector number
        cpu.execute(0xCD, &mut bus, M);

        // CS:IP should be loaded from IVT
        assert_eq!(cpu.ip, 0x1000);
        assert_eq!(cpu.cs, 0xF000);
        // IF and TF should be cleared
        assert!(!fl::get(cpu.flags, Flag::IF));
        assert!(!fl::get(cpu.flags, Flag::TF));

        // Stack should have: FLAGS, old CS, old IP (return addr = 0x0101)
        // SP was 0x200, now should be 0x200 - 6 = 0x1FA
        assert_eq!(cpu.sp, 0x01FA);
        // Check pushed values (SS:SP base = 0x30000)
        let pushed_ip = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp);
        let pushed_cs = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp.wrapping_add(2));
        let pushed_flags = cpu.read_word(&mut bus, M, cpu.ss, cpu.sp.wrapping_add(4));
        assert_eq!(pushed_ip, 0x0101); // IP after fetching imm8
        assert_eq!(pushed_cs, old_cs);
        assert_eq!(pushed_flags, flags_before);
    }

    #[test]
    fn int3_breakpoint() {
        let (mut cpu, mut bus) = setup();
        // Set up IVT for vector 3 at physical 0x000C
        bus.mem[0x000C] = 0x50;
        bus.mem[0x000D] = 0x00; // IP = 0x0050
        bus.mem[0x000E] = 0x00;
        bus.mem[0x000F] = 0x80; // CS = 0x8000

        cpu.execute(0xCC, &mut bus, M); // INT 3
        assert_eq!(cpu.ip, 0x0050);
        assert_eq!(cpu.cs, 0x8000);
    }

    #[test]
    fn iret_restores_state() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        // Simulate an interrupt: push FLAGS, CS, IP
        let orig_flags = cpu.flags | Flag::CF as u16 | Flag::IF as u16;
        cpu.push16(&mut bus, M, orig_flags); // FLAGS
        cpu.push16(&mut bus, M, 0xABCD); // CS
        cpu.push16(&mut bus, M, 0x1234); // IP

        cpu.execute(0xCF, &mut bus, M); // IRET
        assert_eq!(cpu.ip, 0x1234);
        assert_eq!(cpu.cs, 0xABCD);
        assert_eq!(cpu.flags, fl::normalize(orig_flags));
    }

    #[test]
    fn into_fires_when_of_set() {
        let (mut cpu, mut bus) = setup();
        use super::super::flags::{self as fl, Flag};

        // Set up IVT for vector 4 (overflow) at physical 0x0010
        bus.mem[0x0010] = 0x00;
        bus.mem[0x0011] = 0x20; // IP = 0x2000
        bus.mem[0x0012] = 0x00;
        bus.mem[0x0013] = 0x50; // CS = 0x5000

        // INTO with OF clear — should NOT fire
        fl::set(&mut cpu.flags, Flag::OF, false);
        cpu.execute(0xCE, &mut bus, M);
        assert_eq!(cpu.cs, 0x0000); // unchanged

        // INTO with OF set — should fire
        fl::set(&mut cpu.flags, Flag::OF, true);
        cpu.execute(0xCE, &mut bus, M);
        assert_eq!(cpu.ip, 0x2000);
        assert_eq!(cpu.cs, 0x5000);
    }

    // =====================================================================
    // IN / OUT
    // =====================================================================

    #[test]
    fn in_al_imm8() {
        let (mut cpu, mut bus) = setup();
        // io_read defaults to memory read, so set up a value at addr 0x42
        bus.mem[0x42] = 0xAB;
        bus.mem[0x100] = 0x42; // port number
        cpu.execute(0xE4, &mut bus, M); // IN AL, 0x42
        assert_eq!(cpu.al(), 0xAB);
    }

    #[test]
    fn out_imm8_al() {
        let (mut cpu, mut bus) = setup();
        cpu.set_al(0xCD);
        bus.mem[0x100] = 0x80; // port number
        cpu.execute(0xE6, &mut bus, M); // OUT 0x80, AL
        // io_write defaults to memory write
        assert_eq!(bus.mem[0x80], 0xCD);
    }

    #[test]
    fn in_al_dx() {
        let (mut cpu, mut bus) = setup();
        cpu.dx = 0x0060;
        bus.mem[0x60] = 0x77;
        cpu.execute(0xEC, &mut bus, M); // IN AL, DX
        assert_eq!(cpu.al(), 0x77);
    }

    #[test]
    fn out_dx_al() {
        let (mut cpu, mut bus) = setup();
        cpu.dx = 0x0061;
        cpu.set_al(0xEE);
        cpu.execute(0xEE, &mut bus, M); // OUT DX, AL
        assert_eq!(bus.mem[0x61], 0xEE);
    }

    // =====================================================================
    // DIV overflow → INT 0
    // =====================================================================

    #[test]
    fn div_by_zero_triggers_int0() {
        let (mut cpu, mut bus) = setup();
        // Set up IVT for vector 0 at physical 0x0000
        bus.mem[0x0000] = 0x00;
        bus.mem[0x0001] = 0x30; // IP = 0x3000
        bus.mem[0x0002] = 0x00;
        bus.mem[0x0003] = 0x70; // CS = 0x7000

        cpu.ax = 0x1234;
        // modrm = 0x36: mod=0, reg=6 (DIV), rm=6 (direct address)
        bus.mem[0x100] = 0x36;
        bus.mem[0x101] = 0x00; // address low
        bus.mem[0x102] = 0x50; // address high = 0x5000
        // Memory at DS:0x5000 = 0 (default) -> divide by zero
        cpu.execute(0xF6, &mut bus, M);

        // Should have vectored to INT 0 handler
        assert_eq!(cpu.ip, 0x3000);
        assert_eq!(cpu.cs, 0x7000);
        // AX should be unchanged (instruction didn't complete)
        assert_eq!(cpu.ax, 0x1234);
    }

    // =====================================================================
    // HLT
    // =====================================================================

    #[test]
    fn hlt_enters_halted_state() {
        let (mut cpu, mut bus) = setup();
        cpu.execute(0xF4, &mut bus, M); // HLT
        assert!(matches!(cpu.state, super::ExecState::Halted));
    }
}
