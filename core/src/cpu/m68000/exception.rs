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
use crate::core::{Bus, BusMaster};

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
}
