use crate::core::{Bus, BusMaster};
use crate::cpu::flags::parity;
use crate::cpu::z80::{ExecState, Flag, Z80};

impl Z80 {
    /// Perform CB rotate/shift operation on a value.
    /// op: 0=RLC, 1=RRC, 2=RL, 3=RR, 4=SLA, 5=SRA, 6=SLL(undoc), 7=SRL.
    /// Returns (result, new_flags). Flags: S, Z, PV(parity), C from shifted bit. H=0, N=0.
    fn do_cb_rotate_shift(&self, op: u8, val: u8) -> (u8, u8) {
        let (result, carry) = match op {
            0 => {
                // RLC: rotate left circular
                let c = (val >> 7) & 1;
                ((val << 1) | c, c)
            }
            1 => {
                // RRC: rotate right circular
                let c = val & 1;
                ((val >> 1) | (c << 7), c)
            }
            2 => {
                // RL: rotate left through carry
                let old_c = if (self.f & Flag::C as u8) != 0 { 1 } else { 0 };
                let c = (val >> 7) & 1;
                ((val << 1) | old_c, c)
            }
            3 => {
                // RR: rotate right through carry
                let old_c = if (self.f & Flag::C as u8) != 0 {
                    0x80
                } else {
                    0
                };
                let c = val & 1;
                ((val >> 1) | old_c, c)
            }
            4 => {
                // SLA: shift left arithmetic
                let c = (val >> 7) & 1;
                (val << 1, c)
            }
            5 => {
                // SRA: shift right arithmetic (preserves sign)
                let c = val & 1;
                (((val as i8) >> 1) as u8, c)
            }
            6 => {
                // SLL: shift left logical, set bit 0 (undocumented)
                let c = (val >> 7) & 1;
                ((val << 1) | 1, c)
            }
            7 => {
                // SRL: shift right logical
                let c = val & 1;
                (val >> 1, c)
            }
            _ => unreachable!(),
        };

        let mut f = 0;
        if result == 0 {
            f |= Flag::Z as u8;
        }
        if (result & 0x80) != 0 {
            f |= Flag::S as u8;
        }
        if parity(result) {
            f |= Flag::PV as u8;
        }
        if carry != 0 {
            f |= Flag::C as u8;
        }
        // H = 0, N = 0
        f |= result & (Flag::X as u8 | Flag::Y as u8);

        (result, f)
    }

    /// Execute CB-prefixed instruction.
    /// Called from ExecuteCB state with the CB sub-opcode and cycle counter.
    /// Rotate/shift: S, Z, PV(parity), C from shifted bit, H=0, N=0.
    /// BIT: Z = ~bit, S = bit 7 if tested, PV = Z, H=1, N=0, C preserved.
    /// SET/RES: No flags affected.
    /// CB register ops: 1 handler cycle (8T total).
    /// BIT b,(HL): 5 handler cycles (12T total).
    /// Rotate/shift/SET/RES (HL): 8 handler cycles (15T total).
    pub fn execute_instruction_cb<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        op: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let xx = (op >> 6) & 0x03; // 0=rot/shift, 1=BIT, 2=RES, 3=SET
        let yyy = (op >> 3) & 0x07; // bit number or shift operation
        let zzz = op & 0x07; // register index

