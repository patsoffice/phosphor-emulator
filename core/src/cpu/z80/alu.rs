use crate::core::{Bus, BusMaster};
use crate::cpu::z80::{ExecState, Flag, IndexMode, Z80};

impl Z80 {
    // --- Flag Helpers ---

    pub(super) fn get_parity(val: u8) -> bool {
        val.count_ones().is_multiple_of(2)
    }

    /// S and Z for an 8-bit result.
    #[inline]
    pub(super) fn sz_flags(result: u8) -> u8 {
        let mut f = 0;
        if result == 0 {
            f |= Flag::Z as u8;
        }
        if (result & 0x80) != 0 {
            f |= Flag::S as u8;
        }
        f
    }

    /// S and Z for a 16-bit result (`ADC HL,rr` / `SBC HL,rr`).
    #[inline]
    pub(super) fn sz_flags16(result: u16) -> u8 {
        let mut f = 0;
        if result == 0 {
            f |= Flag::Z as u8;
        }
        if (result & 0x8000) != 0 {
            f |= Flag::S as u8;
        }
        f
    }

    /// The undocumented X/Y bits, copied from `source`.
    ///
    /// `source` is a parameter rather than "the result" because it genuinely
    /// varies: CP takes X/Y from the *operand*, the 16-bit adds from the high
    /// byte of the result, and SCF/CCF from A or A|F depending on the previous
    /// instruction's Q. Every caller states its source for that reason.
    #[inline]
    pub(super) fn xy_flags(source: u8) -> u8 {
        source & (Flag::X as u8 | Flag::Y as u8)
    }

    /// Commit a built flag byte, keeping Q in step.
    ///
    /// Q latches "this instruction wrote the flags", which is what the SCF/CCF
    /// X/Y quirk reads via `prev_q`; writing `self.f` without updating it would
    /// silently break those two opcodes.
    #[inline]
    pub(super) fn commit_flags(&mut self, f: u8) {
        self.f = f;
        self.q = self.f;
    }

    fn update_flags_logic(&mut self, result: u8, is_and: bool) {
        let mut f = Self::sz_flags(result);
        if Self::get_parity(result) {
            f |= Flag::PV as u8;
        }
        if is_and {
            f |= Flag::H as u8;
        } // AND sets H, others clear it
        // N is 0, C is 0

        f |= Self::xy_flags(result);
        self.commit_flags(f);
    }

    fn do_add(&mut self, val: u8, carry_in: bool) {
        let a = self.a;
        let c_val = if carry_in && (self.f & Flag::C as u8) != 0 {
            1
        } else {
            0
        };
        let result_u16 = (a as u16) + (val as u16) + (c_val as u16);
        let result = result_u16 as u8;

        let mut f = Self::sz_flags(result);
        if ((a & 0xF) + (val & 0xF) + (c_val as u8)) > 0xF {
            f |= Flag::H as u8;
        }
        if ((a ^ result) & (val ^ result) & 0x80) != 0 {
            f |= Flag::PV as u8;
        }
        if result_u16 > 0xFF {
            f |= Flag::C as u8;
        }

        f |= Self::xy_flags(result);
        self.a = result;
        self.commit_flags(f);
    }

    fn do_sub(&mut self, val: u8, carry_in: bool) {
        let a = self.a;
        let c_val = if carry_in && (self.f & Flag::C as u8) != 0 {
            1
        } else {
            0
        };
        let result_u16 = (a as u16)
            .wrapping_sub(val as u16)
            .wrapping_sub(c_val as u16);
        let result = result_u16 as u8;

        let mut f = Flag::N as u8 | Self::sz_flags(result);
        if (a & 0xF) < ((val & 0xF) + (c_val as u8)) {
            f |= Flag::H as u8;
        }
        if ((a ^ val) & (a ^ result) & 0x80) != 0 {
            f |= Flag::PV as u8;
        }
        if result_u16 > 0xFF {
            f |= Flag::C as u8;
        }

        f |= Self::xy_flags(result);
        self.a = result;
        self.commit_flags(f);
    }

    fn do_cp(&mut self, val: u8) {
        let a = self.a;
        let result_u16 = (a as u16).wrapping_sub(val as u16);
        let result = result_u16 as u8;

        let mut f = Flag::N as u8 | Self::sz_flags(result);
        if (a & 0xF) < (val & 0xF) {
            f |= Flag::H as u8;
        }
        if ((a ^ val) & (a ^ result) & 0x80) != 0 {
            f |= Flag::PV as u8;
        }
        if result_u16 > 0xFF {
            f |= Flag::C as u8;
        }

        // X/Y come from the operand for CP, not the result
        f |= Self::xy_flags(val);
        self.commit_flags(f);
    }

