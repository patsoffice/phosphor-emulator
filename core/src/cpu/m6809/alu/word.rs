use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;
use crate::cpu::m6809::{ExecState, M6809};

impl M6809 {
    // --- Internal 16-bit ALU Helpers ---

    #[inline]
    fn perform_addd(&mut self, operand: u16) {
        let d = self.get_d();
        let (result, carry) = d.overflowing_add(operand);
        let overflow = (d ^ operand) & 0x8000 == 0 && (d ^ result) & 0x8000 != 0;
        self.set_d(result);
        self.set_flags_arithmetic16(result, overflow, carry);
    }

    #[inline]
    fn perform_subd(&mut self, operand: u16) {
        let d = self.get_d();
        let (result, borrow) = d.overflowing_sub(operand);
        let overflow = (d ^ operand) & 0x8000 != 0 && (d ^ result) & 0x8000 != 0;
        self.set_d(result);
        self.set_flags_arithmetic16(result, overflow, borrow);
    }

    #[inline]
    fn perform_cmp16(&mut self, reg_val: u16, operand: u16) {
        let (result, borrow) = reg_val.overflowing_sub(operand);
        let overflow = (reg_val ^ operand) & 0x8000 != 0 && (reg_val ^ result) & 0x8000 != 0;
        self.set_flags_arithmetic16(result, overflow, borrow);
    }

    /// ADDD immediate (0xC3): Adds a 16-bit immediate value to the D register.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned carry out of bit 15.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_addd_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_addd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// SUBD immediate (0x83): Subtracts a 16-bit immediate value from the D register.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_subd_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_subd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPX immediate (0x8C): Compare X with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmpx_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the compare runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.x, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPD immediate (0x1083): Compare D with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmpd_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the compare runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.get_d(), operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPY immediate (0x108C): Compare Y with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmpy_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the compare runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.y, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDD immediate (0xCC): Load D with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldd_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let val = self.temp_addr | low;
                self.set_d(val);
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDX immediate (0x8E): Load X with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldx_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let val = self.temp_addr | low;
                self.x = val;
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDU immediate (0xCE): Load U with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldu_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let val = self.temp_addr | low;
                self.u = val;
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDY immediate (0x108E): Load Y with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldy_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let val = self.temp_addr | low;
                self.y = val;
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDS immediate (0x10CE): Load S with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_lds_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let val = self.temp_addr | low;
                self.s = val;
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Direct addressing mode (16-bit) ---

