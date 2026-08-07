use crate::core::{Bus, BusMaster};
use crate::cpu::flags::parity;
use crate::cpu::z80::{ExecState, Flag, IndexMode, Z80};

impl Z80 {
    /// LD r, n — 7 T: M1(4) + MR(3). No flags affected.
    /// LD (HL), n — 10 T: M1(4) + MR(3) + MW(3)
    /// LD (IX+d), n — 19 T: DD M1(4) + M1(4) + MR(3) + MR(3) + internal(2) + MW(3)
    /// Opcode mask: 00 rrr 110
    pub fn op_ld_r_n<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let r = (opcode >> 3) & 0x07;

        if r == 6 {
            if self.index_mode == IndexMode::HL {
                // LD (HL), n — 10 T: M1(4) + MR(3) + MW(3)
                match cycle {
                    1 | 3 | 4 | 6 => self.state = ExecState::Execute(opcode, cycle + 1),
                    2 => {
                        self.temp_data = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    5 => {
                        let addr = self.get_hl();
                        bus.write(master, addr, self.temp_data);
                        self.state = ExecState::Execute(opcode, 6);
                    }
                    7 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            } else {
                // LD (IX+d), n — 19 T: cycles 1-12
                // 1=pad, 2=read d, 3=pad, 4=pad, 5=read n, 6=pad, 7-8=internal,
                // 9=pad, 10=write (IX+d), 11=pad, 12=done
                match cycle {
                    1 | 3 | 4 | 6 | 7 | 8 | 9 | 11 => {
                        self.state = ExecState::Execute(opcode, cycle + 1);
                    }
                    2 => {
                        // Read displacement
                        self.temp_addr = bus.read(master, self.pc) as u16;
                        self.pc = self.pc.wrapping_add(1);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    5 => {
                        // Read immediate value
                        self.temp_data = bus.read(master, self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.state = ExecState::Execute(opcode, 6);
                    }
                    10 => {
                        // Write to (IX/IY+d): compute address from stored displacement
                        let base = match self.index_mode {
                            IndexMode::IX => self.ix,
                            IndexMode::IY => self.iy,
                            _ => unreachable!(),
                        };
                        let addr = base.wrapping_add(self.temp_addr as i8 as i16 as u16);
                        bus.write(master, addr, self.temp_data);
                        self.memptr = addr;
                        self.state = ExecState::Execute(opcode, 11);
                    }
                    12 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            }
        } else {
            // LD r, n — 7 T: M1(4) + MR(3)
            match cycle {
                1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
                2 => {
                    let n = bus.read(master, self.pc);
                    self.pc = self.pc.wrapping_add(1);
                    self.set_reg8_ix(r, n);
                    self.state = ExecState::Execute(opcode, 3);
                }
                4 => self.state = ExecState::Fetch,
                _ => unreachable!(),
            }
        }
    }

    /// LD r, r' — 4 T: M1 only (register-register). No flags affected.
    /// LD r, (HL) — 7 T: M1(4) + MR(3)
    /// LD r, (IX+d) — 19 T: DD M1(4) + M1(4) + MR(3) + internal(5) + MR(3)
    /// LD (HL), r — 7 T: M1(4) + MW(3)
    /// LD (IX+d), r — 19 T: DD M1(4) + M1(4) + MR(3) + internal(5) + MW(3)
    /// Opcode mask: 01 dst src
    pub fn op_ld_r_r<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let src = opcode & 0x07;
        let dst = (opcode >> 3) & 0x07;

        if src == 6 {
            if self.index_mode == IndexMode::HL {
                // LD r, (HL) — 7 T: cycles 1-4
                match cycle {
                    1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
                    2 => {
                        let addr = self.get_hl();
                        let val = bus.read(master, addr);
                        self.set_reg8(dst, val);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    4 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            } else {
                // LD r, (IX+d) — 19 T: cycles 1-12
                self.indexed_read(opcode, cycle, bus, master, |cpu, val| {
                    cpu.set_reg8(dst, val);
                });
            }
        } else if dst == 6 {
            if self.index_mode == IndexMode::HL {
                // LD (HL), r — 7 T: cycles 1-4
                match cycle {
                    1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
                    2 => {
                        let val = self.get_reg8(src);
                        let addr = self.get_hl();
                        bus.write(master, addr, val);
                        self.state = ExecState::Execute(opcode, 3);
                    }
                    4 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            } else {
                // LD (IX+d), r — 19 T: cycles 1-12
                // 1=pad, 2=read d, 3=pad, 4-8=internal, 9=pad, 10=write (IX+d), 11=pad, 12=done
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
                        let val = self.get_reg8(src);
                        bus.write(master, addr, val);
                        self.memptr = addr;
                        self.state = ExecState::Execute(opcode, 11);
                    }
                    12 => self.state = ExecState::Fetch,
                    _ => unreachable!(),
                }
            }
        } else {
            // LD r, r' — 4 T: M1 only
            let val = self.get_reg8_ix(src);
            self.set_reg8_ix(dst, val);
            self.state = ExecState::Fetch;
        }
    }

    /// Generic 10T 16-bit immediate read helper: M1(4) + MR(3) + MR(3).
    /// Reads a 16-bit value from PC (low byte first) and passes it to the closure.
    /// Used by LD rr,nn / JP nn / JP cc,nn.
    #[inline]
    pub(super) fn read16_imm<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u16),
    {
        // cycles 1=T4, 2=MR1 read low, 3-4=MR1 pad, 5=MR2 read high, 6-7=MR2 pad
        match cycle {
            1 | 3 | 4 | 6 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                let val = ((high as u16) << 8) | self.temp_data as u16;
                operation(self, val);
                self.state = ExecState::Execute(opcode, 6);
            }
            7 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD rr, nn — 10 T: M1(4) + MR(3) + MR(3). No flags affected.
    /// Opcode mask: 00 rr0 001 (rr: 0=BC, 1=DE, 2=HL/IX/IY, 3=SP)
    pub fn op_ld_rr_nn<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rp = (opcode >> 4) & 0x03;
        self.read16_imm(opcode, cycle, bus, master, |cpu, val| {
            cpu.set_rp(rp, val);
        });
    }

    /// LD A, (BC) — 7 T: M1(4) + MR(3). No flags affected.
    pub fn op_ld_a_bc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                let addr = self.get_bc();
                self.a = bus.read(master, addr);
                self.memptr = addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD A, (DE) — 7 T: M1(4) + MR(3). No flags affected.
    pub fn op_ld_a_de<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                let addr = self.get_de();
                self.a = bus.read(master, addr);
                self.memptr = addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD (BC), A — 7 T: M1(4) + MW(3). No flags affected.
    pub fn op_ld_bc_a<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                let addr = self.get_bc();
                bus.write(master, addr, self.a);
                self.memptr = ((self.a as u16) << 8) | ((addr.wrapping_add(1)) & 0xFF);
                self.state = ExecState::Execute(opcode, 3);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD (DE), A — 7 T: M1(4) + MW(3). No flags affected.
    pub fn op_ld_de_a<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            1 | 3 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                let addr = self.get_de();
                bus.write(master, addr, self.a);
                self.memptr = ((self.a as u16) << 8) | ((addr.wrapping_add(1)) & 0xFF);
                self.state = ExecState::Execute(opcode, 3);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD A, (nn) — 13 T: M1(4) + MR(3) + MR(3) + MR(3). No flags affected.
    pub fn op_ld_a_nn<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // 1=T4, 2=MR1 read addr low, 3-4=pad, 5=MR2 read addr high, 6-7=pad,
        // 8=MR3 read data, 9-10=pad
        match cycle {
            1 | 3 | 4 | 6 | 7 | 9 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::Execute(opcode, 6);
            }
            8 => {
                self.a = bus.read(master, self.temp_addr);
                self.memptr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 9);
            }
            10 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD (nn), A — 13 T: M1(4) + MR(3) + MR(3) + MW(3). No flags affected.
    pub fn op_ld_nn_a<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            1 | 3 | 4 | 6 | 7 | 9 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::Execute(opcode, 6);
            }
            8 => {
                bus.write(master, self.temp_addr, self.a);
                self.memptr = ((self.a as u16) << 8) | ((self.temp_addr.wrapping_add(1)) & 0xFF);
                self.state = ExecState::Execute(opcode, 9);
            }
            10 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD SP, HL — 6 T: M1(4) + 2 internal. No flags affected.
    pub fn op_ld_sp_hl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        _bus: &mut B,
        _master: BusMaster,
    ) {
        // cycles 1=T4, 2=internal, 3=done
        match cycle {
            1 | 2 => self.state = ExecState::Execute(opcode, cycle + 1),
            3 => {
                self.sp = self.get_rp(2); // HL/IX/IY depending on prefix
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }

    /// LD (nn), HL — 16 T: M1(4) + MR(3) + MR(3) + MW(3) + MW(3). No flags affected.
    pub fn op_ld_nn_hl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // 1=T4, 2=MR1 addr low, 3-4=pad, 5=MR2 addr high, 6-7=pad,
        // 8=MW1 write low, 9-10=pad, 11=MW2 write high, 12-13=pad
        match cycle {
            1 | 3 | 4 | 6 | 7 | 9 | 10 | 12 => {
                self.state = ExecState::Execute(opcode, cycle + 1);
            }
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::Execute(opcode, 6);
            }
            8 => {
                let val = self.get_rp(2);
                bus.write(master, self.temp_addr, val as u8);
                self.state = ExecState::Execute(opcode, 9);
            }
            11 => {
                let val = self.get_rp(2);
                bus.write(master, self.temp_addr.wrapping_add(1), (val >> 8) as u8);
                self.memptr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 12);
            }
            13 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD HL, (nn) — 16 T: M1(4) + MR(3) + MR(3) + MR(3) + MR(3). No flags affected.
    pub fn op_ld_hl_nn_ind<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // 1=T4, 2=MR1 addr low, 3-4=pad, 5=MR2 addr high, 6-7=pad,
        // 8=MR3 data low, 9-10=pad, 11=MR4 data high, 12-13=pad
        match cycle {
            1 | 3 | 4 | 6 | 7 | 9 | 10 | 12 => {
                self.state = ExecState::Execute(opcode, cycle + 1);
            }
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::Execute(opcode, 6);
            }
            8 => {
                self.temp_data = bus.read(master, self.temp_addr);
                self.state = ExecState::Execute(opcode, 9);
            }
            11 => {
                let high = bus.read(master, self.temp_addr.wrapping_add(1));
                let val = ((high as u16) << 8) | self.temp_data as u16;
                self.set_rp(2, val);
                self.memptr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 12);
            }
            13 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// EX AF, AF' — 4 T: M1 only. No flags affected (swaps AF with AF').
    pub fn op_ex_af_af(&mut self) {
        std::mem::swap(&mut self.a, &mut self.a_prime);
        std::mem::swap(&mut self.f, &mut self.f_prime);
        self.state = ExecState::Fetch;
    }

