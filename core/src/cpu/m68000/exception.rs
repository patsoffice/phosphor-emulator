//! M68000 exception processing: vector dispatch, the supervisor stack
//! frame, and the instruction-generated (group 2) exceptions.
//!
//! Every exception enters through [`M68000::exception`]: copy the SR, force
//! supervisor mode (swapping in the SSP), clear trace, push the frame, and
//! load PC from the vector table. The frame's stacked PC differs by source —
//! the *next* instruction for traps the instruction completed (TRAP, TRAPV,
//! CHK, divide by zero), the *unexecuted* opcode itself for illegal
//! instruction and privilege violation.
//!
//! Exception processing times follow M68000UM table 8-14 (approximate, like
//! all timing in this core).

use super::M68000;
use super::flags::SrFlag;
use crate::core::{Bus, BusMaster, bus::InterruptState};
use crate::cpu::flags::detect_rising_edge;

impl M68000 {
    /// Enter an exception: push the stack frame on the supervisor stack and
    /// vector to the handler. `pushed_pc` is the PC value the frame stores
    /// (see the module docs for which address each source pushes).
    pub(crate) fn exception<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        vector: u8,
        pushed_pc: u32,
    ) {
        let old_sr = self.sr;
        self.set_supervisor(true);
        self.set_flag(SrFlag::T, false);
        // 68000 short frame — PC pushed first, SR at the lowest address.
        // 68010+ add a format/vector word; gate on `self.variant` here when
        // those frames are implemented.
        self.push_long(bus, master, pushed_pc);
        self.push_word(bus, master, old_sr);
        self.pc = self.read_long_at(bus, master, vector as u32 * 4);
    }

    /// TRAP #n (0x4E40-0x4E4F): unconditional trap to vector 32 + n. The
    /// frame PC is the following instruction.
    ///
    /// Flags: none directly (exception entry sets S, clears T). 38 cycles.
    pub(crate) fn op_trap<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let vector = 32 + (opcode & 0xF) as u8;
        self.exception(bus, master, vector, self.pc);
        self.finish(38);
    }

    /// TRAPV (0x4E76): trap to vector 7 if V is set, otherwise continue.
    ///
    /// Flags: none. 34 cycles taken, 4 not taken.
    pub(crate) fn op_trapv<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        if self.flag_is_set(SrFlag::V) {
            self.exception(bus, master, 7, self.pc);
            self.finish(34);
        } else {
            self.finish(4);
        }
    }

    /// Illegal-instruction family: ILLEGAL (0x4AFC) and unassigned opcodes
    /// vector to 4, line-A opcodes to 10, line-F to 11. The frame PC is the
    /// unexecuted opcode itself.
    ///
    /// Flags: none. 34 cycles.
    pub(crate) fn op_illegal<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        vector: u8,
    ) {
        self.exception(bus, master, vector, self.instr_pc);
        self.finish(34);
    }

    /// Verify supervisor privilege for a privileged instruction. In user
    /// mode the privilege-violation exception (vector 8) is entered with
    /// the frame PC at the unexecuted instruction, and false is returned.
    pub(crate) fn privilege_check<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        if self.flag_is_set(SrFlag::S) {
            return true;
        }
        self.exception(bus, master, 8, self.instr_pc);
        self.finish(34);
        false
    }

    /// Load a full status-register value: route the S bit through the
    /// stack-pointer swap and mask the bits the 68000 does not implement
    /// (only T, S, the interrupt mask, and the CCR exist).
    pub(crate) fn write_sr(&mut self, value: u16) {
        self.set_supervisor(value & SrFlag::S as u16 != 0);
        self.sr = value & 0xA71F;
    }

    /// ANDI/ORI/EORI to CCR (0x023C/0x003C/0x0A3C) and to SR
    /// (0x027C/0x007C/0x0A7C): combine the immediate with the flag byte or
    /// the whole status register. The SR forms are privileged.
    ///
    /// Flags: per the operation, on the five CCR bits (and the system byte
    /// for the SR forms). 20 cycles.
    pub(crate) fn op_sr_imm<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) {
        let to_sr = opcode & 0x0040 != 0;
        if to_sr && !self.privilege_check(bus, master) {
            return;
        }
        let imm = self.read_imm_word(bus, master);
        let combine = |a: u16, b: u16| match opcode & 0x0F00 {
            0x0200 => a & b, // ANDI
            0x0A00 => a ^ b, // EORI
            _ => a | b,      // ORI
        };
        if to_sr {
            self.write_sr(combine(self.sr, imm));
        } else {
            let ccr = combine(self.sr & 0x00FF, imm & 0x00FF);
            self.sr = (self.sr & 0xFF00) | (ccr & 0x001F);
        }
        self.finish(20);
    }

    /// RTE (0x4E73, privileged): pop SR then PC from the supervisor stack
    /// and resume the interrupted context. The 68000 frame has no format
    /// word (68010+ gate here when implemented).
    ///
    /// Flags: the whole SR is restored from the frame. 20 cycles.
    pub(crate) fn op_rte<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        if !self.privilege_check(bus, master) {
            return;
        }
        let sr = self.pop_word(bus, master);
        let pc = self.pop_long(bus, master);
        self.write_sr(sr);
        self.set_pc_checked(pc);
        self.finish(20);
    }

    /// STOP #imm (0x4E72, privileged): load the immediate into SR and halt
    /// instruction execution until an interrupt (or reset) arrives.
    ///
    /// Flags: the whole SR is loaded. 4 cycles.
    pub(crate) fn op_stop<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        if !self.privilege_check(bus, master) {
            return;
        }
        let imm = self.read_imm_word(bus, master);
        self.write_sr(imm);
        self.stopped = true;
        self.finish(4);
    }

    /// RESET (0x4E70, privileged): assert the external reset line for 124
    /// clocks. Devices are not wired to the line in this core yet, so the
    /// instruction is a long supervisor no-op; CPU state is unaffected.
    ///
    /// Flags: none. 132 cycles.
    pub(crate) fn op_reset_instruction<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        if !self.privilege_check(bus, master) {
            return;
        }
        self.finish(132);
    }

    /// Decide whether a sampled interrupt should be taken at this
    /// instruction boundary: level 7 (NMI) is edge-triggered, levels 1-6
    /// are taken while above the SR mask.
    pub(crate) fn pending_interrupt(&mut self, ints: InterruptState) -> Option<u8> {
        let level = ints.irq_level & 7;
        let level7_edge = detect_rising_edge(level == 7, &mut self.nmi_previous);
        if level == 7 {
            return level7_edge.then_some(7);
        }
        (level > self.interrupt_mask()).then_some(level)
    }

    /// Take a level-`level` interrupt: wake from STOP, stack the frame
    /// (PC = the next unexecuted instruction), raise the mask to the taken
    /// level, and vector — the autovector `24 + level` unless the device
    /// supplied one (`irq_vector` other than 0xFF, the bus default).
    ///
    /// 44 cycles.
    pub(crate) fn enter_interrupt<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        level: u8,
        irq_vector: u8,
    ) {
        self.stopped = false;
        let vector = if irq_vector != 0xFF {
            irq_vector
        } else {
            24 + level
        };
        self.exception(bus, master, vector, self.pc);
        self.set_interrupt_mask(level);
        self.finish(44);
    }

    /// Address-error (vector 3) entry with the 68000 seven-word group-0
    /// frame: status word, access address, instruction register, SR, PC.
    ///
    /// Approximate by design: real hardware aborts the instruction
    /// mid-flight, while this core completes it with the offending access
    /// forced even, so the stacked PC/SR reflect the completed instruction
    /// (the validation harness skips these vectors). A misaligned
    /// supervisor stack would double-fault to a halt on hardware; here the
    /// frame pushes are themselves forced even. 50 cycles.
    pub(crate) fn enter_address_error<B: Bus<Address = u32, Data = u16> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let old_sr = self.sr;
        self.set_supervisor(true);
        self.set_flag(SrFlag::T, false);
        self.push_long(bus, master, self.pc);
        self.push_word(bus, master, old_sr);
        self.push_word(bus, master, self.opcode);
        self.push_long(bus, master, self.fault_addr);
        // Status word: R/W in bit 4, I/N = 0, FC2-0 = data space at the
        // pre-fault privilege (program-space faults are not distinguished).
        let fc: u16 = if old_sr & SrFlag::S as u16 != 0 { 5 } else { 1 };
        let status = if self.fault_read { 0x10 } else { 0 } | fc;
        self.push_word(bus, master, status);
        self.pc = self.read_long_at(bus, master, 3 * 4);
        self.finish(50);
    }
}