    /// SUBD direct (0x93): Subtracts the 16-bit value at DP:addr from the D register.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_subd_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::Execute(opcode, 4);
                // Store operand for next cycle
                self.temp_addr = operand;
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_subd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// ADDD direct (0xD3): Adds the 16-bit value at DP:addr to the D register.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned carry out of bit 15.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_addd_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_addd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPD direct (0x1093): Compare D with 16-bit value at DP:addr.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as SUBD direct).
    pub(crate) fn op_cmpd_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.get_d(), operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPY direct (0x109C): Compare Y with 16-bit value at DP:addr.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as CMPX direct).
    pub(crate) fn op_cmpy_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.y, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPX direct (0x9C): Compare X with 16-bit value at DP:addr.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_cmpx_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.x, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Extended addressing mode (16-bit) ---

    /// SUBD extended (0xB3): Subtracts the 16-bit value at the extended address from D.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 7 total cycles: 1 fetch + 6 exec.
    pub(crate) fn op_subd_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_subd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPD extended (0x10B3): Compare D with 16-bit value at extended address.
    /// 8 total cycles: 2 prefix + 6 exec (same exec pattern as SUBD extended).
    pub(crate) fn op_cmpd_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage2(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.get_d(), operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPY extended (0x10BC): Compare Y with 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 8 total cycles: 2 prefix + 6 exec (same exec pattern as CMPX extended).
    pub(crate) fn op_cmpy_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage2(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.y, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDY extended (0x10BE): Load Y from 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as LDX extended).
    pub(crate) fn op_ldy_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.y = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.y |= low;
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STY extended (0x10BF): Store Y to 16-bit extended address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as STX extended).
    pub(crate) fn op_sty_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, (self.y >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.y as u8);
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDS extended (0x10FE): Load S from 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as LDX extended).
    pub(crate) fn op_lds_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.s = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.s |= low;
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STS extended (0x10FF): Store S to 16-bit extended address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as STX extended).
    pub(crate) fn op_sts_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage2(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, (self.s >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.s as u8);
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// ADDD extended (0xF3): Adds the 16-bit value at the extended address to D.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned carry out of bit 15.
    /// 7 total cycles: 1 fetch + 6 exec.
    pub(crate) fn op_addd_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_addd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPX extended (0xBC): Compare X with 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 7 total cycles: 1 fetch + 6 exec.
    pub(crate) fn op_cmpx_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.x, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Indexed addressing mode (16-bit ALU) ---

    /// SUBD indexed (0xA3): Subtract 16-bit value at indexed EA from D.
    pub(crate) fn op_subd_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::Execute(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_subd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// ADDD indexed (0xE3): Add 16-bit value at indexed EA to D.
    pub(crate) fn op_addd_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::Execute(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_addd(operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// CMPX indexed (0xAC): Compare X with 16-bit value at indexed EA.
    pub(crate) fn op_cmpx_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::Execute(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.x, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    // --- Page 2 indexed (16-bit) ---

    /// CMPD indexed (0x10A3): Compare D with 16-bit value at indexed EA.
    pub(crate) fn op_cmpd_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::ExecutePage2(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.get_d(), operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 40);
                }
            }
        }
    }

    /// CMPY indexed (0x10AC): Compare Y with 16-bit value at indexed EA.
    pub(crate) fn op_cmpy_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::ExecutePage2(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.y, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 40);
                }
            }
        }
    }

    // --- Page 3 (0x11 prefix): CMPU, CMPS ---

    /// CMPU immediate (0x1183): Compare U with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmpu_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the compare runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.u, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPU direct (0x1193): Compare U with 16-bit value at DP:addr.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as CMPX direct).
    pub(crate) fn op_cmpu_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage3(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage3(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.u, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPU extended (0x11B3): Compare U with 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 8 total cycles: 2 prefix + 6 exec (same exec pattern as CMPX extended).
    pub(crate) fn op_cmpu_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage3(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage3(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.u, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPU indexed (0x11A3): Compare U with 16-bit value at indexed EA.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmpu_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::ExecutePage3(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::ExecutePage3(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.u, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page3(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage3(opcode, 40);
                }
            }
        }
    }

    /// CMPS immediate (0x118C): Compare S with 16-bit immediate.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmps_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                // Trailing don't-care cycle at PC; the compare runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.s, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPS direct (0x119C): Compare S with 16-bit value at DP:addr.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 7 total cycles: 2 prefix + 5 exec (same exec pattern as CMPX direct).
    pub(crate) fn op_cmps_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage3(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage3(opcode, 4);
            }
            4 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.s, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPS extended (0x11BC): Compare S with 16-bit value at extended address.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    /// 8 total cycles: 2 prefix + 6 exec (same exec pattern as CMPX extended).
    pub(crate) fn op_cmps_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::ExecutePage3(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::ExecutePage3(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(opcode, 3);
            }
            3 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high as u8;
                self.state = ExecState::ExecutePage3(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let operand = ((self.scratch as u16) << 8) | low;
                self.temp_addr = operand;
                self.state = ExecState::ExecutePage3(opcode, 5);
            }
            5 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.s, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// CMPS indexed (0x11AC): Compare S with 16-bit value at indexed EA.
    /// N set if result bit 15 is set. Z set if result is zero.
    /// V set if signed overflow occurred. C set if unsigned borrow occurred.
    pub(crate) fn op_cmps_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // Read high byte of the 16-bit operand
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = ExecState::ExecutePage3(opcode, 50);
            }
            50 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = ((self.scratch as u16) << 8) | low;
                self.state = ExecState::ExecutePage3(opcode, 51);
            }
            51 => {
                // Trailing don't-care cycle at PC; the ALU op runs on it
                self.dummy_at_pc(bus, master, 0);
                let operand = self.temp_addr;
                self.perform_cmp16(self.s, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page3(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage3(opcode, 40);
                }
            }
        }
    }
}
