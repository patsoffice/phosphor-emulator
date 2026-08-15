use super::{CcFlag, ExecState, M6809};
use crate::core::{Bus, BusMaster};

impl M6809 {
    /// Generic helper for short branch instructions (8-bit offset).
    /// Takes 3 cycles: 1 (Fetch) + 1 (Read Offset) + 1 (/VMA don't-care).
    fn branch_short<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        condition: bool,
    ) {
        match cycle {
            0 => {
                // Cycle 1: Fetch offset
                let offset = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = offset as u16;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Cycle 2: /VMA don't-care; the offset is added on it if taken
                self.dummy_vma(bus, master);
                if condition {
                    let offset = self.temp_addr as u8 as i8;
                    self.pc = self.pc.wrapping_add(offset as u16);
                }
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // 0x20 BRA: Branch Always
    pub(crate) fn op_bra<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.branch_short(opcode, cycle, bus, master, true);
    }

    // 0x21 BRN: Branch Never (effectively a 3-cycle, 2-byte NOP)
    pub(crate) fn op_brn<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.branch_short(opcode, cycle, bus, master, false);
    }

    // 0x22 BHI: Branch if Higher (Unsigned >) -> C=0 and Z=0
    pub(crate) fn op_bhi<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & ((CcFlag::C as u8) | (CcFlag::Z as u8))) == 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x23 BLS: Branch if Lower or Same (Unsigned <=) -> C=1 or Z=1
    pub(crate) fn op_bls<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & ((CcFlag::C as u8) | (CcFlag::Z as u8))) != 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x24 BCC: Branch if Carry Clear (Higher or Same) -> C=0
    pub(crate) fn op_bcc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::C as u8)) == 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x25 BCS: Branch if Carry Set (Lower) -> C=1
    pub(crate) fn op_bcs<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::C as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x26 BNE: Branch if Not Equal (Z=0)
    pub(crate) fn op_bne<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::Z as u8)) == 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x27 BEQ: Branch if Equal (Z=1)
    pub(crate) fn op_beq<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::Z as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x28 BVC: Branch if Overflow Clear (V=0)
    pub(crate) fn op_bvc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::V as u8)) == 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x29 BVS: Branch if Overflow Set (V=1)
    pub(crate) fn op_bvs<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x2A BPL: Branch if Plus (N=0)
    pub(crate) fn op_bpl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::N as u8)) == 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x2B BMI: Branch if Minus (N=1)
    pub(crate) fn op_bmi<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::N as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, cond);
    }

    // 0x2C BGE: Branch if Greater or Equal (Signed) -> N == V
    pub(crate) fn op_bge<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, n == v);
    }

    // 0x2D BLT: Branch if Less Than (Signed) -> N != V
    pub(crate) fn op_blt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, n != v);
    }

    // 0x2E BGT: Branch if Greater Than (Signed) -> Z=0 and N=V
    pub(crate) fn op_bgt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let z = (self.cc & (CcFlag::Z as u8)) != 0;
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, !z && (n == v));
    }

    // 0x2F BLE: Branch if Less or Equal (Signed) -> Z=1 or N!=V
    pub(crate) fn op_ble<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let z = (self.cc & (CcFlag::Z as u8)) != 0;
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_short(opcode, cycle, bus, master, z || (n != v));
    }

    /// Generic helper for long branch instructions (16-bit offset, Page 2).
    /// Cycle timing after prefix: 3 cycles (not taken) or 4 cycles (taken).
    fn branch_long<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        condition: bool,
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
                // /VMA don't-care; a taken branch spends a second one
                self.dummy_vma(bus, master);
                if condition {
                    self.state = ExecState::ExecutePage2(opcode, 3);
                } else {
                    self.state = ExecState::Fetch;
                }
            }
            3 => {
                self.dummy_vma(bus, master);
                let offset = self.temp_addr as i16;
                self.pc = self.pc.wrapping_add(offset as u16);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // 0x1021 LBRN: Long Branch Never (effectively a 5-cycle NOP)
    pub(crate) fn op_lbrn<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.branch_long(opcode, cycle, bus, master, false);
    }

    // 0x1022 LBHI: Long Branch if Higher (Unsigned >) -> C=0 and Z=0
    pub(crate) fn op_lbhi<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & ((CcFlag::C as u8) | (CcFlag::Z as u8))) == 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1023 LBLS: Long Branch if Lower or Same (Unsigned <=) -> C=1 or Z=1
    pub(crate) fn op_lbls<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & ((CcFlag::C as u8) | (CcFlag::Z as u8))) != 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1024 LBCC: Long Branch if Carry Clear -> C=0
    pub(crate) fn op_lbcc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::C as u8)) == 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1025 LBCS: Long Branch if Carry Set -> C=1
    pub(crate) fn op_lbcs<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::C as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1026 LBNE: Long Branch if Not Equal -> Z=0
    pub(crate) fn op_lbne<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::Z as u8)) == 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1027 LBEQ: Long Branch if Equal -> Z=1
    pub(crate) fn op_lbeq<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::Z as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1028 LBVC: Long Branch if Overflow Clear -> V=0
    pub(crate) fn op_lbvc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::V as u8)) == 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x1029 LBVS: Long Branch if Overflow Set -> V=1
    pub(crate) fn op_lbvs<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x102A LBPL: Long Branch if Plus -> N=0
    pub(crate) fn op_lbpl<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::N as u8)) == 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x102B LBMI: Long Branch if Minus -> N=1
    pub(crate) fn op_lbmi<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let cond = (self.cc & (CcFlag::N as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, cond);
    }

    // 0x102C LBGE: Long Branch if Greater or Equal (Signed) -> N == V
    pub(crate) fn op_lbge<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, n == v);
    }

    // 0x102D LBLT: Long Branch if Less Than (Signed) -> N != V
    pub(crate) fn op_lblt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, n != v);
    }

    // 0x102E LBGT: Long Branch if Greater Than (Signed) -> Z=0 and N=V
    pub(crate) fn op_lbgt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let z = (self.cc & (CcFlag::Z as u8)) != 0;
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, !z && (n == v));
    }

    // 0x102F LBLE: Long Branch if Less or Equal (Signed) -> Z=1 or N!=V
    pub(crate) fn op_lble<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        let z = (self.cc & (CcFlag::Z as u8)) != 0;
        let n = (self.cc & (CcFlag::N as u8)) != 0;
        let v = (self.cc & (CcFlag::V as u8)) != 0;
        self.branch_long(opcode, cycle, bus, master, z || (n != v));
    }

    /// LBRA (0x16): Long Branch Always (Page 0).
    /// Unconditional branch with 16-bit signed offset.
    /// No flags affected. 5 cycles total (1 fetch + 2 offset + 2 /VMA don't-care).
    pub(crate) fn op_lbra<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                self.dummy_vma(bus, master);
                let offset = self.temp_addr as i16;
                self.pc = self.pc.wrapping_add(offset as u16);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// LBSR (0x17): Long Branch to Subroutine (Page 0).
    /// Pushes return address onto S stack, then branches to PC + 16-bit signed offset.
    /// No flags affected. 9 cycles total: 1 fetch + 2 offset bytes + 4 /VMA
    /// don't-care + 2 pushes. The don't-cares all precede the pushes.
    pub(crate) fn op_lbsr<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
            c @ 2..=5 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, c + 1);
            }
            6 => {
                // Push PC low byte (PC is already the return address)
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8);
                self.state = ExecState::Execute(opcode, 7);
            }
            7 => {
                // Push PC high byte, then take the branch
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8);
                let offset = self.temp_addr as i16;
                self.pc = self.pc.wrapping_add(offset as u16);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// BSR (0x8D): Branch to Subroutine.
    /// Pushes return address (PC after offset byte) onto S stack,
    /// then branches to PC + sign-extended 8-bit offset.
    /// No flags affected. 7 cycles total: 1 fetch + 1 offset byte + 3 /VMA
    /// don't-care + 2 pushes.
    pub(crate) fn op_bsr<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Read offset byte; PC is now the return address
                let offset = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = offset as u16;
                self.state = ExecState::Execute(opcode, 1);
            }
            c @ 1..=3 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, c + 1);
            }
            4 => {
                // Push PC low byte (PC is already the return address)
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8);
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Push PC high byte, then take the branch
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8);
                let offset = self.temp_addr as u8 as i8;
                self.pc = self.pc.wrapping_add(offset as u16);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// JSR direct (0x9D): Jump to Subroutine (direct addressing).
    /// Pushes return address onto S stack, then jumps to DP:offset.
    /// No flags affected. 7 cycles total: 1 fetch + 1 address byte + 3
    /// don't-care (/VMA, PC, /VMA) + 2 pushes.
    pub(crate) fn op_jsr_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Read address byte, form DP:addr target
                let addr_low = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | (addr_low as u16);
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                // Push PC low byte
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8);
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Push PC high byte, then jump
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8);
                self.pc = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// JMP indexed (0x6E): Jump to indexed EA.
    /// No flags affected.
    /// 3+ total cycles: 1 fetch + 1 postbyte + the mode's address formation.
    /// JMP has no execute cycle of its own — the jump is taken on the last
    /// don't-care of the address formation, so it lands the moment the resolver
    /// reports the address ready.
    pub(crate) fn op_jmp_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        if self.indexed_resolve(opcode, cycle, bus, master) {
            self.pc = self.temp_addr;
            self.state = ExecState::Fetch;
        }
    }

    /// JSR indexed (0xAD): Jump to Subroutine at indexed EA.
    /// Pushes return address onto S stack, then jumps to indexed EA.
    /// No flags affected. After the address is formed: one don't-care at PC,
    /// one /VMA, then the two pushes.
    pub(crate) fn op_jsr_indexed<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            50 => {
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(opcode, 51);
            }
            51 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 52);
            }
            52 => {
                // Push PC low byte
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8);
                self.state = ExecState::Execute(opcode, 53);
            }
            53 => {
                // Push PC high byte, then jump
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8);
                self.pc = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// JMP direct (0x0E): Jump to direct addressing EA (DP:addr).
    /// No flags affected.
    pub(crate) fn op_jmp_direct<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let addr_low = bus.read(master, self.pc) as u16;
                self.pc = ((self.dp as u16) << 8) | addr_low;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// JMP extended (0x7E): Jump to 16-bit address.
    /// No flags affected.
    pub(crate) fn op_jmp_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
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
                self.pc = self.temp_addr | low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// JSR extended (0xBD): Jump to Subroutine at 16-bit address.
    /// Pushes return address onto S stack, then jumps to 16-bit address.
    /// No flags affected. 8 cycles total: 1 fetch + 2 address bytes + 3
    /// don't-care (/VMA, PC, /VMA) + 2 pushes.
    pub(crate) fn op_jsr_extended<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Read address high byte
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Read address low byte
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
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                // Push PC low byte
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8);
                self.state = ExecState::Execute(opcode, 6);
            }
            6 => {
                // Push PC high byte, then jump
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8);
                self.pc = self.temp_addr;
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// RTS (0x39): Return from Subroutine.
    /// Pulls PC from S stack. No flags affected.
    /// 5 cycles total: 1 fetch + 1 don't-care at PC + 2 pulls + 1 read at the
    /// new S. That last read is real, not a don't-care: every pull sequence on
    /// this CPU ends by reading the byte the stack pointer has landed on.
    pub(crate) fn op_rts<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(0x39, 1);
            }
            1 => {
                // Pull PC high byte
                let high = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                self.temp_addr = (high as u16) << 8;
                self.state = ExecState::Execute(0x39, 2);
            }
            2 => {
                // Pull PC low byte
                let low = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                self.pc = self.temp_addr | (low as u16);
                self.state = ExecState::Execute(0x39, 3);
            }
            3 => {
                // Trailing read at the settled stack pointer
                let _ = bus.read(master, self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }
}