        if zzz == 6 {
            // (HL) operations — multi-cycle
            match xx {
                1 => self.op_cb_bit_hl(op, yyy, cycle, bus, master),
                _ => self.op_cb_rmw_hl(op, xx, yyy, cycle, bus, master),
            }
        } else {
            // Register operations — 1 handler cycle (8T total)
            let val = self.get_reg8(zzz);
            match xx {
                0 => {
                    // Rotate/shift r
                    let (result, f) = self.do_cb_rotate_shift(yyy, val);
                    self.f = f;
                    self.q = self.f;
                    self.set_reg8(zzz, result);
                }
                1 => {
                    // BIT b,r — test bit, no writeback
                    let tested = val & (1 << yyy);
                    let mut f = self.f & Flag::C as u8; // preserve C
                    f |= Flag::H as u8;
                    if tested == 0 {
                        f |= Flag::Z as u8;
                        f |= Flag::PV as u8; // PV = Z for BIT
                    }
                    if yyy == 7 && tested != 0 {
                        f |= Flag::S as u8;
                    }
                    // X/Y from the operand register value
                    f |= val & (Flag::X as u8 | Flag::Y as u8);
                    self.f = f;
                    self.q = self.f;
                }
                2 => {
                    // RES b,r — no flag changes
                    self.set_reg8(zzz, val & !(1 << yyy));
                }
                3 => {
                    // SET b,r — no flag changes
                    self.set_reg8(zzz, val | (1 << yyy));
                }
                _ => unreachable!(),
            }
            self.state = ExecState::Fetch;
        }
    }

    /// BIT b,(HL) — 12T: Main M1(4) + CB M1(4) + MR_ext(4)
    /// Z = ~bit, S = bit 7 if tested, PV = Z, H=1, N=0, C preserved. X/Y from MEMPTR high.
    /// 5 handler cycles: 0=pad, 1=read, 2=pad, 3=internal, 4=done
    fn op_cb_bit_hl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        op: u8,
        bit: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 | 2 | 3 => self.state = ExecState::ExecuteCB(op, cycle + 1),
            1 => {
                let addr = self.get_hl();
                let val = bus.read(master, addr);
                let tested = val & (1 << bit);
                let mut f = self.f & Flag::C as u8; // preserve C
                f |= Flag::H as u8;
                if tested == 0 {
                    f |= Flag::Z as u8;
                    f |= Flag::PV as u8;
                }
                if bit == 7 && tested != 0 {
                    f |= Flag::S as u8;
                }
                // X/Y from high byte of MEMPTR for BIT (HL)
                f |= ((self.memptr >> 8) as u8) & (Flag::X as u8 | Flag::Y as u8);
                self.f = f;
                self.q = self.f;
                self.state = ExecState::ExecuteCB(op, 2);
            }
            4 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// Rotate/shift/SET/RES (HL) — 15T: Main M1(4) + CB M1(4) + MR_ext(4) + MW(3)
    /// Rotate/shift flags: see `do_cb_rotate_shift`. SET/RES: no flags affected.
    /// 8 handler cycles: 0=pad, 1=read, 2=pad, 3=compute, 4=write, 5-6=pad, 7=done
    fn op_cb_rmw_hl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        op: u8,
        xx: u8,
        yyy: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 | 2 | 5 | 6 => self.state = ExecState::ExecuteCB(op, cycle + 1),
            1 => {
                self.temp_addr = self.get_hl();
                self.temp_data = bus.read(master, self.temp_addr);
                self.state = ExecState::ExecuteCB(op, 2);
            }
            3 => {
                // Internal cycle: compute result
                self.temp_data = match xx {
                    0 => {
                        let (r, f) = self.do_cb_rotate_shift(yyy, self.temp_data);
                        self.f = f;
                        self.q = self.f;
                        r
                    }
                    2 => self.temp_data & !(1 << yyy), // RES — no flag changes
                    3 => self.temp_data | (1 << yyy),  // SET — no flag changes
                    _ => unreachable!(),
                };
                self.state = ExecState::ExecuteCB(op, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.temp_data);
                self.state = ExecState::ExecuteCB(op, 5);
            }
            7 => self.state = ExecState::Fetch,
            _ => unreachable!(),
        }
    }

    /// Execute DD CB d op / FD CB d op (indexed bit operations).
    /// Address is pre-computed in temp_addr, displacement in temp_data.
    /// Flags: see `execute_instruction_cb`. BIT X/Y from address high byte.
    /// BIT b,(IX+d): 4 handler cycles (20T total).
    /// Other (IX+d): 7 handler cycles (23T total).
    /// For non-BIT ops with zzz != 6, result is also copied to register zzz (undocumented).
    pub fn execute_instruction_index_cb<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        op: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let xx = (op >> 6) & 0x03; // 0=rot/shift, 1=BIT, 2=RES, 3=SET
        let yyy = (op >> 3) & 0x07; // bit number or shift operation
        let zzz = op & 0x07; // register (6 = no copy, otherwise undocumented copy)

        // MEMPTR = IX+d / IY+d (the computed address)
        if cycle == 0 {
            self.memptr = self.temp_addr;
        }

        if xx == 1 {
            // BIT b,(IX+d) — 20T: 4 handler cycles
            // 0=pad, 1=read (IX+d), 2=pad, 3=done
            match cycle {
                0 | 2 => self.state = ExecState::ExecuteIndexCB(op, cycle + 1),
                1 => {
                    let val = bus.read(master, self.temp_addr);
                    let tested = val & (1 << yyy);
                    let mut f = self.f & Flag::C as u8;
                    f |= Flag::H as u8;
                    if tested == 0 {
                        f |= Flag::Z as u8;
                        f |= Flag::PV as u8;
                    }
                    if yyy == 7 && tested != 0 {
                        f |= Flag::S as u8;
                    }
                    // X/Y from high byte of address for indexed BIT
                    f |= ((self.temp_addr >> 8) as u8) & (Flag::X as u8 | Flag::Y as u8);
                    self.f = f;
                    self.q = self.f;
                    self.state = ExecState::ExecuteIndexCB(op, 2);
                }
                3 => self.state = ExecState::Fetch,
                _ => unreachable!(),
            }
        } else {
            // Rotate/shift/SET/RES (IX+d) — 23T: 7 handler cycles
            // 0=pad, 1=read, 2=pad, 3=compute, 4=write, 5=pad, 6=done
            match cycle {
                0 | 2 | 5 => self.state = ExecState::ExecuteIndexCB(op, cycle + 1),
                1 => {
                    self.temp_data = bus.read(master, self.temp_addr);
                    self.state = ExecState::ExecuteIndexCB(op, 2);
                }
                3 => {
                    self.temp_data = match xx {
                        0 => {
                            let (r, f) = self.do_cb_rotate_shift(yyy, self.temp_data);
                            self.f = f;
                            self.q = self.f;
                            r
                        }
                        2 => self.temp_data & !(1 << yyy),
                        3 => self.temp_data | (1 << yyy),
                        _ => unreachable!(),
                    };
                    // Undocumented: if zzz != 6, copy result to register
                    if zzz != 6 {
                        self.set_reg8(zzz, self.temp_data);
                    }
                    self.state = ExecState::ExecuteIndexCB(op, 4);
                }
                4 => {
                    bus.write(master, self.temp_addr, self.temp_data);
                    self.state = ExecState::ExecuteIndexCB(op, 5);
                }
                6 => self.state = ExecState::Fetch,
                _ => unreachable!(),
            }
        }
    }
}
