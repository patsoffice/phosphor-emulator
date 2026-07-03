use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;
use crate::cpu::m6809::{CcFlag, ExecState, M6809};

impl M6809 {
    /// NEGA inherent (0x40): Negate A (A = 0 - A, two's complement).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if A was 0x80 (-128), since -(-128) overflows signed 8-bit range.
    /// C set if A was non-zero (borrow occurred from 0).
    pub(crate) fn op_nega(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.perform_neg(self.a);
            self.state = ExecState::Fetch;
        }
    }

    /// NEGB inherent (0x50): Negate B (B = 0 - B, two's complement).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if B was 0x80 (-128), since -(-128) overflows signed 8-bit range.
    /// C set if B was non-zero (borrow occurred from 0).
    pub(crate) fn op_negb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.perform_neg(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// COMA inherent (0x43): Complement A (A = ~A, one's complement / bitwise NOT).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V always cleared. C always set.
    pub(crate) fn op_coma(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.perform_com(self.a);
            self.state = ExecState::Fetch;
        }
    }

    /// COMB inherent (0x53): Complement B (B = ~B, one's complement / bitwise NOT).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V always cleared. C always set.
    pub(crate) fn op_comb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.perform_com(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// CLRA inherent (0x4F): Clear A (A = 0).
    /// Flags are always set to fixed values: N=0, Z=1, V=0, C=0.
    pub(crate) fn op_clra(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.perform_clr();
            self.state = ExecState::Fetch;
        }
    }

    /// CLRB inherent (0x5F): Clear B (B = 0).
    /// Flags are always set to fixed values: N=0, Z=1, V=0, C=0.
    pub(crate) fn op_clrb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.perform_clr();
            self.state = ExecState::Fetch;
        }
    }

    /// INCA inherent (0x4C): Increment A (A = A + 1).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if A was 0x7F before increment (positive-to-negative signed overflow).
    /// C is not affected.
    pub(crate) fn op_inca(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.perform_inc(self.a);
            self.state = ExecState::Fetch;
        }
    }

    /// INCB inherent (0x5C): Increment B (B = B + 1).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if B was 0x7F before increment (positive-to-negative signed overflow).
    /// C is not affected.
    pub(crate) fn op_incb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.perform_inc(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// DECA inherent (0x4A): Decrement A (A = A - 1).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if A was 0x80 before decrement (negative-to-positive signed overflow).
    /// C is not affected.
    pub(crate) fn op_deca(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = self.perform_dec(self.a);
            self.state = ExecState::Fetch;
        }
    }

    /// DECB inherent (0x5A): Decrement B (B = B - 1).
    /// N set if result bit 7 is set. Z set if result is zero.
    /// V set if B was 0x80 before decrement (negative-to-positive signed overflow).
    /// C is not affected.
    pub(crate) fn op_decb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.b = self.perform_dec(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// TSTA inherent (0x4D): Test A (set flags based on A, no modification).
    /// N set if A bit 7 is set. Z set if A is zero. V always cleared.
    pub(crate) fn op_tsta(&mut self, cycle: u8) {
        if cycle == 0 {
            self.perform_tst(self.a);
            self.state = ExecState::Fetch;
        }
    }

    /// TSTB inherent (0x5D): Test B (set flags based on B, no modification).
    /// N set if B bit 7 is set. Z set if B is zero. V always cleared.
    pub(crate) fn op_tstb(&mut self, cycle: u8) {
        if cycle == 0 {
            self.perform_tst(self.b);
            self.state = ExecState::Fetch;
        }
    }

    /// NOP inherent (0x12): No operation.
    /// No flags affected.
    pub(crate) fn op_nop(&mut self, cycle: u8) {
        if cycle == 0 {
            self.state = ExecState::Fetch;
        }
    }

    /// SEX inherent (0x1D): Sign-extend B into A.
    /// If B bit 7 is set, A = 0xFF; otherwise A = 0x00.
    /// N set if result is negative. Z set if D (A:B) is zero.
    pub(crate) fn op_sex(&mut self, cycle: u8) {
        if cycle == 0 {
            self.a = if self.b & 0x80 != 0 { 0xFF } else { 0x00 };
            let d = self.get_d();
            self.set_flag(CcFlag::N, d & 0x8000 != 0);
            self.set_flag(CcFlag::Z, d == 0);
            self.state = ExecState::Fetch;
        }
    }

    /// ABX inherent (0x3A): Add B (unsigned) to X.
    /// X = X + B. No flags affected.
    /// 3 total cycles: 1 fetch + 2 exec (internal + compute).
    pub(crate) fn op_abx(&mut self, cycle: u8) {
        match cycle {
            0 => {
                // Internal cycle
                self.state = ExecState::Execute(0x3A, 1);
            }
            1 => {
                self.x = self.x.wrapping_add(self.b as u16);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// DAA inherent (0x19): Decimal Adjust A after BCD addition.
    /// Adjusts A to produce valid BCD result after ADDA/ADCA.
    /// N set if result bit 7 is set. Z set if result is zero.
    /// C set if addition of A + correction overflows byte. V undefined (left unchanged).
    pub(crate) fn op_daa(&mut self, cycle: u8) {
        if cycle == 0 {
            let mut correction: u8 = 0;
            let lsn = self.a & 0x0F;

            if lsn > 0x09 || (self.cc & (CcFlag::H as u8) != 0) {
                correction = 0x06;
            }

            if (self.cc & (CcFlag::C as u8) != 0)
                || ((self.a & 0xF0) >> 4) > 9
                || (((self.a & 0xF0) >> 4) > 8 && lsn > 9)
            {
                correction |= 0x60;
            }

            let result = self.a as u16 + correction as u16;
            self.a = result as u8;
            self.set_flag(CcFlag::N, self.a & 0x80 != 0);
            self.set_flag(CcFlag::Z, self.a == 0);
            self.set_flag(CcFlag::C, result & 0x0100 != 0);
            self.state = ExecState::Fetch;
        }
    }

    // --- Direct addressing mode (memory unary ops, 0x00-0x0F) ---

    /// NEG direct (0x00): Negate memory byte at DP:addr.
    pub(crate) fn op_neg_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, val| cpu.perform_neg(val));
    }

    /// COM direct (0x03): Complement memory byte at DP:addr.
    pub(crate) fn op_com_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, val| cpu.perform_com(val));
    }

    /// DEC direct (0x0A): Decrement memory byte at DP:addr.
    pub(crate) fn op_dec_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, val| cpu.perform_dec(val));
    }

    /// INC direct (0x0C): Increment memory byte at DP:addr.
    pub(crate) fn op_inc_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, val| cpu.perform_inc(val));
    }

    /// TST direct (0x0D): Test memory byte at DP:addr.
    /// Shares RMW timing (6 cycles) but performs no write-back — the final cycle
    /// is a dummy VMA read (handled in `rmw_direct`).
    pub(crate) fn op_tst_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, val| {
            cpu.perform_tst(val);
            val
        });
    }

    /// CLR direct (0x0F): Clear memory byte at DP:addr.
    pub(crate) fn op_clr_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_direct(opcode, cycle, bus, master, |cpu, _val| cpu.perform_clr());
    }

    // --- Extended addressing mode (memory unary ops, 0x70-0x7F) ---

    /// NEG extended (0x70): Negate memory byte at 16-bit address.
    pub(crate) fn op_neg_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, val| cpu.perform_neg(val));
    }

    /// COM extended (0x73): Complement memory byte at 16-bit address.
    pub(crate) fn op_com_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, val| cpu.perform_com(val));
    }

    /// DEC extended (0x7A): Decrement memory byte at 16-bit address.
    pub(crate) fn op_dec_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, val| cpu.perform_dec(val));
    }

    /// INC extended (0x7C): Increment memory byte at 16-bit address.
    pub(crate) fn op_inc_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, val| cpu.perform_inc(val));
    }

    /// TST extended (0x7D): Test memory byte at 16-bit address.
    /// Shares RMW timing (7 cycles) but performs no write-back — the final cycle
    /// is a dummy VMA read (handled in `rmw_extended`).
    pub(crate) fn op_tst_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, val| {
            cpu.perform_tst(val);
            val
        });
    }

    /// CLR extended (0x7F): Clear memory byte at 16-bit address.
    pub(crate) fn op_clr_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_extended(opcode, cycle, bus, master, |cpu, _val| cpu.perform_clr());
    }

    // --- Indexed addressing mode (memory unary ops, 0x60-0x6F) ---

    /// NEG indexed (0x60): Negate memory byte at indexed EA.
    pub(crate) fn op_neg_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, val| cpu.perform_neg(val));
    }

    /// COM indexed (0x63): Complement memory byte at indexed EA.
    pub(crate) fn op_com_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, val| cpu.perform_com(val));
    }

    /// DEC indexed (0x6A): Decrement memory byte at indexed EA.
    pub(crate) fn op_dec_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, val| cpu.perform_dec(val));
    }

    /// INC indexed (0x6C): Increment memory byte at indexed EA.
    pub(crate) fn op_inc_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, val| cpu.perform_inc(val));
    }

    /// TST indexed (0x6D): Test memory byte at indexed EA.
    /// Shares RMW timing but performs no write-back — the final cycle is a dummy
    /// VMA read (handled in `rmw_indexed`).
    pub(crate) fn op_tst_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, val| {
            cpu.perform_tst(val);
            val
        });
    }

    /// CLR indexed (0x6F): Clear memory byte at indexed EA.
    pub(crate) fn op_clr_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.rmw_indexed(opcode, cycle, bus, master, |cpu, _val| cpu.perform_clr());
    }
}
