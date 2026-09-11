//! Motorola 68000 CPU emulation.
//!
//! The 68000 is a 16-bit-data / 32-bit-register big-endian CPU with eight
//! data registers, eight address registers (A7 doubles as the active stack
//! pointer), supervisor/user privilege modes, and a 256-entry vectored
//! exception table. The external data bus is 16 bits wide: the bus interface
//! uses `Address = u32` (24-bit physical address space on the 68000) and
//! `Data = u16` (one bus transaction = one word at an even address).
//!
//! Execution is modeled at the instruction level (like the i8088): the full
//! instruction is decoded and applied atomically on its first cycle, then the
//! remaining cycles are burned as bus-idle wait states. The bus activity itself
//! is modeled: byte accesses are strobed, every transfer costs four clocks, and
//! instruction words come out of a real two-word prefetch queue
//! ([`prefetch`]). What is not modeled yet is *when* within an instruction each
//! transfer runs.

pub(crate) mod addressing;
mod alu;
mod bit;
mod branch;
mod disasm;
mod exception;
pub mod flags;
pub mod format;
mod move_ops;
mod prefetch;
mod stack;
use alu::binary::LogicalOp;
use alu::unary::UnaryOp;
pub use flags::SrFlag;

use crate::core::{
    Bus16, BusMaster, BusSignals, bus::InterruptState, component::BusMasterComponent,
};
use crate::cpu::{
    Cpu, CpuControl,
    state::{CpuStateTrait, M68000State},
};
use crate::prelude::Saveable;

/// Which member of the 68000 family this CPU instance models.
///
/// Only `M68000` has behavior today; the enum exists so variant-dependent
/// logic (address-bus width, exception frame formats, brief-extension-word
/// scaling) can be gated in one place as later variants are added.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Saveable)]
pub enum M68kVariant {
    M68000 = 0,
    M68010 = 1,
    M68020 = 2,
    M68030 = 3,
}

/// Execution state machine for multi-cycle instructions.
#[derive(Clone, Debug)]
pub(crate) enum ExecState {
    /// Ready to fetch the next instruction.
    Fetch,
    /// Executing an instruction: (remaining_cycles). The instruction has
    /// already been decoded and its effect applied on the first cycle;
    /// remaining cycles are bus-idle wait states.
    Execute(u32),
    /// Burning an addressing mode's arithmetic before the instruction's first
    /// bus cycle. See [`format::leading_internal`].
    Lead(u32),
    /// Waiting out the refill the loader issued behind the opcode, before the
    /// instruction itself runs.
    LoadWait(u32),
    /// Waiting to issue the instruction's trailing prefetch.
    ///
    /// The refill behind the last word an instruction consumed is not part of
    /// applying its effect: the part issues it after the operand cycles are
    /// done, and the recorded traces put it there. `delay` counts the clocks
    /// still to pass before the next one runs, `owed` is how many are left, and
    /// `tail` is the instruction's remaining length from the last one's clock.
    ///
    /// **Two are owed whenever the instruction discarded the queue**, and they
    /// are two bus cycles four clocks apart rather than one event: a taken
    /// branch fetches both words at its target, and driving them on the same
    /// clock is the difference between the whole branch family reading 0% on
    /// positions and reading correctly.
    TrailingRefill { delay: u32, owed: u32, tail: u32 },
    /// STOP instruction executed, waiting for an interrupt.
    Stopped,
    /// Halted by a double bus fault or external HALT; only reset recovers.
    Halted,
}

/// Fields are ordered to match the save-state serialization layout (version 1).
#[derive(Saveable)]
#[save_version(1)]
pub struct M68000 {
    /// Data registers D0-D7.
    pub d: [u32; 8],
    /// Address registers A0-A6; `a[7]` is the ACTIVE stack pointer
    /// (USP or SSP depending on the SR supervisor bit).
    pub a: [u32; 8],
    /// Inactive user stack pointer (valid while in supervisor mode).
    pub usp: u32,
    /// Inactive supervisor stack pointer (valid while in user mode).
    pub ssp: u32,
    /// Address of the instruction word the part is about to execute, which is
    /// also the address of `prefetch[0]`.
    ///
    /// Private, and deliberately: a control transfer must discard the prefetch
    /// queue, and the only way to guarantee that is for every write to go
    /// through [`Self::set_pc_flush`] (or the straight-line advance the queue
    /// itself makes). Inferring the flush from PC moving does not work, because
    /// a taken branch with a zero displacement lands where execution would have
    /// gone anyway and the part still flushes. Read it with [`Self::pc`].
    pc: u32,
    /// Status register: high byte = system byte (T, S, interrupt mask),
    /// low byte = condition code register (X N Z V C).
    pub sr: u16,
    pub variant: M68kVariant,

    // Internal state (serialized)
    /// Previous level-7 interrupt state for NMI edge detection.
    pub(crate) nmi_previous: bool,
    /// STOP instruction executed, waiting for interrupt.
    pub(crate) stopped: bool,
    /// Double bus fault or external halt; only reset recovers.
    pub(crate) halted: bool,

