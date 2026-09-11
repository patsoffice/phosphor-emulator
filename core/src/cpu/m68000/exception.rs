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
use super::addressing::{Abort, AccessResult, AddressError};
use super::flags::SrFlag;
use crate::core::{Bus16, BusMaster, bus::InterruptState};
use crate::cpu::flags::detect_rising_edge;

/// What the access that address-errors costs before entry begins.
///
/// **The part does not check the address first.** It puts the access on the
/// bus, spends the four clocks a bus cycle takes, and only then finds the
/// address odd, at which point it spends four more recognizing the fault and
/// entering. Eight clocks, on every fault, before any of exception entry's
/// fifty. The cycle commits nothing, because the address strobe is never
/// asserted, so it is time with no transfer and no observer sees it: nothing
/// is presented to the bus here, only charged.
///
/// **The two corpora disagree about this and the disagreement is exactly these
/// eight clocks.** The documentation-derived set charges nothing for the
/// aborted access and records six clocks of internal time; the
/// microcode-derived set charges the cycle and records ten. That is not the
/// manual against the microcode, because the manual's figure is for entry
/// alone and says nothing about the attempt: it is one generated
/// implementation omitting a mechanism the die-extracted one has, and the
/// mechanism is readable a step at a time.
///
/// **Adopting it makes one reported number worse and it is labeled rather than
/// hidden.** Against the documentation-derived corpus it takes the
/// address-error population from exact on all 178,089 cases to exact on two,
/// and the aggregate clock rate from 96.81% to 79.00%; against the
/// microcode-derived one it takes the same population from 0.00% to exact on
/// all 55,607 and the aggregate from 79.40% to 96.91%. That is the only floor
/// in this conversion that has come down, and it comes down because the
/// mechanism is readable and the rate is not the thing being optimized.
///
/// The two cases that do not move are both `MOVEM.l` loads, which suspend past
/// the replay cap and have already spent more clocks than their own length, so
/// the finish saturates rather than adding these eight. That is the cap, not
/// this constant.
const ABORTED_ACCESS_CLOCKS: u32 = 8;

