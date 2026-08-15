use super::{ExecState, M6809};
use crate::core::{Bus, BusMaster};

impl M6809 {
    // Register IDs for TFR/EXG
    // 16-bit: 0=D, 1=X, 2=Y, 3=U, 4=S, 5=PC
    // 8-bit: 8=A, 9=B, 10=CC, 11=DP
    // No register: 6, 7, 12, 13, 14, 15
    //
    // TFR/EXG move a register through a 16-bit internal path. A register only
    // drives as many of those bits as it is wide; the bits above it read back
    // as ones, and a register narrower than the path only latches the low bits
    // it can hold. So an 8-bit register reads as $FFnn and writes only the low
    // byte, and a code with no register behind it reads as $FFFF and latches
    // nothing — which is what makes a mixed-size TFR fill the other half with
    // $FF. Motorola documents mismatched sizes as undefined; this is the
    // behaviour of the part.

    /// Read register `id` onto the 16-bit transfer path, filling the bits the
    /// register does not drive with ones.
    fn get_reg_val(&self, id: u8) -> u16 {
        match id {
            0 => self.get_d(),
            1 => self.x,
            2 => self.y,
            3 => self.u,
            4 => self.s,
            5 => self.pc,
            8 => 0xFF00 | self.a as u16,
            9 => 0xFF00 | self.b as u16,
            10 => 0xFF00 | self.cc as u16,
            11 => 0xFF00 | self.dp as u16,
            _ => 0xFFFF, // no register drives the path
        }
    }

    /// Latch the transfer path into register `id`, keeping only the low bits
    /// the register is wide enough to hold.
    fn set_reg_val(&mut self, id: u8, val: u16) {
        match id {
            0 => self.set_d(val),
            1 => self.x = val,
            2 => self.y = val,
            3 => self.u = val,
            4 => self.s = val,
            5 => self.pc = val,
            8 => self.a = val as u8,
            9 => self.b = val as u8,
            10 => self.cc = val as u8,
            11 => self.dp = val as u8,
            _ => {} // no register latches the path
        }
    }

    /// TFR immediate (0x1F): Transfer register R1 to R2.
    /// Operand: High nibble = Source, Low nibble = Dest.
    /// Mismatched sizes are documented undefined but do transfer — see the
    /// note on `get_reg_val`. No condition codes are affected, unless CC is
    /// itself the destination.
    /// 6 cycles total: 1 fetch + 1 read operand + 4 /VMA don't-care.
    pub(crate) fn op_tfr<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let operand = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = operand as u16;
                self.state = ExecState::Execute(0x1F, 1);
            }
            1..=3 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(0x1F, cycle + 1);
            }
            4 => {
                self.dummy_vma(bus, master);
                let operand = self.temp_addr as u8;
                let src = operand >> 4;
                let dst = operand & 0x0F;
                let val = self.get_reg_val(src);
                self.set_reg_val(dst, val);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// EXG immediate (0x1E): Exchange registers R1 and R2.
    /// Operand: High nibble = R1, Low nibble = R2.
    /// Mismatched sizes follow the same rule as TFR, applied in both
    /// directions. No condition codes are affected, unless CC is one of the
    /// two registers.
    /// 8 cycles total: 1 fetch + 1 read operand + 6 /VMA don't-care.
    pub(crate) fn op_exg<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                let operand = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = operand as u16;
                self.state = ExecState::Execute(0x1E, 1);
            }
            1..=5 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(0x1E, cycle + 1);
            }
            6 => {
                self.dummy_vma(bus, master);
                let operand = self.temp_addr as u8;
                let r1 = operand >> 4;
                let r2 = operand & 0x0F;
                let val1 = self.get_reg_val(r1);
                let val2 = self.get_reg_val(r2);
                self.set_reg_val(r1, val2);
                self.set_reg_val(r2, val1);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }
}