    // Execution temporaries — not saved, reset to defaults on load
    #[save_skip(default = ExecState::Fetch)]
    pub(crate) state: ExecState,
    /// Opcode word of the instruction currently executing.
    #[allow(dead_code)]
    #[save_skip(default)]
    pub(crate) opcode: u16,
    /// Address of that opcode word (PC before the fetch). Exception frames
    /// that point at the faulting instruction push this value.
    #[save_skip(default)]
    pub(crate) instr_pc: u32,
    /// The instruction prefetch queue: the word at `pc` and the word after it.
    ///
    /// Not serialized, and it does not need to be, for the reason the i8088's
    /// queue is not: a save state is taken at an instruction boundary, and a
    /// queue that comes back empty is refilled from `pc` before the next word
    /// is consumed. The words are the same words; the only difference is that
    /// their fetches happen after the load rather than before the save, which
    /// is the same thing a flush does. See [`prefetch`].
    #[save_skip(default)]
    pub(crate) prefetch: [u16; 2],
    /// How many of the two queue slots hold a fetched word.
    ///
    /// A hole only ever forms at the top, so this is a length rather than a
    /// pair of validity bits: slot 0 is the word at `pc` and slot 1 the word at
    /// `pc + 2`, and consuming shifts down.
    #[save_skip(default)]
    pub(crate) prefetch_len: u8,
    /// Bus transfers the instruction now executing has performed.
    ///
    /// Every transfer on this part is four clocks at immediate DTACK, so an
    /// instruction's time is four clocks per transfer plus whatever it spends
    /// away from the bus. Instructions timed through [`Self::finish_from_bus`]
    /// charge from this count rather than from a table total, which is what
    /// stops a right total from hiding wrong bus activity.
    #[save_skip(default)]
    pub(crate) transfers: u32,
    /// Words the instruction now executing has taken out of the queue,
    /// including its opcode.
    ///
    /// This exists to keep [`format::extension_words`] honest. That table says
    /// in advance what this counts afterwards, and the two must agree on every
    /// encoding or a per-clock loader built on the table fetches the wrong
    /// number of words. Counting here rather than asserting here is deliberate:
    /// the comparison belongs in the gate, where it runs over both corpora at
    /// full speed, instead of in the hot path where it would have to be
    /// compiled out to be affordable and would then check nothing.
    #[save_skip(default)]
    pub(crate) words_consumed: u32,
    /// Of those, how many were taken without refilling behind them, because
    /// the instruction was about to discard the queue.
    ///
    /// Keeps [`format::suppresses_refill`] honest for the same reason
    /// [`Self::words_consumed`] keeps the word count honest: a loader hoists
    /// exactly these fetches out of the instruction bodies, so the table and
    /// the bodies have to agree about which instructions make them.
    #[save_skip(default)]
    pub(crate) words_without_refill: u32,
    /// Clocks of address arithmetic burned before this instruction's first bus
    /// cycle, from [`format::leading_internal`].
    #[save_skip(default)]
    pub(crate) lead_burned: u32,
    /// The clock the instruction body ran on, counted from the instruction's
    /// first.
    ///
    /// Zero once, and no longer: the loader spends clocks in front of the body
    /// now, on address arithmetic and on the refill behind the opcode. The
    /// finish measures from here rather than from zero, which is what keeps an
    /// instruction's length the same while its transfers move.
    #[save_skip(default)]
    pub(crate) exec_clock: u32,
    /// Transfers already made when the body started, so the ones the body
    /// itself makes can be counted apart from the loader's.
    #[save_skip(default)]
    pub(crate) pre_exec_transfers: u32,
}

impl Default for M68000 {
    fn default() -> Self {
        Self::new()
    }
}