    /// EXX — 4 T: M1 only. No flags affected.
    pub fn op_exx(&mut self) {
        std::mem::swap(&mut self.b, &mut self.b_prime);
        std::mem::swap(&mut self.c, &mut self.c_prime);
        std::mem::swap(&mut self.d, &mut self.d_prime);
        std::mem::swap(&mut self.e, &mut self.e_prime);
        std::mem::swap(&mut self.h, &mut self.h_prime);
        std::mem::swap(&mut self.l, &mut self.l_prime);
        self.state = ExecState::Fetch;
    }

    /// EX DE, HL — 4 T: M1 only (NOT affected by DD/FD prefix). No flags affected.
    pub fn op_ex_de_hl(&mut self) {
        std::mem::swap(&mut self.d, &mut self.h);
        std::mem::swap(&mut self.e, &mut self.l);
        self.state = ExecState::Fetch;
    }

    // --- ED Load/Store Operations ---

    /// LD I,A — 9T (ED prefix): 2 handler cycles. No flags affected.
    pub fn op_ld_i_a(&mut self, opcode: u8, cycle: u8) {
        match cycle {
            0 => {
                self.i = self.a;
                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD R,A — 9T (ED prefix): 2 handler cycles. No flags affected.
    pub fn op_ld_r_a(&mut self, opcode: u8, cycle: u8) {
        match cycle {
            0 => {
                self.r = self.a;
                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD A,I — 9T (ED prefix): 2 handler cycles.
    /// Flags: S, Z from I, H=0, N=0, PV=IFF2, C preserved, X/Y from I.
    pub fn op_ld_a_i(&mut self, opcode: u8, cycle: u8) {
        match cycle {
            0 => {
                self.a = self.i;
                let mut f = self.f & Flag::C as u8;
                if self.a == 0 {
                    f |= Flag::Z as u8;
                }
                if (self.a & 0x80) != 0 {
                    f |= Flag::S as u8;
                }
                if self.iff2 {
                    f |= Flag::PV as u8;
                }
                f |= self.a & (Flag::X as u8 | Flag::Y as u8);
                self.f = f;
                self.q = self.f;
                self.p = true;
                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD A,R — 9T (ED prefix): 2 handler cycles.
    /// Flags: S, Z from R, H=0, N=0, PV=IFF2, C preserved, X/Y from R.
    pub fn op_ld_a_r(&mut self, opcode: u8, cycle: u8) {
        match cycle {
            0 => {
                self.a = self.r;
                let mut f = self.f & Flag::C as u8;
                if self.a == 0 {
                    f |= Flag::Z as u8;
                }
                if (self.a & 0x80) != 0 {
                    f |= Flag::S as u8;
                }
                if self.iff2 {
                    f |= Flag::PV as u8;
                }
                f |= self.a & (Flag::X as u8 | Flag::Y as u8);
                self.f = f;
                self.q = self.f;
                self.p = true;
                self.state = ExecState::ExecuteED(opcode, 1);
            }
            1 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD (nn),rr — 20T (ED prefix): M1(4)+M1(4)+MR(3)+MR(3)+MW(3)+MW(3). No flags affected.
    /// 13 handler cycles: 0=pad, 1=read addr_lo, 2=pad, 3=pad, 4=read addr_hi,
    /// 5=pad, 6=write data_lo, 7-8=pad, 9=write data_hi, 10-11=pad, 12=done.
    pub fn op_ld_nn_rr_ed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rp = (opcode >> 4) & 0x03;
        match cycle {
            0 | 2 | 3 | 5 | 7 | 8 | 10 | 11 => {
                self.state = ExecState::ExecuteED(opcode, cycle + 1);
            }
            1 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::ExecuteED(opcode, 2);
            }
            4 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::ExecuteED(opcode, 5);
            }
            6 => {
                let val = self.get_rp(rp);
                bus.write(master, self.temp_addr, val as u8);
                self.state = ExecState::ExecuteED(opcode, 7);
            }
            9 => {
                let val = self.get_rp(rp);
                bus.write(master, self.temp_addr.wrapping_add(1), (val >> 8) as u8);
                self.memptr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecuteED(opcode, 10);
            }
            12 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// LD rr,(nn) — 20T (ED prefix): M1(4)+M1(4)+MR(3)+MR(3)+MR(3)+MR(3). No flags affected.
    /// 13 handler cycles (same structure as LD (nn),rr but reads instead of writes).
    pub fn op_ld_rr_nn_ed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let rp = (opcode >> 4) & 0x03;
        match cycle {
            0 | 2 | 3 | 5 | 7 | 8 | 10 | 11 => {
                self.state = ExecState::ExecuteED(opcode, cycle + 1);
            }
            1 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::ExecuteED(opcode, 2);
            }
            4 => {
                let high = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::ExecuteED(opcode, 5);
            }
            6 => {
                self.temp_data = bus.read(master, self.temp_addr);
                self.state = ExecState::ExecuteED(opcode, 7);
            }
            9 => {
                let high = bus.read(master, self.temp_addr.wrapping_add(1));
                let val = ((high as u16) << 8) | self.temp_data as u16;
                self.set_rp(rp, val);
                self.memptr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecuteED(opcode, 10);
            }
            12 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// IN r,(C) — 12T (ED prefix): reads port BC via bus.io_read().
    /// 5 handler cycles: 0-3=IO cycle, 4=done.
    /// Flags: S, Z, PV(parity) from input, H=0, N=0, C preserved. X/Y from input.
    /// For r=6 (IN F,(C)): flags affected but value not stored.
    pub fn op_in_r_c<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0..=3 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            4 => {
                let port = self.get_bc();
                let val = bus.io_read(master, port);
                let r = (opcode >> 3) & 0x07;
                if r != 6 {
                    self.set_reg8(r, val);
                }
                // Set flags from input value
                let mut f = self.f & Flag::C as u8;
                if val == 0 {
                    f |= Flag::Z as u8;
                }
                if (val & 0x80) != 0 {
                    f |= Flag::S as u8;
                }
                if parity(val) {
                    f |= Flag::PV as u8;
                }
                f |= val & (Flag::X as u8 | Flag::Y as u8);
                self.f = f;
                self.q = self.f;
                self.memptr = port.wrapping_add(1);
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }

    /// OUT (C),r — 12T (ED prefix): writes to port BC via bus.io_write().
    /// 5 handler cycles: 0-3=IO cycle, 4=done. No flag changes.
    /// For r=6: outputs 0 (undocumented).
    pub fn op_out_c_r<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0..=3 => self.state = ExecState::ExecuteED(opcode, cycle + 1),
            4 => {
                let port = self.get_bc();
                let r = (opcode >> 3) & 0x07;
                let val = if r == 6 { 0 } else { self.get_reg8(r) };
                bus.io_write(master, port, val);
                self.memptr = port.wrapping_add(1);
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }

    /// IN A,(n) — 11T: M1(4) + MR(3) + IO(4). No flags affected.
    /// Reads port address n from immediate byte via bus.io_read().
    /// MEMPTR = ((A << 8) | n) + 1
    pub fn op_in_a_n<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // Handler cycles 1-8 (8 total after overhead cycle 0):
        // 1=pad, 2=read n, 3-4=pad, 5-7=IO pad, 8=IO complete
        match cycle {
            1 | 3 | 4 | 5 | 6 | 7 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            8 => {
                let n = self.temp_data;
                let port_addr = ((self.a as u16) << 8) | n as u16;
                self.a = bus.io_read(master, port_addr);
                self.memptr = port_addr.wrapping_add(1);
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }

    /// OUT (n),A — 11T: M1(4) + MR(3) + IO(4). No flags affected.
    /// Reads port address n from immediate byte, writes A via bus.io_write().
    /// MEMPTR low = (n+1) & 0xFF, MEMPTR high = A
    pub fn op_out_n_a<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // Same timing as IN A,(n): handler cycles 1-8
        match cycle {
            1 | 3 | 4 | 5 | 6 | 7 => self.state = ExecState::Execute(opcode, cycle + 1),
            2 => {
                self.temp_data = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            8 => {
                let n = self.temp_data;
                let port_addr = ((self.a as u16) << 8) | n as u16;
                bus.io_write(master, port_addr, self.a);
                self.memptr = ((self.a as u16) << 8) | ((n.wrapping_add(1)) as u16);
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }

    /// EX (SP), HL — 19 T: M1(4) + MR(3) + MR(4) + MW(3) + MW(5). No flags affected.
    pub fn op_ex_sp_hl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        // 1=T4, 2=MR1 read low from (SP), 3-4=pad,
        // 5=MR2 read high from (SP+1), 6-7=pad, 8=extra internal,
        // 9=MW1 write high to (SP+1), 10-11=pad,
        // 12=MW2 write low to (SP), 13-14=pad, 15-16=extra internal
        match cycle {
            1 | 3 | 4 | 6 | 7 | 8 | 10 | 11 | 13 | 14 | 15 => {
                self.state = ExecState::Execute(opcode, cycle + 1);
            }
            2 => {
                // Read low byte from (SP)
                self.temp_data = bus.read(master, self.sp);
                self.state = ExecState::Execute(opcode, 3);
            }
            5 => {
                // Read high byte from (SP+1)
                let high = bus.read(master, self.sp.wrapping_add(1));
                self.temp_addr = ((high as u16) << 8) | self.temp_data as u16;
                self.state = ExecState::Execute(opcode, 6);
            }
            9 => {
                // Write HL high to (SP+1)
                let hl = self.get_rp(2);
                bus.write(master, self.sp.wrapping_add(1), (hl >> 8) as u8);
                self.state = ExecState::Execute(opcode, 10);
            }
            12 => {
                // Write HL low to (SP)
                let hl = self.get_rp(2);
                bus.write(master, self.sp, hl as u8);
                self.state = ExecState::Execute(opcode, 13);
            }
            16 => {
                // Set HL to the value read from stack
                self.set_rp(2, self.temp_addr);
                self.memptr = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => unreachable!(),
        }
    }
}
