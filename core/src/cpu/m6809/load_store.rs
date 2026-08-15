use super::{CcFlag, ExecState, M6809};
use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;

impl M6809 {
    /// LDA immediate (0x86)
    pub(crate) fn op_lda_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        if cycle == 0 {
            self.a = bus.read(master, self.pc);
            self.pc = self.pc.wrapping_add(1);
            self.set_flag(CcFlag::N, self.a & 0x80 != 0);
            self.set_flag(CcFlag::Z, self.a == 0);
            self.set_flag(CcFlag::V, false);
            self.state = ExecState::Fetch;
        }
    }

    /// LDB immediate (0xC6)
    pub(crate) fn op_ldb_imm<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        if cycle == 0 {
            self.b = bus.read(master, self.pc);
            self.pc = self.pc.wrapping_add(1);
            self.set_flag(CcFlag::N, self.b & 0x80 != 0);
            self.set_flag(CcFlag::Z, self.b == 0);
            self.set_flag(CcFlag::V, false);
            self.state = ExecState::Fetch;
        }
    }

    /// LDA direct (0x96): Load A from memory at DP:addr.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_lda_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.a = bus.read(master, self.temp_addr);
                self.set_flags_logical(self.a);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STA direct (0x97): Store A to memory at DP:addr.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_sta_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.a);
                self.set_flag(CcFlag::N, self.a & 0x80 != 0);
                self.set_flag(CcFlag::Z, self.a == 0);
                self.set_flag(CcFlag::V, false);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDB direct (0xD6): Load B from memory at DP:addr.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_ldb_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.b = bus.read(master, self.temp_addr);
                self.set_flags_logical(self.b);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STB direct (0xD7): Store B to memory at DP:addr.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 4 total cycles: 1 fetch + 3 exec.
    pub(crate) fn op_stb_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.b);
                self.set_flag(CcFlag::N, self.b & 0x80 != 0);
                self.set_flag(CcFlag::Z, self.b == 0);
                self.set_flag(CcFlag::V, false);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDD direct (0xDC): Load D (A:B) from memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_ldd_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.a = high as u8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                self.b = bus.read(master, self.temp_addr);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STD direct (0xDD): Store D (A:B) to memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_std_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.a);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, self.b);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDX direct (0x9E): Load X from memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_ldx_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.x = high << 8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.x |= low;
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STX direct (0x9F): Store X to memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_stx_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.x >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, self.x as u8);
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDU direct (0xDE): Load U from memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_ldu_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.u = high << 8;
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.u |= low;
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDY direct (0x109E): Load Y from memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 2 prefix + 4 exec (same exec pattern as LDX direct).
    pub(crate) fn op_ldy_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.y = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.y |= low;
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STY direct (0x109F): Store Y to memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 2 prefix + 4 exec (same exec pattern as STX direct).
    pub(crate) fn op_sty_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.y >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, self.y as u8);
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDS direct (0x10DE): Load S from memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 2 prefix + 4 exec (same exec pattern as LDX direct).
    pub(crate) fn op_lds_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.s = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.s |= low;
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STS direct (0x10DF): Store S to memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 2 prefix + 4 exec (same exec pattern as STX direct).
    pub(crate) fn op_sts_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.s >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, self.s as u8);
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STU direct (0xDF): Store U to memory at DP:addr (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_stu_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.u >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                bus.write(master, self.temp_addr, self.u as u8);
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Extended addressing mode (8-bit load/store) ---

    /// LDA extended (0xB6): Load A from memory at 16-bit address.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_lda_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.alu_extended(opcode, cycle, bus, master, |cpu, val| {
            cpu.a = val;
            cpu.set_flags_logical(val);
        });
    }

    /// STA extended (0xB7): Store A to memory at 16-bit address.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_sta_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.a);
                self.set_flags_logical(self.a);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDB extended (0xF6): Load B from memory at 16-bit address.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldb_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.alu_extended(opcode, cycle, bus, master, |cpu, val| {
            cpu.b = val;
            cpu.set_flags_logical(val);
        });
    }

    /// STB extended (0xF7): Store B to memory at 16-bit address.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    /// 5 total cycles: 1 fetch + 4 exec.
    pub(crate) fn op_stb_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.b);
                self.set_flags_logical(self.b);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Extended addressing mode (16-bit load/store) ---

    /// LDD extended (0xFC): Load D (A:B) from memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_ldd_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.a = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                self.b = bus.read(master, self.temp_addr);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STD extended (0xFD): Store D (A:B) to memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_std_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, self.a);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.b);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDX extended (0xBE): Load X from memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_ldx_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.x = high << 8;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.x |= low;
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STX extended (0xBF): Store X to memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_stx_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.x >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.x as u8);
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LDU extended (0xFE): Load U from memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_ldu_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.u = high << 8;
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.u |= low;
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// STU extended (0xFF): Store U to memory at 16-bit address.
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    /// 6 total cycles: 1 fetch + 5 exec.
    pub(crate) fn op_stu_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                bus.write(master, self.temp_addr, (self.u >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                bus.write(master, self.temp_addr, self.u as u8);
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Indexed addressing mode (8-bit load/store) ---

    /// LDA indexed (0xA6): Load A from memory at indexed EA.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_lda_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.alu_indexed(opcode, cycle, bus, master, |cpu, val| {
            cpu.a = val;
            cpu.set_flags_logical(val);
        });
    }

    /// STA indexed (0xA7): Store A to memory at indexed EA.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_sta_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, self.a);
                self.set_flags_logical(self.a);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// LDB indexed (0xE6): Load B from memory at indexed EA.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldb_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.alu_indexed(opcode, cycle, bus, master, |cpu, val| {
            cpu.b = val;
            cpu.set_flags_logical(val);
        });
    }

    /// STB indexed (0xE7): Store B to memory at indexed EA.
    /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_stb_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, self.b);
                self.set_flags_logical(self.b);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    // --- Indexed addressing mode (16-bit load/store) ---

    /// LDD indexed (0xEC): Load D (A:B) from memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldd_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.a = high;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                self.b = bus.read(master, self.temp_addr);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// STD indexed (0xED): Store D (A:B) to memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_std_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, self.a);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                bus.write(master, self.temp_addr, self.b);
                let val = self.get_d();
                self.set_flags_logical16(val);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// LDX indexed (0xAE): Load X from memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldx_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.x = high << 8;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.x |= low;
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// STX indexed (0xAF): Store X to memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_stx_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, (self.x >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                bus.write(master, self.temp_addr, self.x as u8);
                self.set_flags_logical16(self.x);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// LDU indexed (0xEE): Load U from memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldu_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.u = high << 8;
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.u |= low;
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// STU indexed (0xEF): Store U to memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_stu_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, (self.u >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                bus.write(master, self.temp_addr, self.u as u8);
                self.set_flags_logical16(self.u);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    // --- LEA instructions ---

    /// LEAX indexed (0x30): Load Effective Address into X.
    /// Z set if result is zero. No other flags affected.
    /// 4+ total cycles: 1 fetch + 1 postbyte + mode overhead + 2 don't-care cycles.
    pub(crate) fn op_leax<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // LEA's own /VMA don't-care; the register is loaded on it
                self.dummy_vma(bus, master);
                self.x = self.temp_addr;
                self.set_flag(CcFlag::Z, self.x == 0);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// LEAY indexed (0x31): Load Effective Address into Y.
    /// Z set if result is zero. No other flags affected.
    /// 4+ total cycles: 1 fetch + 1 postbyte + mode overhead + 2 don't-care cycles.
    pub(crate) fn op_leay<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // LEA's own /VMA don't-care; the register is loaded on it
                self.dummy_vma(bus, master);
                self.y = self.temp_addr;
                self.set_flag(CcFlag::Z, self.y == 0);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// LEAS indexed (0x32): Load Effective Address into S.
    /// No flags affected.
    /// 4+ total cycles: 1 fetch + 1 postbyte + mode overhead + 2 don't-care cycles.
    pub(crate) fn op_leas<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // LEA's own /VMA don't-care; the register is loaded on it
                self.dummy_vma(bus, master);
                self.s = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// LEAU indexed (0x33): Load Effective Address into U.
    /// No flags affected.
    /// 4+ total cycles: 1 fetch + 1 postbyte + mode overhead + 2 don't-care cycles.
    pub(crate) fn op_leau<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            40 => {
                // LEA's own /VMA don't-care; the register is loaded on it
                self.dummy_vma(bus, master);
                self.u = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    // --- Page 2 Indexed load/store (16-bit) ---

    /// LDY indexed (0x10AE): Load Y from memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_ldy_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.y = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.y |= low;
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 50);
                }
            }
        }
    }

    /// STY indexed (0x10AF): Store Y to memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_sty_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, (self.y >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                bus.write(master, self.temp_addr, self.y as u8);
                self.set_flags_logical16(self.y);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 50);
                }
            }
        }
    }

    /// LDS indexed (0x10EE): Load S from memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_lds_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                let high = bus.read(master, self.temp_addr) as u16;
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.s = high << 8;
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                let low = bus.read(master, self.temp_addr) as u16;
                self.s |= low;
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 50);
                }
            }
        }
    }

    /// STS indexed (0x10EF): Store S to memory at indexed EA (16-bit).
    /// N set if result bit 15 is set. Z set if result is zero. V always cleared.
    pub(crate) fn op_sts_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                bus.write(master, self.temp_addr, (self.s >> 8) as u8);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.state = ExecState::ExecutePage2(opcode, 51);
            }
            51 => {
                bus.write(master, self.temp_addr, self.s as u8);
                self.set_flags_logical16(self.s);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 50);
                }
            }
        }
    }
}