impl M68000 {
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            // Supervisor mode, interrupts masked (reset state)
            sr: 0x2700,
            variant: M68kVariant::M68000,
            nmi_previous: false,
            stopped: false,
            halted: false,
            state: ExecState::Fetch,
            opcode: 0,
            instr_pc: 0,
            prefetch: [0; 2],
            prefetch_len: 0,
            transfers: 0,
            words_consumed: 0,
            words_without_refill: 0,
            lead_burned: 0,
            exec_clock: 0,
            pre_exec_transfers: 0,
        }
    }

    /// Of the words the instruction that just retired consumed, how many it
    /// took without refilling behind them.
    pub fn words_without_refill(&self) -> u32 {
        self.words_without_refill
    }

    /// Words the instruction that just retired took out of the prefetch queue,
    /// including its opcode. One more than its extension-word count.
    ///
    /// Exposed for the per-cycle gate, which checks it against what
    /// [`format::extension_words`] predicted from the opcode alone.
    pub fn words_consumed(&self) -> u32 {
        self.words_consumed
    }

    /// Returns true when the CPU is at an instruction boundary (ready to fetch).
    pub fn at_instruction_boundary(&self) -> bool {
        matches!(self.state, ExecState::Fetch)
    }

    /// The address of the instruction word about to be executed.
    #[inline]
    pub fn pc(&self) -> u32 {
        self.pc
    }

    /// Load a new PC and discard the prefetch queue.
    ///
    /// This is what a control transfer does, and it is the only way to move PC
    /// other than consuming a word out of the queue. Callers inside the core
    /// go through [`Self::set_pc_checked`], which faults on an odd target
    /// first; this is the raw form, for reset and for a test or harness
    /// planting a starting address.
    #[inline]
    pub fn set_pc_flush(&mut self, target: u32) {
        self.pc = target;
        self.flush_prefetch();
    }

    /// The two words currently in the prefetch queue, and how many are live.
    ///
    /// Exposed for the per-cycle gate, which seeds the queue from a vector's
    /// recorded `prefetch` pair and compares ours against the recorded final
    /// pair afterwards.
    pub fn prefetch_queue(&self) -> ([u16; 2], u8) {
        (self.prefetch, self.prefetch_len)
    }

    /// Seed the queue with two already-fetched words, as if the part had
    /// prefetched them from `pc` and `pc + 2`.
    pub fn load_prefetch_queue(&mut self, words: [u16; 2]) {
        self.prefetch = words;
        self.prefetch_len = 2;
    }

    /// Mask an effective address to the physical address-bus width.
    /// The 68000/68010 drive 24 address lines; 68020+ drive all 32.
    #[inline]
    pub(crate) fn mask_addr(&self, addr: u32) -> u32 {
        match self.variant {
            M68kVariant::M68000 | M68kVariant::M68010 => addr & 0x00FF_FFFF,
            M68kVariant::M68020 | M68kVariant::M68030 => addr,
        }
    }

    /// The cycle descriptor for an instruction-stream transfer.
    ///
    /// Program space, at whatever privilege the part is running at *now*: the
    /// privilege is read here rather than once per instruction because
    /// exception entry and `RTE` move it mid-instruction, and the cycles either
    /// side of the move name different address spaces.
    #[inline]
    pub(crate) fn program_cycle(&self, is_write: bool) -> BusSignals {
        BusSignals {
            is_write,
            program: true,
            supervisor: self.flag_is_set(SrFlag::S),
            byte: false,
        }
    }

    /// The cycle descriptor for an operand transfer. See
    /// [`Self::program_cycle`] for why privilege is sampled per transfer.
    #[inline]
    pub(crate) fn data_cycle(&self, is_write: bool, byte: bool) -> BusSignals {
        BusSignals {
            is_write,
            program: false,
            supervisor: self.flag_is_set(SrFlag::S),
            byte,
        }
    }

    /// Whether this variant is a 68010 or later. Gates the handful of
    /// post-68000 behaviors this core models (the longer exception frame,
    /// `MOVE from SR` becoming privileged).
    #[inline]
    pub(crate) fn is_68010_plus(&self) -> bool {
        !matches!(self.variant, M68kVariant::M68000)
    }

    /// Whether this variant stacks the 68010+ four-word exception frame: a
    /// format/vector-offset word above the 68000 SR+PC short frame. The
    /// 68000 has no such word; RTE on the 68010+ pops and discards it.
    #[inline]
    pub(crate) fn uses_long_exception_frame(&self) -> bool {
        self.is_68010_plus()
    }

    /// Execute one bus cycle.
    pub fn execute_cycle<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        match self.state {
            ExecState::Fetch => {
                if self.halted {
                    self.state = ExecState::Halted;
                    return;
                }
                if self.stopped {
                    self.state = ExecState::Stopped;
                    return;
                }
                // Cleared before the interrupt check, not after it: an
                // interrupt is charged from the transfers its own entry
                // sequence makes, and would otherwise inherit the count of
                // whatever instruction happened to run before it.
                self.transfers = 0;
                self.words_consumed = 0;
                self.words_without_refill = 0;
                self.lead_burned = 0;

                // Sample interrupts at the instruction boundary.
                let ints = bus.check_interrupts(master);
                if let Some(level) = self.pending_interrupt(ints) {
                    self.enter_interrupt(bus, master, level, ints.irq_vector);
                    return;
                }

                self.instr_pc = self.pc;
                if self.pc & 1 != 0 {
                    // Defensive: control transfers fault before loading an
                    // odd PC, but external state (a bad reset vector, a
                    // debugger) can still plant one.
                    let fault = addressing::AddressError {
                        addr: self.pc,
                        write: false,
                        program: true,
                        stacked_pc: self.pc,
                    };
                    self.enter_address_error(bus, master, fault);
                    return;
                }
                // Take the opcode out of the prefetch queue and execute the
                // instruction atomically; an odd word/long access aborts the
                // instruction at the fault, exactly like hardware.
                //
                // In steady state the queue is already full here and this
                // costs no bus cycle at all, which is why the recorded traces
                // contain no fetch of the instruction's own opcode.
                // The part spends an addressing mode's arithmetic before the
                // bus cycle that mode causes, so where that is the first cycle
                // of the instruction it is burned here, ahead of everything.
                // The queue already holds the opcode, so peeking costs nothing.
                self.fill_prefetch(bus, master);
                let lead = u32::from(format::leading_internal(self.prefetch[0]));
                if lead > 0 {
                    self.lead_burned = lead;
                    self.state = ExecState::Lead(lead - 1);
                    return;
                }
                self.begin_instruction(bus, master, 0);
            }
            ExecState::Lead(remaining) => {
                if remaining > 0 {
                    self.state = ExecState::Lead(remaining - 1);
                } else {
                    self.begin_instruction(bus, master, self.lead_burned);
                }
            }
            ExecState::LoadWait(remaining) => {
                if remaining > 0 {
                    self.state = ExecState::LoadWait(remaining - 1);
                } else {
                    self.run_instruction(bus, master);
                }
            }
            ExecState::Execute(remaining) => {
                if remaining <= 1 {
                    self.state = ExecState::Fetch;
                } else {
                    self.state = ExecState::Execute(remaining - 1);
                }
            }
            ExecState::TrailingRefill { delay, owed, tail } => {
                let remaining = delay - 1;
                if remaining == 0 {
                    // This clock is the one the part drives a refill on.
                    self.drive_refill(bus, master, owed, tail);
                } else {
                    self.state = ExecState::TrailingRefill {
                        delay: remaining,
                        owed,
                        tail,
                    };
                }
            }
            ExecState::Stopped => {
                // STOP: only an interrupt (or external reset) resumes.
                let ints = bus.check_interrupts(master);
                if let Some(level) = self.pending_interrupt(ints) {
                    self.enter_interrupt(bus, master, level, ints.irq_vector);
                }
            }
            ExecState::Halted => {
                // Only an external reset recovers from a halt.
            }
        }
    }

    /// Take the opcode and, where the instruction has a word behind it, issue
    /// that word's refill on a clock of its own before the body runs.
    ///
    /// This is the loader. It exists because the refill behind the opcode is a
    /// bus cycle the part drives *before* it does anything with the operand,
    /// and a body that runs atomically cannot put it there by itself.
    ///
    /// **It needs no staging buffer, and that is the whole reason it is this
    /// small.** Taking a word out of the queue fills the hole first and pops
    /// second, so a body that finds the queue already full issues nothing and
    /// simply takes the word the loader fetched for it. Only the second and
    /// later extension words are still fetched by the body, and they are the
    /// ones a staging buffer would be for.
    ///
    /// `base` is the clock the loader starts on, after any leading arithmetic.
    fn begin_instruction<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster, base: u32) {
        let opcode = self.take_opcode();
        self.opcode = opcode;

        // An instruction about to discard the queue does not refill behind the
        // words it consumes, and a privileged instruction in user mode consumes
        // none at all: the part settles privilege before its first prefetch.
        let words = if format::privileged(opcode) && !self.flag_is_set(SrFlag::S) {
            0
        } else {
            format::words_before_operand(opcode)
        };
        if words == 0 || format::suppresses_refill(opcode) {
            self.exec_clock = base;
            self.run_instruction(bus, master);
            return;
        }

        self.refill_prefetch(bus, master);
        self.exec_clock = base + 4;
        self.state = ExecState::LoadWait(3);
    }

    /// Run the instruction body, whose opcode the loader has already taken.
    fn run_instruction<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        self.pre_exec_transfers = self.transfers;
        let opcode = self.opcode;
        if let Err(fault) = self.execute_instruction(opcode, bus, master) {
            self.enter_address_error(bus, master, fault);
        }
    }

    /// Complete an instruction that took `total_cycles` clock cycles: the
    /// current tick already counts as one, and any remainder is burned as
    /// bus-idle wait states.
    pub(crate) fn finish(&mut self, total_cycles: u32) {
        self.state = if total_cycles <= 1 {
            ExecState::Fetch
        } else {
            ExecState::Execute(total_cycles - 1)
        };
    }

    /// Complete an instruction, refilling the prefetch queue and charging four
    /// clocks for every bus transfer it performed plus `internal` clocks away
    /// from the bus.
    ///
    /// **The refill is part of finishing.** An instruction leaves the queue one
    /// word short for every word it consumed, and the part fills it back up
    /// before the next instruction runs, so those fetches belong to this
    /// instruction and are charged to it. That is the whole reason a taken
    /// branch costs more than the instruction it branches over: the two words
    /// at the target are fetched here.
    ///
    /// Placing the refill last is the default rather than the universal rule.
    /// `MOVE` records its refill after its write, which is what this does; the
    /// read-modify-write families record it before, and say so by calling
    /// [`Self::refill_prefetch`] themselves before the write. Once they have,
    /// the fill here has nothing left to do.
    ///
    /// This is the same arithmetic the part does, and it is what the recorded
    /// traces show: their entries tile each instruction's length, every
    /// transfer is four clocks at immediate DTACK, and no case in either
    /// corpus has a length shorter than four clocks per transfer.
    ///
    /// The point of charging this way rather than from a documented total is
    /// that the total can be right while the bus activity underneath it is
    /// wrong, which is two errors agreeing to look like none. Here a wrong
    /// transfer count cannot hide: it moves the clock count with it.
    pub(crate) fn finish_from_bus<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        internal: u32,
    ) {
        // The instruction's whole length: four clocks for every transfer it
        // makes, the refill it still owes included, plus its time off the bus.
        // This is unchanged by anything the loader does, which is the point:
        // the loader moves transfers around inside the instruction, it does not
        // make the instruction longer or shorter.
        self.finish_from_bus_inner(bus, master, internal, false);
    }

    /// Complete an instruction whose internal time runs *before* its trailing
    /// refill rather than after it.
    ///
    /// The distinction is mechanical, not a per-family quirk to be tabulated:
    /// a branch cannot fetch at its target until it has finished working out
    /// the target, so its address arithmetic precedes both fetches. An `ADD.l`
    /// with a register operand runs its prefetch first and its two ALU passes
    /// after, because those passes have nothing to do with where the next fetch
    /// goes. **Internal time that computes the next fetch address precedes the
    /// fetch; arithmetic on data follows it.**
    ///
    /// Taken from the part's own sequence rather than fitted. A branch with a
    /// byte displacement computes its target and then goes one of two ways: two
    /// clocks and two fetches when taken, or two clocks, two more, and a single
    /// fetch when not. Ten and eight, with the idle first in both.
    pub(crate) fn finish_from_bus_address_first<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        internal: u32,
    ) {
        self.finish_from_bus_inner(bus, master, internal, true);
    }

    fn finish_from_bus_inner<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        internal: u32,
        internal_first: bool,
    ) {
        let owed = u32::from(2 - self.prefetch_len);
        let full = 4 * (self.transfers + owed) + internal;

        // Everything is measured from the clock the body ran on, because the
        // loader has already spent clocks in front of it. Saturating rather
        // than asserting: a wrong row in `leading_internal` would otherwise
        // underflow here, and the symptom should be a rung-3 miss on that row
        // rather than a panic on a board.
        let from_body = full.saturating_sub(self.exec_clock);

        // Nothing owed: a read-modify-write family has already refilled at its
        // own declared point, so there is no trailing fetch to place.
        if owed == 0 {
            self.finish(from_body);
            return;
        }

        // The transfers the *body* made occupy the clocks after it started,
        // four each, so the refill belongs on the clock after the last of them.
        // They all still happen on one clock, which is what makes them wrong
        // and this right: their count is correct even where their positions are
        // not, so the refill lands correctly now and stays correct when they
        // are spread out later.
        let mut delay = 4 * (self.transfers - self.pre_exec_transfers);
        if internal_first {
            // Whatever of the internal time was not already burned up front as
            // address arithmetic runs here, ahead of the fetches it computes.
            delay += internal.saturating_sub(self.lead_burned);
        }
        // Each owed refill is its own bus cycle four clocks after the last, so
        // `tail` is measured from the clock the *final* one runs on.
        let tail = from_body
            .saturating_sub(delay)
            .saturating_sub(4 * (owed - 1));
        if delay == 0 {
            self.drive_refill(bus, master, owed, tail);
            return;
        }
        self.state = ExecState::TrailingRefill { delay, owed, tail };
    }

    /// Drive one owed refill on this clock, and either queue the next or end
    /// the instruction.
    fn drive_refill<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        owed: u32,
        tail: u32,
    ) {
        self.refill_prefetch(bus, master);
        match owed - 1 {
            0 => self.finish(tail),
            left => {
                self.state = ExecState::TrailingRefill {
                    delay: 4,
                    owed: left,
                    tail,
                }
            }
        }
    }

    /// Complete an instruction that leaves the prefetch queue as it found it.
    ///
    /// `STOP` is the only user and the recorded trace is why: a supervisor
    /// `STOP` is four clocks with no bus cycle at all, and its recorded final
    /// state has the queue and PC exactly where they started. The part stops
    /// before issuing the refill, so charging one here would put `STOP` at
    /// eight clocks and fetch a word nothing executes.
    pub(crate) fn finish_without_refill(&mut self, internal: u32) {
        self.finish(4 * self.transfers + internal);
    }

    /// Decode and execute one instruction, leaving `self.state` either back
    /// at `Fetch` or in `Execute(n)` to burn the remaining documented cycles.
    ///
    /// Dispatch is two-level: first on the opcode "line" (top 4 bits), then
    /// on the line-specific sub-encoding. Instruction families are wired in
    /// here as they are implemented.
    /// Returns the [`addressing::AddressError`] of the access that aborted
    /// the instruction, if any; the caller enters the vector-3 exception.
    fn execute_instruction<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> addressing::AccessResult<()> {
        let opmode = (opcode >> 6) & 7;
        let ea_mode = (opcode >> 3) & 7;
        match (opcode >> 12) & 0xF {
            // ANDI/ORI/EORI to CCR (byte forms) and to SR (word forms,
            // privileged)
            0x0 if matches!(opcode, 0x003C | 0x007C | 0x023C | 0x027C | 0x0A3C | 0x0A7C) => {
                self.op_sr_imm(opcode, bus, master)
            }
            // Dynamic bit ops (bit number in Dn); EA mode 001 there
            // encodes MOVEP
            0x0 if opcode & 0x0100 != 0 => {
                if ea_mode == 1 {
                    self.op_movep(opcode, bus, master)
                } else {
                    self.op_bitop(opcode, bus, master, true)
                }
            }
            // Static bit ops (bit number in an extension word)
            0x0 if opcode & 0x0F00 == 0x0800 => self.op_bitop(opcode, bus, master, false),
            // ORI/ANDI/SUBI/ADDI/EORI/CMPI
            0x0 => {
                if !self.op_imm_alu(opcode, bus, master)? {
                    self.finish_from_bus(bus, master, 0);
                }
                Ok(())
            }
            // MOVE.b / MOVE.l / MOVE.w (and MOVEA for An destinations)
            0x1..=0x3 => self.op_move(opcode, bus, master),
            // CHK (line 0x4, bits 8-6 = 110) and LEA (111) share the line
            0x4 if opcode & 0x01C0 == 0x0180 => self.op_chk(opcode, bus, master),
            0x4 if opcode & 0x01C0 == 0x01C0 => self.op_lea_pea(opcode, bus, master, false),
            // Line 0x4 "misc": the unary ALU group, the SR/CCR moves (the
            // size-11 encodings of the unary sub-ops), JMP/JSR/RTS/RTR,
            // MOVEM, and the privileged one-words all live here.
            0x4 => match (opcode >> 8) & 0xF {
                0x0 if opcode & 0x00C0 == 0x00C0 => self.op_move_from_sr(opcode, bus, master),
                0x0 => self.op_unary(opcode, bus, master, UnaryOp::Negx),
                // 0x42C0 (MOVE from CCR) is 68010+; on the 68000 the CLR
                // size-11 hole stays a bounded NOP via op_unary
                0x2 => self.op_unary(opcode, bus, master, UnaryOp::Clr),
                0x4 if opcode & 0x00C0 == 0x00C0 => self.op_move_to_ccr(opcode, bus, master),
                0x4 => self.op_unary(opcode, bus, master, UnaryOp::Neg),
                0x6 if opcode & 0x00C0 == 0x00C0 => self.op_move_to_sr(opcode, bus, master),
                0x6 => self.op_unary(opcode, bus, master, UnaryOp::Not),
                // NBCD (size bits 00); SWAP and PEA (M4) share sub-op 0x8
                0x8 if opcode & 0x00C0 == 0 => self.op_nbcd(opcode, bus, master),
                // SWAP Dn (PEA takes the other EA modes of this encoding)
                0x8 if opcode & 0x00F8 == 0x0040 => self.op_swap(opcode, bus, master),
                0x8 if opcode & 0x00C0 == 0x0040 => self.op_lea_pea(opcode, bus, master, true),
                // EXT.w / EXT.l (EA mode bits 000); other modes with bit 7
                // set are the MOVEM store direction
                0x8 if opcode & 0x0038 == 0 && opcode & 0x0080 != 0 => {
                    self.op_ext(opcode, bus, master)
                }
                0x8 if opcode & 0x0080 != 0 => self.op_movem(opcode, bus, master, false),
                // ILLEGAL is the one architecturally-guaranteed illegal
                // encoding (a TAS hole); the other size-11 encodings of
                // sub-op 0xA are TAS, the rest TST
                0xA if opcode == 0x4AFC => self.op_illegal(bus, master, 4),
                0xA if opcode & 0x00C0 == 0x00C0 => self.op_tas(opcode, bus, master),
                0xA => self.op_tst(opcode, bus, master),
                // MOVEM load direction (bit 7 clear is unassigned here)
                0xC if opcode & 0x0080 != 0 => self.op_movem(opcode, bus, master, true),
                // 0x4E40-0x4EFF: JMP/JSR plus the one-word specials. NOP,
                // TRAP, LINK/UNLK, MOVE USP, RESET, STOP, RTE, and TRAPV
                // stay bounded NOPs until M4/M5.
                0xE => match (opcode >> 6) & 3 {
                    3 => self.op_jmp_jsr(opcode, bus, master, false),
                    2 => self.op_jmp_jsr(opcode, bus, master, true),
                    1 => match opcode {
                        0x4E40..=0x4E4F => self.op_trap(opcode, bus, master),
                        0x4E50..=0x4E57 => self.op_link(opcode, bus, master),
                        0x4E58..=0x4E5F => self.op_unlk(opcode, bus, master),
                        0x4E60..=0x4E6F => self.op_move_usp(opcode, bus, master),
                        0x4E70 => self.op_reset_instruction(bus, master),
                        0x4E71 => self.op_nop(bus, master), // NOP
                        0x4E72 => self.op_stop(bus, master),
                        0x4E73 => self.op_rte(bus, master),
                        0x4E75 => self.op_rts(bus, master),
                        0x4E76 => self.op_trapv(bus, master),
                        0x4E77 => self.op_rtr(bus, master),
                        _ => self.op_nop(bus, master),
                    },
                    _ => self.op_nop(bus, master),
                },
                _ => self.op_nop(bus, master),
            },
            // Size bits 11 on line 0x5 split into DBcc (EA mode 001 = An)
            // and Scc (everything else); the other sizes are ADDQ/SUBQ
            0x5 if opmode & 3 == 3 && ea_mode == 1 => self.op_dbcc(opcode, bus, master),
            0x5 if opmode & 3 == 3 => self.op_scc(opcode, bus, master),
            0x5 => self.op_addq_subq(opcode, bus, master),
            // BRA / BSR / Bcc
            0x6 => self.op_bcc(opcode, bus, master),
            // MOVEQ (bit 8 set is unassigned on the 68000)
            0x7 if opcode & 0x0100 == 0 => self.op_moveq(opcode, bus, master),
            // OR / DIVU / DIVS / SBCD plus the illegal PACK/UNPK slots
            0x8 => match opmode {
                3 => self.op_div(opcode, bus, master, false),
                7 => self.op_div(opcode, bus, master, true),
                4 if ea_mode < 2 => self.op_bcd(opcode, bus, master, false),
                5 | 6 if ea_mode < 2 => self.op_nop(bus, master), // illegal (PACK/UNPK on 68020+)
                _ => self.op_logical(opcode, bus, master, LogicalOp::Or),
            },
            // SUB / SUBA / SUBX
            0x9 => match opmode {
                4..=6 if ea_mode < 2 => self.op_addx_subx(opcode, bus, master, false),
                _ => self.op_add_sub(opcode, bus, master, false),
            },
            // CMP / CMPA / CMPM / EOR
            0xB => match opmode {
                4..=6 if ea_mode == 1 => self.op_cmpm(opcode, bus, master),
                4..=6 => self.op_logical(opcode, bus, master, LogicalOp::Eor),
                _ => self.op_cmp(opcode, bus, master),
            },
            // AND / MULU / MULS / ABCD; the line also carries EXG (M4)
            0xC => match opmode {
                3 => self.op_mul(opcode, bus, master, false),
                7 => self.op_mul(opcode, bus, master, true),
                4 if ea_mode < 2 => self.op_bcd(opcode, bus, master, true),
                5 | 6 if ea_mode < 2 => self.op_exg(opcode, bus, master),
                _ => self.op_logical(opcode, bus, master, LogicalOp::And),
            },
            // ADD / ADDA / ADDX
            0xD => match opmode {
                4..=6 if ea_mode < 2 => self.op_addx_subx(opcode, bus, master, true),
                _ => self.op_add_sub(opcode, bus, master, true),
            },
            // Shifts and rotates. Size bits 11 select the one-bit memory
            // form (bit 11 set there is unassigned on the 68000); other
            // sizes are the register form.
            0xE if opmode & 3 == 3 => {
                if opcode & 0x0800 == 0 {
                    self.op_shift_mem(opcode, bus, master)
                } else {
                    self.op_nop(bus, master)
                }
            }
            0xE => self.op_shift_reg(opcode, bus, master),
            // Line-A and line-F opcodes are unassigned on the 68000 and
            // vector through their dedicated exceptions
            0xA => self.op_illegal(bus, master, 10),
            0xF => self.op_illegal(bus, master, 11),
            // Remaining unassigned encodings inside implemented lines stay
            // bounded NOPs
            _ => self.op_nop(bus, master),
        }
    }

    /// Bounded 4-cycle no-op: NOP itself and the unassigned encodings
    /// inside implemented lines.
    fn op_nop<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> addressing::AccessResult<()> {
        // Refilling the queue behind the opcode is the whole instruction.
        self.finish_from_bus(bus, master, 0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl<B: Bus16 + ?Sized> BusMasterComponent<B> for M68000 {
    type Address = u32;
    type Data = u16;

    fn tick_with_bus(&mut self, bus: &mut B, master: BusMaster) -> bool {
        self.execute_cycle(bus, master);
        matches!(self.state, ExecState::Fetch)
    }
}

impl<B: Bus16 + ?Sized> Cpu<B> for M68000 {
    fn reset(&mut self, bus: &mut B, master: BusMaster) {
        // Reset enters supervisor mode with trace off and interrupts masked,
        // then loads SSP from vector 0 and PC from vector 1.
        self.sr = 0x2700; // S=1, T=0, interrupt mask = 7
        self.stopped = false;
        self.halted = false;
        self.nmi_previous = false;
        self.state = ExecState::Fetch;

        self.a[7] = self
            .read_long_at(bus, master, 0x0000_0000)
            .expect("vector 0 is aligned");
        let entry = self
            .read_long_at(bus, master, 0x0000_0004)
            .expect("vector 1 is aligned");
        // Reset leaves the queue empty, so the first instruction pays for two
        // fetches to fill it, which is what the part does coming out of reset:
        // it reads the two vectors and then prefetches two instruction words
        // before executing anything.
        //
        // Those eight clocks are the whole reason Road Runner's golden frame
        // moved at M3. They shift the machine's phase against video timing once,
        // at boot, and its attract animation lands one step further along.
        // Filling the queue here instead, where the clocks would not be charged
        // to any instruction, restores the previous frame exactly. That is how
        // the mechanism was identified, and it is also a model of a part that
        // starts with a queue it never fetched.
        self.set_pc_flush(entry);
    }
}

impl CpuControl for M68000 {
    fn signal_interrupt(&mut self, _int: InterruptState) {
        // Interrupts are sampled from the bus at instruction boundaries
        // (execute_cycle), not pushed through this entry point.
    }

    fn is_sleeping(&self) -> bool {
        self.stopped || self.halted
    }
}

impl CpuStateTrait for M68000 {
    type Snapshot = M68000State;

    fn snapshot(&self) -> M68000State {
        M68000State {
            d: self.d,
            a: self.a,
            usp: self.usp,
            ssp: self.ssp,
            pc: self.pc,
            sr: self.sr,
        }
    }
}

// ---------------------------------------------------------------------------
// Debug support
// ---------------------------------------------------------------------------

use crate::core::debug::{DebugCpu, DebugRegister, Debuggable};

impl Debuggable for M68000 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

impl DebugCpu for M68000 {
    fn debug_pc(&self) -> u32 {
        self.pc
    }

    fn debug_at_instruction_boundary(&self) -> bool {
        self.at_instruction_boundary()
    }

    fn debug_disassemble(
        &self,
        addr: u32,
        bytes: &[u8],
    ) -> crate::cpu::disasm::DisassembledInstruction {
        disasm::disassemble(addr, bytes)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Minimal word bus shared by the m68000 unit tests: 64 KB of big-endian
/// byte memory served 16 bits at a time at even addresses.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::core::{Bus, Bus16, BusMaster, bus::InterruptState};

    pub(crate) struct WordBus {
        pub(crate) memory: Vec<u8>,
    }

    impl WordBus {
        pub(crate) fn new() -> Self {
            Self {
                memory: vec![0; 0x10000],
            }
        }

        /// Load bytes at a byte address (test setup helper).
        pub(crate) fn load(&mut self, addr: u32, data: &[u8]) {
            let start = addr as usize;
            self.memory[start..start + data.len()].copy_from_slice(data);
        }
    }

    impl Bus for WordBus {
        type Address = u32;
        type Data = u16;

        fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
            let i = (addr & 0xFFFE) as usize;
            u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
        }

        fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
            let i = (addr & 0xFFFE) as usize;
            self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
        }

        fn is_halted_for(&self, _master: BusMaster) -> bool {
            false
        }

        fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
            InterruptState::default()
        }
    }

    /// Flat RAM: a byte transfer touches its own byte and nothing else.
    impl Bus16 for WordBus {
        fn read_byte(&mut self, _master: BusMaster, addr: u32) -> u8 {
            self.memory[(addr & 0xFFFF) as usize]
        }

        fn write_byte(&mut self, _master: BusMaster, addr: u32, data: u8) {
            self.memory[(addr & 0xFFFF) as usize] = data;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::WordBus;
    use super::*;
    use crate::core::Bus;

    /// A bus that keeps what the part drove on FC2..FC0 with each address.
    struct FcBus {
        inner: WordBus,
        seen: Vec<(u32, u8)>,
    }

    impl Bus for FcBus {
        type Address = u32;
        type Data = u16;
        fn read(&mut self, m: BusMaster, addr: u32) -> u16 {
            self.inner.read(m, addr)
        }
        fn write(&mut self, m: BusMaster, addr: u32, data: u16) {
            self.inner.write(m, addr, data);
        }
        fn is_halted_for(&self, _m: BusMaster) -> bool {
            false
        }
        fn check_interrupts(&mut self, _t: BusMaster) -> InterruptState {
            InterruptState::default()
        }
        fn observe_bus_cycle(&mut self, _m: BusMaster, addr: u32, signals: BusSignals) {
            self.seen.push((addr, signals.function_code()));
        }
    }

    impl Bus16 for FcBus {
        fn read_byte(&mut self, m: BusMaster, addr: u32) -> u8 {
            self.inner.read_byte(m, addr)
        }
        fn write_byte(&mut self, m: BusMaster, addr: u32, data: u8) {
            self.inner.write_byte(m, addr, data);
        }
    }

    /// An instruction word is fetched from program space and an operand from
    /// data space, at whatever privilege the part is running at.
    ///
    /// The 68000 drives these on three pins beside the address, and nothing
    /// downstream can reconstruct them: two cycles at the same address with the
    /// same direction name different address spaces depending only on which
    /// unit inside the part asked for them.
    #[test]
    fn transfers_name_program_and_data_space() {
        // MOVE.w (A0), D0: one operand read, then the refill behind the opcode.
        for (supervisor, data_fc, program_fc) in [(true, 5, 6), (false, 1, 2)] {
            let mut cpu = M68000::new();
            let mut bus = FcBus {
                inner: WordBus::new(),
                seen: Vec::new(),
            };
            bus.inner.load(0x1000, &[0x30, 0x10, 0x4E, 0x71]);
            cpu.set_flag(SrFlag::S, supervisor);
            cpu.a[0] = 0x2000;
            cpu.set_pc_flush(0x1000);

            while !cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {}

            assert_eq!(
                bus.seen,
                vec![
                    // Filling the empty queue: two program fetches.
                    (0x1000, program_fc),
                    (0x1002, program_fc),
                    // The operand, in data space.
                    (0x2000, data_fc),
                    // The refill behind the consumed opcode.
                    (0x1004, program_fc),
                ],
                "supervisor = {supervisor}"
            );
        }
    }

    #[test]
    fn new_state() {
        let cpu = M68000::new();
        assert_eq!(cpu.sr, 0x2700);
        assert_eq!(cpu.pc, 0);
        assert_eq!(cpu.d, [0; 8]);
        assert_eq!(cpu.a, [0; 8]);
        assert_eq!(cpu.variant, M68kVariant::M68000);
        assert!(cpu.at_instruction_boundary());
        assert!(!cpu.is_sleeping());
    }

    #[test]
    fn reset_loads_ssp_and_pc_vectors() {
        let mut cpu = M68000::new();
        let mut bus = WordBus::new();
        // Vector 0 (SSP) = $00012000, vector 1 (PC) = $00000400
        bus.memory[0..8].copy_from_slice(&[0x00, 0x01, 0x20, 0x00, 0x00, 0x00, 0x04, 0x00]);

        cpu.reset(&mut bus, BusMaster::Cpu(0));

        assert_eq!(cpu.a[7], 0x0001_2000);
        assert_eq!(cpu.pc, 0x0000_0400);
        assert_eq!(cpu.sr, 0x2700);
        assert!(cpu.at_instruction_boundary());
    }

    #[test]
    fn mask_addr_is_24_bit_on_68000() {
        let cpu = M68000::new();
        assert_eq!(cpu.mask_addr(0xFF12_3456), 0x0012_3456);
        let mut cpu20 = M68000::new();
        cpu20.variant = M68kVariant::M68020;
        assert_eq!(cpu20.mask_addr(0xFF12_3456), 0xFF12_3456);
    }

    #[test]
    fn unimplemented_opcode_burns_cycles_to_boundary() {
        let mut cpu = M68000::new();
        let mut bus = WordBus::new();
        cpu.pc = 0x1000;
        // 0x4E7A is the 68010+ MOVEC slot — a permanent hole on the 68000
        // that executes as a bounded NOP.
        bus.load(0x1000, &[0x4E, 0x7A]);

        // First tick fetches and "executes"; instruction must complete in a
        // bounded number of cycles and advance PC by one word.
        let mut ticks = 0;
        while !cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            ticks += 1;
            assert!(ticks < 100, "instruction never reached a boundary");
        }
        assert_eq!(cpu.pc, 0x1002);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut cpu = M68000::new();
        cpu.d[0] = 0x1234_5678;
        cpu.a[6] = 0xDEAD_BEEF;
        cpu.usp = 0x0000_8000;
        cpu.ssp = 0x0000_9000;
        cpu.pc = 0x0040_0000;
        cpu.sr = 0x2704;
        let snap = cpu.snapshot();
        assert_eq!(snap.d[0], 0x1234_5678);
        assert_eq!(snap.a[6], 0xDEAD_BEEF);
        assert_eq!(snap.usp, 0x0000_8000);
        assert_eq!(snap.ssp, 0x0000_9000);
        assert_eq!(snap.pc, 0x0040_0000);
        assert_eq!(snap.sr, 0x2704);
    }

    #[test]
    fn is_sleeping_when_stopped_or_halted() {
        let mut cpu = M68000::new();
        assert!(!cpu.is_sleeping());
        cpu.stopped = true;
        assert!(cpu.is_sleeping());
        cpu.stopped = false;
        cpu.halted = true;
        assert!(cpu.is_sleeping());
    }
}