    fn perform_alu_op(&mut self, op: u8, val: u8) {
        match op {
            0 => self.do_add(val, false), // ADD
            1 => self.do_add(val, true),  // ADC
            2 => self.do_sub(val, false), // SUB
            3 => self.do_sub(val, true),  // SBC
            4 => {
                self.a &= val;
                self.update_flags_logic(self.a, true);
            } // AND
            5 => {
                self.a ^= val;
                self.update_flags_logic(self.a, false);
            } // XOR
            6 => {
                self.a |= val;
                self.update_flags_logic(self.a, false);
            } // OR
            7 => self.do_cp(val),         // CP
            _ => unreachable!(),
        }
    }

    // --- Addressing Mode Helpers ---

    /// Generic 19T indexed read helper for (IX+d)/(IY+d) operations.
    /// 12 handler cycles: 1=pad, 2=read displacement, 3-9=pad/internal,
    /// 10=read from (IX/IY+d) + apply, 11=pad, 12=done.
    /// Used by ALU A,(IX+d) and LD r,(IX+d).
    #[inline]
    pub(super) fn indexed_read<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        match cycle {
            1 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 11 => {
                self.state = ExecState::Execute(opcode, cycle + 1);
            }
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            10 => {
                let addr = self.get_index_addr();
                self.memptr = addr;
                let val = bus.read(master, addr);
                operation(self, val);
                self.state = ExecState::Execute(opcode, 11);
            }
            12 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    // --- Instructions ---

    /// ALU A, r — 4 T (reg) or 7 T ((HL)) or 19 T ((IX+d))
    /// ADD/ADC: S, Z, H, PV(overflow), N=0, C affected.
    /// SUB/SBC/CP: S, Z, H, PV(overflow), N=1, C affected. CP: X/Y from operand.
    /// AND: S, Z, PV(parity), H=1, N=0, C=0. XOR/OR: S, Z, PV(parity), H=0, N=0, C=0.
    /// Opcode mask: 10 xxx zzz
    pub fn op_alu_r<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let alu_op = (opcode >> 3) & 0x07;
        let r = opcode & 0x07;

        if r == 6 {
            if self.index_mode == IndexMode::HL {
                // ALU A, (HL) — 7 T: cycles 1-4
                match cycle {
                    1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
                    2 => {
                        let addr = self.get_hl();
                        let val = bus.read(master, addr);
                        self.perform_alu_op(alu_op, val);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    4 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            } else {
                // ALU A, (IX+d) — 19 T: cycles 1-12
                self.indexed_read(opcode, cycle, bus, master, |cpu, val| {
                    cpu.perform_alu_op(alu_op, val);
                });
            }
        } else {
            // ALU A, r — 4 T: M1 only
            let val = self.get_reg8_ix(r);
            self.perform_alu_op(alu_op, val);
            self.state = ExecState::Fetch;
        }
    }

    /// ALU A, n — 7 T: M1(4) + MR(3). Flags: see `op_alu_r`.
    /// Opcode mask: 11 xxx 110
    pub fn op_alu_n<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let alu_op = (opcode >> 3) & 0x07;

        // cycles 1=T4, 2=MR read imm, 3=MR pad, 4=done
        match cycle {
            1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                let val = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.perform_alu_op(alu_op, val);
                self.state = ExecState::Execute(opcode, 3);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// INC/DEC r — 4 T (reg) or 11 T ((HL)) or 23 T ((IX+d))
    /// INC: S, Z, H, PV(overflow: 7F→80), N=0, C preserved.
    /// DEC: S, Z, H, PV(overflow: 80→7F), N=1, C preserved.
    /// Opcode mask: 00 rrr 10x
    pub fn op_inc_dec_r<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let r = (opcode >> 3) & 0x07;
        let is_dec = (opcode & 0x01) != 0;

        if r == 6 {
            if self.index_mode == IndexMode::HL {
                // INC/DEC (HL) — 11 T: M1(4) + MR(3) + internal(1) + MW(3)
                // cycles 1=T4, 2=MR read, 3-4=pad, 5=compute, 6=MW write, 7=pad, 8=done
                match cycle {
                    1 | 3 | 4 | 7 => self.state = ExecState::Execute(opcode, cycle + 1),
                    2 => {
                        let addr = self.get_hl();
                        self.temp_data = bus.read(master, addr);
                        self.temp_addr = addr;
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    5 => {
                        self.temp_data = if is_dec {
                            self.calc_dec_flags(self.temp_data)
                        } else {
                            self.calc_inc_flags(self.temp_data)
                        };
                        self.state = ExecState::Execute(opcode, 6);
                    }
                    6 => {
                        bus.write(master, self.temp_addr, self.temp_data);
                        self.state = ExecState::Execute(opcode, 7);
                    }
                    8 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            } else {
                // INC/DEC (IX+d) — 23 T: DD M1(4) + M1(4) + MR(3) + internal(5) + MR(3) + internal(1) + MW(3)
                // cycles 1-16: 1=pad, 2=read d, 3=pad, 4-8=internal, 9=pad, 10=read (IX+d),
                //               11=pad, 12=compute, 13=pad, 14=write (IX+d), 15=pad, 16=done
                match cycle {
                    1 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 11 | 13 | 15 => {
                        self.state = ExecState::Execute(opcode, cycle + 1);
                    }
                    2 => {
                        self.temp_data = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    10 => {
                        let addr = self.get_index_addr();
                        self.temp_addr = addr;
                        self.temp_data = bus.read(master, addr);
                        self.memptr = addr;
                        self.state = ExecState::Execute(opcode, 11);
                    }
                    12 => {
                        self.temp_data = if is_dec {
                            self.calc_dec_flags(self.temp_data)
                        } else {
                            self.calc_inc_flags(self.temp_data)
                        };
                        self.state = ExecState::Execute(opcode, 13);
                    }
                    14 => {
                        bus.write(master, self.temp_addr, self.temp_data);
                        self.state = ExecState::Execute(opcode, 15);
                    }
                    16 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            }
        } else {
            // INC/DEC r — 4 T: M1 only
            let val = self.get_reg8_ix(r);
            let result = if is_dec {
                self.calc_dec_flags(val)
            } else {
                self.calc_inc_flags(val)
            };
            self.set_reg8_ix(r, result);
            self.state = ExecState::Fetch;
        }
    }

    fn calc_inc_flags(&mut self, val: u8) -> u8 {
        let result = val.wrapping_add(1);
        let mut f = (self.f & Flag::C as u8) | Self::sz_flags(result); // Preserve C
        if (val & 0xF) == 0xF {
            f |= Flag::H as u8;
        }
        if val == 0x7F {
            f |= Flag::PV as u8;
        } // Overflow 7F -> 80
        // N is 0
        f |= Self::xy_flags(result);
        self.commit_flags(f);
        result
    }

    fn calc_dec_flags(&mut self, val: u8) -> u8 {
        let result = val.wrapping_sub(1);
        // Preserve C, Set N
        let mut f = (self.f & Flag::C as u8) | Flag::N as u8 | Self::sz_flags(result);
        if (val & 0xF) == 0x0 {
            f |= Flag::H as u8;
        } // Borrow from bit 4
        if val == 0x80 {
            f |= Flag::PV as u8;
        } // Overflow 80 -> 7F
        f |= Self::xy_flags(result);
        self.commit_flags(f);
        result
    }

    // --- 16-bit ALU ---

    /// ADD HL,rr — 11 T: M1(4) + internal(7)
    /// Opcode mask: 00 rr1 001 (rr: 0=BC, 1=DE, 2=HL/IX/IY, 3=SP)
    /// Flags: H = carry from bit 11, C = carry from bit 15, N = 0.
    /// S, Z, PV preserved. X/Y from high byte of result.
    pub fn op_add_hl_rr(&mut self, opcode: u8, cycle: u8) {
        let rp = (opcode >> 4) & 0x03;
        // cycles 1-7 = internal, 8 = done
        match cycle {
            1 => {
                let hl = self.get_rp(2);
                let rr = self.get_rp(rp);
                let result = (hl as u32) + (rr as u32);
                self.memptr = hl.wrapping_add(1);

                let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
                if ((hl & 0x0FFF) + (rr & 0x0FFF)) > 0x0FFF {
                    f |= Flag::H as u8;
                }
                if result > 0xFFFF {
                    f |= Flag::C as u8;
                }
                f |= Self::xy_flags((result >> 8) as u8);
                self.commit_flags(f);
                self.set_rp(2, result as u16);

                self.state = ExecState::Execute(opcode, 2);
            }
            2..=7 => self.state = ExecState::Execute(opcode, cycle + 1),
            8 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// INC rr / DEC rr — 6 T: M1(4) + internal(2)
    /// INC: 00 rr0 011, DEC: 00 rr1 011. No flags affected.
    pub fn op_inc_dec_rr(&mut self, opcode: u8, cycle: u8) {
        let rp = (opcode >> 4) & 0x03;
        let is_dec = (opcode & 0x08) != 0;
        // cycles 1-2 = internal, 3 = done
        match cycle {
            1 => {
                let val = self.get_rp(rp);
                let result = if is_dec {
                    val.wrapping_sub(1)
                } else {
                    val.wrapping_add(1)
                };
                self.set_rp(rp, result);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => self.state = ExecState::Execute(opcode, 3),
            3 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    // --- Accumulator Rotates ---

    /// RLCA — 4 T: M1 only.
    /// Rotate A left circular. Old bit 7 to carry and bit 0.
    /// H = 0, N = 0, C = old bit 7. X/Y from A. S, Z, PV preserved.
    pub fn op_rlca(&mut self) {
        let bit7 = (self.a >> 7) & 1;
        self.a = (self.a << 1) | bit7;
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        if bit7 != 0 {
            f |= Flag::C as u8;
        }
        f |= self.a & (Flag::X as u8 | Flag::Y as u8);
        self.f = f;
        self.q = self.f;
        self.state = ExecState::Fetch;
    }

    /// RRCA — 4 T: M1 only.
    /// Rotate A right circular. Old bit 0 to carry and bit 7.
    /// H = 0, N = 0, C = old bit 0. X/Y from A. S, Z, PV preserved.
    pub fn op_rrca(&mut self) {
        let bit0 = self.a & 1;
        self.a = (self.a >> 1) | (bit0 << 7);
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        if bit0 != 0 {
            f |= Flag::C as u8;
        }
        f |= self.a & (Flag::X as u8 | Flag::Y as u8);
        self.f = f;
        self.q = self.f;
        self.state = ExecState::Fetch;
    }

    /// RLA — 4 T: M1 only.
    /// Rotate A left through carry. Old bit 7 to C, old C to bit 0.
    /// H = 0, N = 0, C = old bit 7. X/Y from A. S, Z, PV preserved.
    pub fn op_rla(&mut self) {
        let old_carry = if (self.f & Flag::C as u8) != 0 {
            1u8
        } else {
            0
        };
        let bit7 = (self.a >> 7) & 1;
        self.a = (self.a << 1) | old_carry;
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        if bit7 != 0 {
            f |= Flag::C as u8;
        }
        f |= self.a & (Flag::X as u8 | Flag::Y as u8);
        self.f = f;
        self.q = self.f;
        self.state = ExecState::Fetch;
    }

    /// RRA — 4 T: M1 only.
    /// Rotate A right through carry. Old bit 0 to C, old C to bit 7.
    /// H = 0, N = 0, C = old bit 0. X/Y from A. S, Z, PV preserved.
    pub fn op_rra(&mut self) {
        let old_carry = if (self.f & Flag::C as u8) != 0 {
            0x80u8
        } else {
            0
        };
        let bit0 = self.a & 1;
        self.a = (self.a >> 1) | old_carry;
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        if bit0 != 0 {
            f |= Flag::C as u8;
        }
        f |= self.a & (Flag::X as u8 | Flag::Y as u8);
        self.f = f;
        self.q = self.f;
        self.state = ExecState::Fetch;
    }

    // --- Misc ALU ---

    /// DAA — 4 T: M1 only.
    /// Decimal adjust accumulator after BCD add/sub.
    /// S, Z, H, PV(parity), C affected. N preserved.
    pub fn op_daa(&mut self) {
        let a = self.a;
        let n = (self.f & Flag::N as u8) != 0;
        let old_h = (self.f & Flag::H as u8) != 0;
        let old_c = (self.f & Flag::C as u8) != 0;

        let mut correction = 0u8;
        let mut new_c = old_c;

        if old_h || (a & 0x0F) > 9 {
            correction |= 0x06;
        }
        if old_c || a > 0x99 {
            correction |= 0x60;
            new_c = true;
        }

        let result = if n {
            a.wrapping_sub(correction)
        } else {
            a.wrapping_add(correction)
        };

        let new_h = if n {
            old_h && (a & 0x0F) < 6
        } else {
            (a & 0x0F) > 9
        };

        self.a = result;
        let mut f = Self::sz_flags(result);
        if new_c {
            f |= Flag::C as u8;
        }
        if n {
            f |= Flag::N as u8;
        }
        if new_h {
            f |= Flag::H as u8;
        }
        if Self::get_parity(result) {
            f |= Flag::PV as u8;
        }
        f |= Self::xy_flags(result);
        self.commit_flags(f);
        self.state = ExecState::Fetch;
    }

    /// CPL — 4 T: M1 only.
    /// Complement A. Sets H and N. X/Y from A. S, Z, PV, C preserved.
    pub fn op_cpl(&mut self) {
        self.a = !self.a;
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8 | Flag::C as u8);
        f |= Flag::H as u8 | Flag::N as u8;
        f |= Self::xy_flags(self.a);
        self.commit_flags(f);
        self.state = ExecState::Fetch;
    }

    /// SCF — 4 T: M1 only.
    /// Set carry flag. C = 1, H = 0, N = 0. S, Z, PV preserved.
    /// Undocumented: X/Y from A if q=1, from (A | F) if q=0.
    pub fn op_scf(&mut self) {
        let xy_source = if self.prev_q != 0 {
            self.a
        } else {
            self.a | self.f
        };
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        f |= Flag::C as u8;
        f |= Self::xy_flags(xy_source);
        self.commit_flags(f);
        self.state = ExecState::Fetch;
    }

    /// CCF — 4 T: M1 only.
    /// Complement carry flag. H = old C, C = ~C, N = 0. S, Z, PV preserved.
    /// Undocumented: X/Y from A if q=1, from (A | F) if q=0.
    pub fn op_ccf(&mut self) {
        let xy_source = if self.prev_q != 0 {
            self.a
        } else {
            self.a | self.f
        };
        let old_c = self.f & Flag::C as u8;
        let mut f = self.f & (Flag::S as u8 | Flag::Z as u8 | Flag::PV as u8);
        if old_c != 0 {
            f |= Flag::H as u8;
        }
        if old_c == 0 {
            f |= Flag::C as u8;
        }
        f |= Self::xy_flags(xy_source);
        self.commit_flags(f);
        self.state = ExecState::Fetch;
    }

    // --- ED ALU Operations ---

    /// NEG — 8T (ED prefix): A = 0 - A.
    /// Flags: S, Z, H (borrow from bit 3), PV (A was 0x80), N=1, C (A was not 0).
    pub fn op_neg(&mut self) {
        let a = self.a;
        let result = 0u8.wrapping_sub(a);
        let mut f = Flag::N as u8 | Self::sz_flags(result);
        if (a & 0x0F) != 0 {
            f |= Flag::H as u8;
        }
        if a == 0x80 {
            f |= Flag::PV as u8;
        }
        if a != 0 {
            f |= Flag::C as u8;
        }
        f |= Self::xy_flags(result);
        self.a = result;
        self.commit_flags(f);
        self.state = ExecState::Fetch;
    }

    /// ADC HL,rr — 15T (ED prefix): HL = HL + rr + C.
    /// 8 handler cycles: 0=compute, 1-6=internal, 7=done.
    /// Flags: S, Z, H (carry from bit 11), PV (overflow), N=0, C. X/Y from high byte.
    pub fn op_adc_hl_rr(&mut self, opcode: u8, cycle: u8) {
        let rp = (opcode >> 4) & 0x03;
        match cycle {
            0 => {
                let hl = self.get_hl();
                let rr = self.get_rp(rp);
                let c_val = if (self.f & Flag::C as u8) != 0 {
                    1u32
                } else {
                    0
                };
                let result = (hl as u32) + (rr as u32) + c_val;
                let result16 = result as u16;
                self.memptr = hl.wrapping_add(1);

                let mut f = Self::sz_flags16(result16);
                if ((hl & 0x0FFF) + (rr & 0x0FFF) + (c_val as u16)) > 0x0FFF {
                    f |= Flag::H as u8;
                }
                // Overflow via signed arithmetic
                let signed = (hl as i16 as i32) + (rr as i16 as i32) + (c_val as i32);
                if !(-0x8000..=0x7FFF).contains(&signed) {
                    f |= Flag::PV as u8;
                }
                if result > 0xFFFF {
                    f |= Flag::C as u8;
                }
                f |= Self::xy_flags((result16 >> 8) as u8);
                self.commit_flags(f);
                self.set_hl(result16);

                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1..=6 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            7 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// SBC HL,rr — 15T (ED prefix): HL = HL - rr - C.
    /// 8 handler cycles: 0=compute, 1-6=internal, 7=done.
    /// Flags: S, Z, H, PV (overflow), N=1, C. X/Y from high byte.
    pub fn op_sbc_hl_rr(&mut self, opcode: u8, cycle: u8) {
        let rp = (opcode >> 4) & 0x03;
        match cycle {
            0 => {
                let hl = self.get_hl();
                let rr = self.get_rp(rp);
                let c_val = if (self.f & Flag::C as u8) != 0 {
                    1u32
                } else {
                    0
                };
                let result = (hl as u32).wrapping_sub(rr as u32).wrapping_sub(c_val);
                let result16 = result as u16;
                self.memptr = hl.wrapping_add(1);

                let mut f = Flag::N as u8 | Self::sz_flags16(result16);
                if (hl & 0x0FFF) < ((rr & 0x0FFF) + (c_val as u16)) {
                    f |= Flag::H as u8;
                }
                let signed = (hl as i16 as i32) - (rr as i16 as i32) - (c_val as i32);
                if !(-0x8000..=0x7FFF).contains(&signed) {
                    f |= Flag::PV as u8;
                }
                if result > 0xFFFF {
                    f |= Flag::C as u8;
                }
                f |= Self::xy_flags((result16 >> 8) as u8);
                self.commit_flags(f);
                self.set_hl(result16);

                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1..=6 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            7 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// RRD — 18T (ED prefix): rotate right nibbles between A and (HL).
    /// (HL)_low → A_low, A_low → (HL)_high, (HL)_high → (HL)_low.
    /// Flags from A: S, Z, PV(parity), H=0, N=0, C preserved.
    /// 11 handler cycles: 0=pad, 1=read(HL), 2=pad, 3-6=internal, 7=write(HL), 8-9=pad, 10=done.
    pub fn op_rrd<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 | 2 | 4 | 5 | 6 | 8 | 9 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            1 => {
                let addr = self.get_hl();
                self.temp_data = bus.read(master, addr);
                self.temp_addr = addr;
                self.state = ExecState::ExecuteED(opcode, 2);
            }
            3 => {
                // Compute: (HL) = (A_low << 4) | (tmp >> 4), A = (A & 0xF0) | (tmp & 0x0F)
                let tmp = self.temp_data;
                let new_mem = ((self.a & 0x0F) << 4) | (tmp >> 4);
                self.a = (self.a & 0xF0) | (tmp & 0x0F);
                self.temp_data = new_mem;
                self.memptr = self.temp_addr.wrapping_add(1);

                // Flags from A: S, Z, PV(parity), H=0, N=0, C preserved, X/Y from A
                let mut f = (self.f & Flag::C as u8) | Self::sz_flags(self.a);
                if Self::get_parity(self.a) {
                    f |= Flag::PV as u8;
                }
                f |= Self::xy_flags(self.a);
                self.commit_flags(f);
                self.state = ExecState::ExecuteED(opcode, 4);
            }
            7 => {
                bus.write(master, self.temp_addr, self.temp_data);
                self.state = ExecState::ExecuteED(opcode, 8);
            }
            10 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// RLD — 18T (ED prefix): rotate left nibbles between A and (HL).
    /// (HL)_high → A_low, A_low → (HL)_low, (HL)_low → (HL)_high.
    /// Flags from A: S, Z, PV(parity), H=0, N=0, C preserved.
    /// Same cycle structure as RRD.
    pub fn op_rld<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 | 2 | 4 | 5 | 6 | 8 | 9 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            1 => {
                let addr = self.get_hl();
                self.temp_data = bus.read(master, addr);
                self.temp_addr = addr;
                self.state = ExecState::ExecuteED(opcode, 2);
            }
            3 => {
                let tmp = self.temp_data;
                let new_mem = ((tmp & 0x0F) << 4) | (self.a & 0x0F);
                self.a = (self.a & 0xF0) | (tmp >> 4);
                self.temp_data = new_mem;
                self.memptr = self.temp_addr.wrapping_add(1);

                let mut f = (self.f & Flag::C as u8) | Self::sz_flags(self.a);
                if Self::get_parity(self.a) {
                    f |= Flag::PV as u8;
                }
                f |= Self::xy_flags(self.a);
                self.commit_flags(f);
                self.state = ExecState::ExecuteED(opcode, 4);
            }
            7 => {
                bus.write(master, self.temp_addr, self.temp_data);
                self.state = ExecState::ExecuteED(opcode, 8);
            }
            10 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }
}