impl M68000 {
    /// Enter an exception: push the stack frame on the supervisor stack and
    /// vector to the handler. `pushed_pc` is the PC value the frame stores
    /// (see the module docs for which address each source pushes).
    ///
    /// A misaligned supervisor stack makes the frame push itself fault; the
    /// error propagates so the address-error entry (and from there the
    /// double-fault halt) takes over.
    pub(crate) fn exception<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        vector: u8,
        pushed_pc: u32,
    ) -> AccessResult<()> {
        // Anything the aborted instruction handed over runs before the frame
        // does. The part committed those cycles; what it does not do is
        // interleave them with the entry sequence, and the entry is about to
        // change the privilege they would be driven at.
        self.flush_pending(bus, master);
        let old_sr = self.sr;
        self.set_supervisor(true);
        self.set_flag(SrFlag::T, false);
        // 68000 short frame — PC pushed first, SR at the lowest address.
        // The 68010+ add a format/vector-offset word above it (highest
        // address, so pushed first): format nibble 0 = the short format
        // these group-1/2 exceptions use, the low 12 bits the vector
        // number × 4. RTE consumes it. (The 68010 group-0 bus/address-error
        // frame is the larger format $8 and is not modeled — see README.)
        if self.uses_long_exception_frame() {
            // The 68010's order is not sourced from anything: its frame has no
            // recorded trace and no microcode listing in reach, so it keeps the
            // straightforward high-to-low push rather than borrowing the
            // 68000's order below on the assumption that they match.
            self.push_word(bus, master, vector as u16 * 4)?;
            self.push_long(bus, master, pushed_pc)?;
            self.push_word(bus, master, old_sr)?;
        } else {
            self.push_short_frame(bus, master, pushed_pc, old_sr)?;
        }
        let handler = self.read_long_at(bus, master, vector as u32 * 4)?;
        // Loading the vector is a control transfer and discards the queue; the
        // two words at the handler are fetched by the finish, and they are why
        // exception entry costs two transfers more than its frame and vector.
        self.set_pc_flush(handler);
        Ok(())
    }

    /// Push the 68000's three-word exception frame, in the order the part
    /// drives the writes rather than the order the words sit in.
    ///
    /// The frame is SR at the lowest address then the PC above it, but the part
    /// does not write it downwards. It writes **the low half of the PC first**,
    /// at `sp - 2`, then SR at `sp - 6`, then the high half at `sp - 4`. Three
    /// words, three addresses, one order that is none of the obvious ones.
    ///
    /// The stack pointer reaches its final value on the *second* write, not the
    /// first, so a frame that faults on its first write leaves A7 exactly as it
    /// was. That is why the decrement is placed between the writes here.
    ///
    /// Nothing above rung 4 of the per-cycle gate can see any of this: the
    /// addresses, the values and the transfer count are identical whichever
    /// order they are driven in, and so is the memory afterwards. It is visible
    /// to a device that watches the address bus, which is the reason to get it
    /// right rather than merely the reason it was found.
    fn push_short_frame<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        pushed_pc: u32,
        old_sr: u16,
    ) -> AccessResult<()> {
        let sp = self.a[7];
        self.write_word_at(bus, master, sp.wrapping_sub(2), pushed_pc as u16)?;
        self.a[7] = sp.wrapping_sub(6);
        self.write_word_at(bus, master, sp.wrapping_sub(6), old_sr)?;
        self.write_word_at(bus, master, sp.wrapping_sub(4), (pushed_pc >> 16) as u16)
    }

    /// TRAP #n (0x4E40-0x4E4F): unconditional trap to vector 32 + n. The
    /// frame PC is the following instruction.
    ///
    /// Flags: none directly (exception entry sets S, clears T). 38 cycles.
    pub(crate) fn op_trap<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let vector = 32 + (opcode & 0xF) as u8;
        self.exception(bus, master, vector, self.pc)?;
        // Exception entry's seven transfers are counted: three frame words,
        // the two-word vector, and the two refills at the handler. Six clocks
        // are left, which puts TRAP at 34 and agrees with the other group-2
        // entries: the illegal-instruction and privilege-violation vectors are
        // also seven transfers and six, and TRAPV taken is eight and two.
        //
        // The manual says 38. Both corpora record 34, independently generated,
        // and where they agree against this core the rule is that this core is
        // wrong; the four clocks were this instruction's alone, because the
        // sibling entries above already came out at 34.
        self.finish_from_bus(bus, master, 6);
        Ok(())
    }

    /// TRAPV (0x4E76): trap to vector 7 if V is set, otherwise continue.
    ///
    /// Flags: none. 34 cycles taken, 4 not taken.
    pub(crate) fn op_trapv<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if self.flag_is_set(SrFlag::V) {
            // The refill behind the opcode precedes the frame, as CHK's does:
            // TRAPV taken is recorded as a program read, three frame writes,
            // the vector, and two refills, and it is eight transfers where
            // TRAP, which traps without testing anything, is seven.
            self.refill_prefetch(bus, master);
            self.exception(bus, master, 7, self.pc)?;
            self.finish_from_bus(bus, master, 2);
        } else {
            self.finish_from_bus(bus, master, 0);
        }
        Ok(())
    }

    /// Illegal-instruction family: ILLEGAL (0x4AFC) and unassigned opcodes
    /// vector to 4, line-A opcodes to 10, line-F to 11. The frame PC is the
    /// unexecuted opcode itself.
    ///
    /// Flags: none. 34 cycles.
    pub(crate) fn op_illegal<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        vector: u8,
    ) -> AccessResult<()> {
        self.exception(bus, master, vector, self.instr_pc)?;
        self.finish_from_bus(bus, master, 6);
        Ok(())
    }

    /// Verify supervisor privilege for a privileged instruction. In user
    /// mode the privilege-violation exception (vector 8) is entered with
    /// the frame PC at the unexecuted instruction, and false is returned.
    pub(crate) fn privilege_check<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<bool> {
        if self.flag_is_set(SrFlag::S) {
            return Ok(true);
        }
        self.exception(bus, master, 8, self.instr_pc)?;
        self.finish_from_bus(bus, master, 6);
        Ok(false)
    }

    /// Load a full status-register value: route the S bit through the
    /// stack-pointer swap and mask the bits the 68000 does not implement
    /// (only T, S, the interrupt mask, and the CCR exist).
    ///
    /// **Writing the status register discards the prefetch queue.** The queued
    /// words were fetched in the old privilege state, and the recorded traces
    /// show the part fetching two words afterwards where an ordinary
    /// instruction fetches one: `MOVE.w D3, SR` is two program reads for one
    /// word consumed, and `ANDI to SR` is three for two. The CCR-only forms do
    /// the same, sharing the microcode, so [`Self::write_ccr`] flushes too.
    pub(crate) fn write_sr(&mut self, value: u16) {
        self.set_supervisor(value & SrFlag::S as u16 != 0);
        self.sr = value & 0xA71F;
        self.flush_prefetch();
    }

    /// Load the five implemented condition-code bits, leaving the system byte
    /// alone. Discards the prefetch queue for the reason [`Self::write_sr`]
    /// gives.
    pub(crate) fn write_ccr(&mut self, value: u16) {
        self.sr = (self.sr & 0xFF00) | (value & 0x001F);
        self.flush_prefetch();
    }

    /// ANDI/ORI/EORI to CCR (0x023C/0x003C/0x0A3C) and to SR
    /// (0x027C/0x007C/0x0A7C): combine the immediate with the flag byte or
    /// the whole status register. The SR forms are privileged.
    ///
    /// Flags: per the operation, on the five CCR bits (and the system byte
    /// for the SR forms). 20 cycles.
    pub(crate) fn op_sr_imm<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let to_sr = opcode & 0x0040 != 0;
        if to_sr && !self.privilege_check(bus, master)? {
            return Ok(());
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
            self.write_ccr(combine(self.sr & 0x00FF, imm & 0x00FF));
        }
        // Three transfers: the refill behind the immediate word, then the two
        // at the refetch the status-register write forces. Eight clocks are
        // left, spent settling the mode the write may just have changed, and
        // they run *between* the two: the part's sequence is the immediate's
        // fetch, four two-clock steps, then the refetch. So the recorded
        // clocks are 0, 12 and 16, not 0, 4 and 8.
        self.finish_from_bus_address_first(bus, master, 8);
        Ok(())
    }

    /// RTE (0x4E73, privileged): pop SR then PC from the supervisor stack
    /// and resume the interrupted context. The 68010+ frame carries an
    /// extra format/vector-offset word above the PC; we only ever stack the
    /// format $0 short frame, so it is popped and discarded with no
    /// format-error (vector 14) check.
    ///
    /// Flags: the whole SR is restored from the frame. 20 cycles.
    pub(crate) fn op_rte<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if !self.privilege_check(bus, master)? {
            return Ok(());
        }
        let sr = self.pop_word(bus, master)?;
        let pc = self.pop_long(bus, master)?;
        if self.uses_long_exception_frame() {
            let _format = self.pop_word(bus, master)?;
        }
        self.write_sr(sr);
        self.set_pc_checked(pc)?;
        // Every popped word is a counted transfer, including the 68010's
        // format word, so the longer frame costs its own four clocks. The two
        // refills at the resumed address are the rest of the twenty, and
        // nothing is left over.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }

    /// STOP #imm (0x4E72, privileged): load the immediate into SR and halt
    /// instruction execution until an interrupt (or reset) arrives.
    ///
    /// Flags: the whole SR is loaded. 4 cycles.
    pub(crate) fn op_stop<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if !self.privilege_check(bus, master)? {
            return Ok(());
        }
        // Both of STOP's words come out of the queue and neither is refilled:
        // a supervisor STOP is recorded at four clocks with no bus cycle at
        // all, and its recorded final state has PC and the queue exactly where
        // they started. The part stops before issuing the refill, so this is
        // the one instruction that finishes without one. It was charged a flat
        // documented total until the queue existed to say why.
        let imm = self.read_imm_word_no_refill(bus, master);
        self.write_sr(imm);
        self.stopped = true;
        self.finish_without_refill(4);
        Ok(())
    }

    /// RESET (0x4E70, privileged): assert the external reset line for 124
    /// clocks. Devices are not wired to the line in this core yet, so the
    /// instruction is a long supervisor no-op; CPU state is unaffected.
    ///
    /// Flags: none. 132 cycles.
    pub(crate) fn op_reset_instruction<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if !self.privilege_check(bus, master)? {
            return Ok(());
        }
        // RESET asserts its line for 124 clocks and does nothing on the bus
        // besides its own fetch, which comes *after* the line is released: the
        // recorded trace puts that single transfer on clock 128.
        self.finish_from_bus_address_first(bus, master, 128);
        Ok(())
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
    pub(crate) fn enter_interrupt<B: Bus16 + ?Sized>(
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
        let pushed_pc = self.pc;
        match self.exception(bus, master, vector, pushed_pc) {
            Ok(()) => {}
            // Misaligned supervisor stack: the entry itself address-errors.
            Err(Abort::Fault(fault)) => {
                self.enter_address_error(bus, master, fault);
                return;
            }
            // Interrupt recognition runs from the state machine, not from a
            // body, so there is nothing to unwind to and nothing asks it to:
            // `M68000::must_suspend` is false outside a body.
            Err(Abort::Suspend) => {
                debug_assert!(false, "exception entry cannot suspend outside a body");
                return;
            }
        }
        self.set_interrupt_mask(level);
        // No instruction runs: the entry sequence's own seven transfers are
        // the whole bus cost, three frame words, the vector and the two
        // refills at the handler, leaving the recognition and the vector
        // arithmetic.
        self.finish_from_bus(bus, master, 16);
    }

    /// Address-error (vector 3) entry with the 68000 seven-word group-0
    /// frame: status word, access address, instruction register, SR, PC.
    ///
    /// Hardware-verified frame contents: the status word carries the
    /// opcode's upper 11 bits (the internal IR rides along on the bus)
    /// above R/W, I/N, and the function code; the stacked PC follows the
    /// per-fault rules recorded in [`AddressError`]. A fault while pushing
    /// this frame is a double bus fault: the processor halts. 50 cycles.
    pub(crate) fn enter_address_error<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        fault: AddressError,
    ) {
        // Anything the aborted instruction handed over runs before the frame
        // does. The part committed those cycles; what it does not do is
        // interleave them with the entry sequence, and the entry is about to
        // change the privilege they would be driven at.
        self.flush_pending(bus, master);
        let old_sr = self.sr;
        self.set_supervisor(true);
        self.set_flag(SrFlag::T, false);
        let fc: u16 = match (fault.program, old_sr & SrFlag::S as u16 != 0) {
            (true, true) => 6,
            (true, false) => 2,
            (false, true) => 5,
            (false, false) => 1,
        };
        let status = (self.opcode & 0xFFE0)
            | if fault.write { 0 } else { 0x10 }
            | if fault.program { 0x08 } else { 0 }
            | fc;
        let opcode = self.opcode;
        let frame = self
            .push_long(bus, master, fault.stacked_pc)
            .and_then(|()| self.push_word(bus, master, old_sr))
            .and_then(|()| self.push_word(bus, master, opcode))
            .and_then(|()| self.push_long(bus, master, fault.addr))
            .and_then(|()| self.push_word(bus, master, status));
        if frame.is_err() {
            // Address error during address-error processing: double bus
            // fault; only an external reset recovers.
            self.halted = true;
            return;
        }
        let handler = self
            .read_long_at(bus, master, 3 * 4)
            .expect("vector 3 is aligned");
        self.set_pc_flush(handler);
        // Fifty clocks of entry, and every one of them is accounted for by
        // mechanism: eleven transfers and six idle. Seven transfers are the
        // group-0 frame, two the vector, and two the refills at the handler
        // that the flush above makes the finish issue.
        //
        // This site charged a flat documented total until M3, and the two
        // missing prefetches were exactly why: charging it from the bus without
        // them cost the 680x0 corpus 4.56 points, because neither side's
        // transfer count was a subset of the other's. The queue is what makes
        // the two counts the same count.
        //
        // **The aborted instruction's own internal time is part of the length
        // too, and only this site can add it.** The part computes an address,
        // drives the access, and only then finds it odd, so those clocks are
        // spent whatever happens next; a body that runs to its end declares
        // them at its finish, and an aborted one never reaches that.
        // `internal_spent` is the record of them. Leaving it out made every
        // faulting instruction with a predecrement, an index add, a jump target
        // or a branch displacement end two or six clocks short, and both
        // corpora agreed on which: 77,019 cases and 22,032, exactly the shapes
        // whose internal time is not zero.
        self.finish_from_bus(
            bus,
            master,
            6 + ABORTED_ACCESS_CLOCKS + self.internal_spent,
        );
    }
}
