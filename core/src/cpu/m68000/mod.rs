//! Motorola 68000 CPU emulation.
//!
//! The 68000 is a 16-bit-data / 32-bit-register big-endian CPU with eight
//! data registers, eight address registers (A7 doubles as the active stack
//! pointer), supervisor/user privilege modes, and a 256-entry vectored
//! exception table. The external data bus is 16 bits wide: the bus interface
//! uses `Address = u32` (24-bit physical address space on the 68000) and
//! `Data = u16` (one bus transaction = one word at an even address).
//!
//! **Execution is no longer atomic, and where each bus cycle lands is part of
//! the model.** Byte accesses are strobed, every transfer costs four clocks,
//! instruction words come out of a real two-word prefetch queue
//! ([`prefetch`]), and three things put the transfers on the clocks the part
//! puts them on: a loader that takes the opcode and drives the refills around
//! the body, a bus unit that runs the cycles nothing is waiting on, and a body
//! that suspends and runs again to reach an operand read. See
//! [`M68000::run_body`], [`PendingCycle`] and `docs/designs/cycle-accurate-m68000.md`.
//!
//! What is still charged rather than placed is the data-dependent internal time
//! of `MULx` and `DIVx`, and `MOVEM`'s cycles past the replay cap.

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

/// A bus cycle the execution unit has handed over to be run on a later clock.
///
/// Only cycles nothing in the instruction is waiting on can be handed over: a
/// write, whose value the part has already decided, and a queue refill, which
/// no instruction reads back within itself. An operand *read* cannot, because
/// the body is about to use what it returns, which is the whole reason the
/// remaining rung-3 residual is shaped the way it is.
/// The function code travels with the cycle, latched when it was handed over
/// rather than read when it runs. The part drives FC2..FC0 with the address, so
/// it is decided by whatever issued the cycle; asking the status register later
/// asks a question whose answer may have moved, and it does: a `BSR` that
/// pushes in user mode and then faults would otherwise report its push at
/// supervisor privilege, because the exception got there first.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PendingCycle {
    /// Fill the next hole in the prefetch queue. The address is deliberately
    /// *not* latched: it is wherever the queue's hole is when the cycle runs.
    Refill { signals: BusSignals },
    /// Drive `data` at `addr`, as a byte behind one strobe or a full word.
    Write {
        addr: u32,
        data: u16,
        byte: bool,
        signals: BusSignals,
    },
    /// Microcode steps that drive nothing, holding the bus idle for `clocks`
    /// before the next cycle on the list.
    ///
    /// The list is the part's own sequence handed to the bus unit, and the
    /// part's sequence has steps in it that make no access. Without them every
    /// gap in a recorded trace would have to be a gap at the front or the back,
    /// and exception entry has one in the middle: its two fetches at the
    /// handler are six clocks apart, not four, because a step sits between
    /// them.
    Idle { clocks: u32 },
}

impl PendingCycle {
    /// How long this entry occupies the bus unit. Every transfer is four clocks
    /// at immediate DTACK; an idle step is however long its microcode steps
    /// are.
    fn clocks(self) -> u32 {
        match self {
            PendingCycle::Idle { clocks } => clocks,
            _ => 4,
        }
    }
}

/// Cycles one instruction can have outstanding at once.
///
/// Exception entry is the longest: an aborted instruction's own outstanding
/// writes, then the idle step in front of the frame, then seven frame words.
const MAX_PENDING: usize = 12;

/// A bus cycle a body has already run, kept so that running the body again
/// gives it back rather than asking the bus a second time.
///
/// See [`M68000::run_body`] for why a body runs more than once, and
/// [`M68000::must_suspend`] for what stops it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ReplayedCycle {
    /// A word read, and what the bus returned.
    Read(u16),
    /// A byte read.
    ReadByte(u8),
    /// A queue refill, and the word it fetched. Replaying it puts the word
    /// back into the hole: the queue is unwound with the rest of the body's
    /// state, and the fetch behind it must not happen twice.
    Refill(u16),
    /// A write handed to the bus unit. It may already have been driven, so
    /// replaying must neither queue it again nor drive it, and there is
    /// nothing to give back.
    Handed,
}

/// Cycles of one body that can be given back to a later run of it.
///
/// Sixteen covers every instruction outside `MOVEM`, whose long form moves up
/// to thirty-two words. Past the limit the body keeps running and its cycles
/// keep happening; what stops is suspending, because a cycle that is not
/// recorded cannot be replayed and running it twice would be a second access
/// to the bus. `MOVEM` therefore gets clocks of its own for its first sixteen
/// words and the rest on one, which is where it already was, and it is M5's.
const MAX_REPLAY: usize = 16;

/// The body's state as the body first found it, so an attempt that suspends
/// can be unwound and run again from the top.
///
/// Everything an instruction body writes is here. What is deliberately *not*
/// here is what belongs to the bus rather than to the body: the transfer
/// count, the cycles handed to the bus unit, and the replay log itself. Those
/// record cycles that really happened, and unwinding the body does not unhappen
/// them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BodyState {
    d: [u32; 8],
    a: [u32; 8],
    usp: u32,
    ssp: u32,
    pc: u32,
    sr: u16,
    prefetch: [u16; 2],
    prefetch_len: u8,
    words_consumed: u32,
    words_without_refill: u32,
    internal_spent: u32,
    ea_program_space: bool,
    stopped: bool,
    halted: bool,
}

/// What [`M68000::run_body`] runs when it runs again.
///
/// **Exception entry is a body in its own right, and it has to be.** The
/// instruction that faulted is over, so there is nothing left to unwind it to;
/// without a body of its own the entry drove all eleven of its cycles on one
/// clock, which is the whole faulting side of the per-cycle gate's position
/// rung. Giving it a state capture and a replay log of its own costs nothing at
/// run time and makes it suspend exactly the way an instruction does.
#[derive(Clone, Copy, Debug)]
pub(crate) enum BodyKind {
    /// The decoded instruction.
    Instruction,
    /// Address-error entry, carrying the fault it is stacking a frame for.
    AddressError(addressing::AddressError),
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
    /// Waiting out the clocks of the bus cycles the body ran on its last
    /// attempt, before running it again. See [`M68000::run_body`].
    BodyWait(u32),
    /// Driving the cycles a suspended body handed over, one every four clocks,
    /// before running the body again.
    ///
    /// A body that has handed over writes and then wants to read cannot have
    /// the read first: the part drives them in the order it decided them. The
    /// cheap answer is to drive the whole list at once and let the read follow
    /// on the same clock, and that is what this core did; it is also why an
    /// exception frame's writes all landed together. The part gives each of
    /// them a clock, so the body waits for the list to drain and then runs
    /// again, reaching the read on a clock of its own.
    DrainPending { delay: u32 },
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
    ///
    /// The queue also holds whatever the body handed over rather than ran
    /// itself, so this is the bus unit working through a list, not a refill
    /// with a delay in front of it.
    TrailingRefill { delay: u32, tail: u32 },
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
    /// Clocks this instruction has spent away from the bus so far.
    ///
    /// **Only the abort path reads this, and only the abort path needs it.** An
    /// instruction that runs to its end declares its whole internal time at the
    /// finish, which is where the length comes from; one that address-errors
    /// never reaches its finish, and the clocks it had already spent are still
    /// spent. The part computes an address, drives the access, and only then
    /// finds it odd.
    ///
    /// It starts at whatever the loader burned in front of the instruction and
    /// grows as a body declares time it spends before an access that can fault.
    /// See [`Self::spend_internal`].
    #[save_skip(default)]
    pub(crate) internal_spent: u32,
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
    /// The effective address just resolved names program space, because it was
    /// formed from PC. Set by `decode_ea` and taken by `ea_read`.
    #[save_skip(default)]
    pub(crate) ea_program_space: bool,
    /// Cycles the body handed to the bus unit, in the order it handed them
    /// over, waiting for their clocks.
    #[save_skip(default = [PendingCycle::Refill {
        signals: BusSignals {
            is_write: false,
            program: true,
            supervisor: true,
            byte: false,
            rmw: false,
        },
    }; MAX_PENDING])]
    pub(crate) pending: [PendingCycle; MAX_PENDING],
    /// How many of `pending` are live, and how many have been driven.
    #[save_skip(default)]
    pub(crate) pending_len: u8,
    #[save_skip(default)]
    pub(crate) pending_pos: u8,
    /// The body's state before it first ran, restored whenever it suspends.
    #[save_skip(default)]
    pub(crate) body: BodyState,
    /// The bus cycles the body has run so far, in the order it ran them.
    #[save_skip(default = [ReplayedCycle::Handed; MAX_REPLAY])]
    pub(crate) replay: [ReplayedCycle; MAX_REPLAY],
    /// How many of `replay` are live, and how far the current attempt has
    /// walked through them.
    #[save_skip(default)]
    pub(crate) replay_len: u8,
    #[save_skip(default)]
    pub(crate) replay_pos: u8,
    /// Bus cycles driven on this clock. One is all the part can drive, so a
    /// body that wants a second one suspends instead.
    #[save_skip(default)]
    pub(crate) tick_cycles: u32,
    /// Whether a body is running, and can therefore be unwound.
    #[save_skip(default)]
    pub(crate) in_body: bool,
    /// Which body [`Self::run_body`] runs: the instruction, or the exception
    /// entry that replaced it when the instruction faulted.
    #[save_skip(default = BodyKind::Instruction)]
    pub(crate) body_kind: BodyKind,
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
            internal_spent: 0,
            exec_clock: 0,
            pre_exec_transfers: 0,
            ea_program_space: false,
            pending: [PendingCycle::Refill {
                signals: BusSignals { is_write: false, program: true, supervisor: true, byte: false, rmw: false },
            }; MAX_PENDING],
            pending_len: 0,
            pending_pos: 0,
            body: BodyState::default(),
            replay: [ReplayedCycle::Handed; MAX_REPLAY],
            replay_len: 0,
            replay_pos: 0,
            tick_cycles: 0,
            in_body: false,
            body_kind: BodyKind::Instruction,
        }
    }

    /// The body's state as it stands, to unwind to if it suspends.
    fn capture_body_state(&self) -> BodyState {
        BodyState {
            d: self.d,
            a: self.a,
            usp: self.usp,
            ssp: self.ssp,
            pc: self.pc,
            sr: self.sr,
            prefetch: self.prefetch,
            prefetch_len: self.prefetch_len,
            words_consumed: self.words_consumed,
            words_without_refill: self.words_without_refill,
            internal_spent: self.internal_spent,
            ea_program_space: self.ea_program_space,
            stopped: self.stopped,
            halted: self.halted,
        }
    }

    /// Unwind the body to where it started, leaving what the bus has already
    /// done alone.
    fn restore_body_state(&mut self) {
        let b = self.body;
        self.d = b.d;
        self.a = b.a;
        self.usp = b.usp;
        self.ssp = b.ssp;
        self.pc = b.pc;
        self.sr = b.sr;
        self.prefetch = b.prefetch;
        self.prefetch_len = b.prefetch_len;
        self.words_consumed = b.words_consumed;
        self.words_without_refill = b.words_without_refill;
        self.internal_spent = b.internal_spent;
        self.ea_program_space = b.ea_program_space;
        self.stopped = b.stopped;
        self.halted = b.halted;
    }

    /// Whether the bus cycle the body is about to run has to wait for a clock
    /// of its own.
    ///
    /// The part drives one cycle every four clocks and overlaps none of them,
    /// so the first cycle of a clock goes ahead and a second one does not. The
    /// body has no way to wait, so it is unwound and run again instead.
    ///
    /// Two things switch this off. Outside a body there is nothing to unwind
    /// to. And once the log is full the cycles past it are not recorded, so
    /// running the body again would drive them a second time; past that point
    /// the body runs to the end with its remaining cycles on one clock, which
    /// is where every cycle used to be.
    #[inline]
    pub(crate) fn must_suspend(&self) -> bool {
        self.can_suspend() && self.tick_cycles > 0
    }

    /// Whether the body can be unwound at all.
    ///
    /// Outside a body there is nothing to unwind to, and once the log is full
    /// the cycles past it are not recorded, so running the body again would
    /// drive them a second time.
    #[inline]
    pub(crate) fn can_suspend(&self) -> bool {
        self.in_body && usize::from(self.replay_len) < MAX_REPLAY
    }

    /// The cycle at the cursor, if this attempt has not caught up with what
    /// earlier attempts already ran.
    #[inline]
    fn replayed(&self) -> Option<ReplayedCycle> {
        (self.replay_pos < self.replay_len).then(|| self.replay[usize::from(self.replay_pos)])
    }

    /// Give back the word a word read returned on an earlier attempt.
    ///
    /// A body is deterministic in its own state and in what the bus gave it,
    /// and both are reproduced exactly, so the cycles come back in the order
    /// they went in. A mismatch is a body reading something neither of those
    /// covers; the assertion names it, and the release build falls through to
    /// a real access rather than returning a word from the wrong cycle.
    #[inline]
    fn replay_read(&mut self) -> Option<u16> {
        match self.replayed()? {
            ReplayedCycle::Read(word) => {
                self.replay_pos += 1;
                Some(word)
            }
            other => {
                debug_assert!(false, "replay expected a word read, found {other:?}");
                None
            }
        }
    }

    /// Give back the byte a byte read returned on an earlier attempt.
    #[inline]
    fn replay_read_byte(&mut self) -> Option<u8> {
        match self.replayed()? {
            ReplayedCycle::ReadByte(byte) => {
                self.replay_pos += 1;
                Some(byte)
            }
            other => {
                debug_assert!(false, "replay expected a byte read, found {other:?}");
                None
            }
        }
    }

    /// Give back the word a queue refill fetched on an earlier attempt, putting
    /// it back into the hole the unwind reopened.
    #[inline]
    fn replay_refill(&mut self) -> bool {
        match self.replayed() {
            Some(ReplayedCycle::Refill(word)) => {
                self.replay_pos += 1;
                self.prefetch[usize::from(self.prefetch_len)] = word;
                self.prefetch_len += 1;
                true
            }
            Some(other) => {
                debug_assert!(false, "replay expected a refill, found {other:?}");
                false
            }
            None => false,
        }
    }

    /// Skip a handover the body made on an earlier attempt. The cycle is
    /// already on the bus unit's list or already driven; either way it must not
    /// be queued twice.
    #[inline]
    fn replay_handed(&mut self) -> bool {
        match self.replayed() {
            Some(ReplayedCycle::Handed) => {
                self.replay_pos += 1;
                true
            }
            Some(other) => {
                debug_assert!(false, "replay expected a handover, found {other:?}");
                false
            }
            None => false,
        }
    }

    /// Declare `clocks` of internal time the body has spent at this point in
    /// its sequence, ahead of an access that could fault.
    ///
    /// This does not lengthen the instruction: a body still declares its whole
    /// internal time at its finish, and that is what the length comes from.
    /// What this changes is the *aborted* case, where there is no finish and
    /// the clocks already spent would otherwise vanish. Declaring at the point
    /// of spending rather than tabulating per opcode is deliberate: where the
    /// arithmetic sits relative to the fetches is a fact about each family's
    /// sequence, and a table that tried to state it was written, checked,
    /// rejected on 22,337 cases and thrown away during M4.
    #[inline]
    pub(crate) fn spend_internal(&mut self, clocks: u32) {
        self.internal_spent += clocks;
    }

    /// Record a cycle this attempt has just run.
    #[inline]
    fn log_cycle(&mut self, cycle: ReplayedCycle) {
        if usize::from(self.replay_len) < MAX_REPLAY {
            self.replay[usize::from(self.replay_len)] = cycle;
            self.replay_len += 1;
            self.replay_pos = self.replay_len;
        }
    }

    /// Hand a bus cycle to the bus unit, to run on its own clock later.
    ///
    /// A full list is drained first rather than refused. Refusing would have
    /// the caller drive the new cycle at once while older ones were still
    /// waiting, which puts them on the bus in the wrong order, and a wrong
    /// order is a wrong program where a wrong clock is only a wrong clock.
    /// `MOVEM` is what reaches the limit, moving up to sixteen registers.
    pub(crate) fn hand_over<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        cycle: PendingCycle,
    ) {
        // Handed over on an earlier attempt at this body: it is on the list
        // already, or already driven, and either way this is not a new cycle.
        if self.replay_handed() {
            return;
        }
        if usize::from(self.pending_len) >= MAX_PENDING {
            self.flush_pending(bus, master);
        }
        self.pending[usize::from(self.pending_len)] = cycle;
        self.pending_len += 1;
        self.transfers += 1;
        self.log_cycle(ReplayedCycle::Handed);
    }

    /// Put clocks the part spends without driving anything onto the bus unit's
    /// list, so the cycles behind them land that much later.
    ///
    /// Not a transfer, so nothing is counted and the instruction's length is
    /// unchanged: the clocks are already in the internal time its finish
    /// declares. This only says *where* in the sequence they fall. It is
    /// replayed like a handover, so a body that runs again does not add them
    /// twice.
    pub(crate) fn defer_idle<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        clocks: u32,
    ) {
        // A step of no clocks is not a step. `TRAPV` asks for one, because it
        // is the one exception source with no idle in front of its frame, and
        // an entry of zero width would schedule the cycle behind it for a clock
        // that never arrives.
        if clocks == 0 {
            return;
        }
        if self.replay_handed() {
            return;
        }
        if usize::from(self.pending_len) >= MAX_PENDING {
            self.flush_pending(bus, master);
        }
        self.pending[usize::from(self.pending_len)] = PendingCycle::Idle { clocks };
        self.pending_len += 1;
        self.log_cycle(ReplayedCycle::Handed);
    }

    /// Spend internal time *and* put it on the bus unit's list, for a body that
    /// spends clocks part way through its own sequence.
    ///
    /// Two statements about the same clocks, and both are needed. One says they
    /// are already spent if the instruction goes on to fault, which is the only
    /// thing the abort path can charge them from. The other says where in the
    /// sequence they fall, so the cycles behind them land that much later.
    pub(crate) fn spend_idle<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        clocks: u32,
    ) {
        self.spend_internal(clocks);
        self.defer_idle(bus, master, clocks);
    }

    /// The clocks every outstanding entry occupies, and the last one's alone.
    fn outstanding_clocks(&self) -> (u32, u32) {
        let live = &self.pending[usize::from(self.pending_pos)..usize::from(self.pending_len)];
        let total = live.iter().map(|c| c.clocks()).sum();
        (total, live.last().map_or(0, |c| c.clocks()))
    }

    /// Of the outstanding entries, the clocks that drive nothing.
    ///
    /// A finish that places internal time in front of its fetches must not
    /// place the part of it the body already put on the list, or the same
    /// clocks are spent twice.
    fn outstanding_idle(&self) -> u32 {
        self.pending[usize::from(self.pending_pos)..usize::from(self.pending_len)]
            .iter()
            .filter_map(|c| match c {
                PendingCycle::Idle { clocks } => Some(*clocks),
                _ => None,
            })
            .sum()
    }

    /// Drive every cycle still outstanding, immediately and in order.
    ///
    /// The escape hatch for a body that needs the bus back before the bus unit
    /// would have got to them: an operand read cannot overtake a write the
    /// instruction has already decided on, so the writes go first, on this
    /// clock, and their positions are lost rather than their order.
    pub(crate) fn flush_pending<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        while self.pending_pos < self.pending_len {
            let cycle = self.pending[usize::from(self.pending_pos)];
            self.pending_pos += 1;
            self.drive_pending_cycle(bus, master, cycle);
        }
        self.pending_len = 0;
        self.pending_pos = 0;
    }

    /// Run one handed-over cycle. The transfer was counted when it was handed
    /// over, so this does not count it again.
    fn drive_pending_cycle<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        cycle: PendingCycle,
    ) {
        // A refill driven while the body can still suspend would put a word in
        // the queue that the unwind then throws away, and the body would fetch
        // it again. Nothing hands one over before its last read: the
        // read-modify-write families hand theirs over after both, and the
        // finish hands its own over once the body is done.
        debug_assert!(
            !(self.in_body && matches!(cycle, PendingCycle::Refill { .. })),
            "a refill driven while the body can still suspend would be unwound"
        );
        // An idle step drives nothing and occupies no clock of the bus unit's,
        // so it does not make the next cycle a second one on this clock.
        if matches!(cycle, PendingCycle::Idle { .. }) {
            return;
        }
        self.tick_cycles += 1;
        match cycle {
            PendingCycle::Idle { .. } => unreachable!("returned above"),
            PendingCycle::Refill { signals } => {
                if self.prefetch_len < 2 {
                    let addr =
                        self.mask_addr(self.pc.wrapping_add(2 * u32::from(self.prefetch_len)));
                    bus.observe_bus_cycle(master, addr, signals);
                    self.prefetch[usize::from(self.prefetch_len)] = bus.read(master, addr);
                    self.prefetch_len += 1;
                }
            }
            PendingCycle::Write {
                addr,
                data,
                byte,
                signals,
            } => {
                let a = self.mask_addr(addr);
                bus.observe_bus_cycle(master, a, signals);
                if byte {
                    bus.write_byte(master, a, data as u8);
                } else {
                    bus.write(master, a, data);
                }
            }
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
            rmw: false,
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
            rmw: false,
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
        // Cycles driven belong to the clock, not to whatever drove them.
        //
        // Two things turned on this. An instruction that faults hands over to
        // exception entry *inside this call*, and the clock it faulted on may
        // already carry the cycle it faulted after: `RTS` reads the second half
        // of its return address and then finds it odd, and the entry behind it
        // must not reuse that clock. And interrupt entry never runs through a
        // body at all, so zeroing this inside one left it counting cycles from
        // whenever a body last ran, which is not a number about this clock.
        // That second one is what moved Road Runner's golden frame: its
        // interrupt handler's two fetches were placed from an accumulated
        // count, and they now land where the part lands them.
        self.tick_cycles = 0;
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
                self.internal_spent = 0;

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
                    self.begin_address_error(bus, master, fault);
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
                    self.internal_spent = lead;
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
            ExecState::BodyWait(remaining) => {
                if remaining > 0 {
                    self.state = ExecState::BodyWait(remaining - 1);
                } else {
                    self.run_body(bus, master);
                }
            }
            ExecState::DrainPending { delay } => {
                let remaining = delay - 1;
                if remaining == 0 {
                    self.drain_one_pending(bus, master);
                } else {
                    self.state = ExecState::DrainPending { delay: remaining };
                }
            }
            ExecState::Execute(remaining) => {
                if remaining <= 1 {
                    self.state = ExecState::Fetch;
                } else {
                    self.state = ExecState::Execute(remaining - 1);
                }
            }
            ExecState::TrailingRefill { delay, tail } => {
                let remaining = delay - 1;
                if remaining == 0 {
                    // This clock is one the part drives a bus cycle on.
                    self.drive_next_pending(bus, master, tail);
                } else {
                    self.state = ExecState::TrailingRefill {
                        delay: remaining,
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
        let opcode = self.take_word_deferred_refill();
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

    /// Start the instruction body, whose opcode the loader has already taken.
    fn run_instruction<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        self.pre_exec_transfers = self.transfers;
        self.body_kind = BodyKind::Instruction;
        self.replay_len = 0;
        self.body = self.capture_body_state();
        self.run_body(bus, master);
    }

    /// Start exception entry as a body of its own, with its own state capture
    /// and its own replay log.
    ///
    /// The instruction is over: what it did to the registers stands, and what
    /// this unwinds to is the state the fault left. The log starts empty
    /// because the instruction's cycles are behind it and none of them may be
    /// replayed into the entry.
    fn begin_address_error<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        fault: addressing::AddressError,
    ) {
        self.body_kind = BodyKind::AddressError(fault);
        self.replay_len = 0;
        self.replay_pos = 0;
        self.body = self.capture_body_state();
        self.run_body(bus, master);
    }

    /// Run the body once.
    ///
    /// **A body can run more than once, and each run is one clock's worth of
    /// bus activity.** The part drives one bus cycle every four clocks; a body
    /// written as straight-line code drives all of its cycles in one call and
    /// cannot wait between them. So a cycle it has not run before, arriving on
    /// a clock that already has one, unwinds the body instead: the registers go
    /// back to where the body found them, the clocks of the cycles it did run
    /// are spent, and the body runs again. The second run finds those cycles in
    /// the log and is given their values back without touching the bus, so it
    /// reaches the next one having made no access twice, and runs it on a clock
    /// of its own.
    ///
    /// The log is what makes this safe rather than clever. Nothing is read
    /// twice, so a device with a side effect on read sees one access; nothing
    /// is written twice, because writes go to the bus unit and the log says
    /// which are already there; and the addresses cannot drift, because they
    /// are recomputed from restored registers and replayed words.
    ///
    /// What this costs is one run of the body per bus cycle the body runs
    /// itself, which for almost every instruction is one or two.
    fn run_body<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        self.replay_pos = 0;
        self.in_body = true;
        let outcome = match self.body_kind {
            BodyKind::Instruction => self.execute_instruction(self.opcode, bus, master),
            BodyKind::AddressError(fault) => self.address_error_body(bus, master, fault),
        };
        self.in_body = false;
        match outcome {
            Ok(()) => {}
            Err(addressing::Abort::Suspend) => {
                self.restore_body_state();
                // The clocks of the cycles this attempt did run, and of the
                // ones it handed over and is still waiting on. They are spent
                // here rather than charged at the finish, which is what keeps
                // the instruction the same length while its transfers move.
                let (outstanding, _) = self.outstanding_clocks();
                let span = (4 * self.tick_cycles + outstanding).max(4);
                self.exec_clock += span;
                if outstanding == 0 {
                    self.state = ExecState::BodyWait(span - 1);
                    return;
                }
                // The cycles this attempt drove occupy the clocks after it, so
                // the first outstanding one belongs on the clock after the last
                // of them, and on *this* clock when there were none.
                let delay = 4 * self.tick_cycles;
                if delay == 0 {
                    self.drain_one_pending(bus, master);
                } else {
                    self.state = ExecState::DrainPending { delay };
                }
            }
            Err(addressing::Abort::Fault(fault)) => {
                // Only an instruction can reach here. The entry body catches
                // its own frame faults, because a fault there is a double bus
                // fault and halts rather than entering again.
                debug_assert!(
                    matches!(self.body_kind, BodyKind::Instruction),
                    "exception entry must not propagate a fault"
                );
                self.begin_address_error(bus, master, fault);
            }
        }
    }

    /// Drive one cycle of a suspended body's outstanding list, and either queue
    /// the one after it or let the body run again four clocks later.
    fn drain_one_pending<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        let cycle = self.pending[usize::from(self.pending_pos)];
        self.pending_pos += 1;
        self.drive_pending_cycle(bus, master, cycle);
        let width = cycle.clocks();
        if self.pending_pos < self.pending_len {
            self.state = ExecState::DrainPending { delay: width };
        } else {
            self.pending_len = 0;
            self.pending_pos = 0;
            self.state = ExecState::BodyWait(width - 1);
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
        // The body is done. What follows is the bus unit's, and it neither
        // suspends nor unwinds.
        self.in_body = false;
        // Whatever the body handed over is still to run, and so is the refill
        // it leaves owed. They go on one list in that order, because that is
        // the order the part drives them: an instruction's write precedes the
        // fetch behind the word it consumed.
        let owed = u32::from(2 - self.prefetch_len) - u32::from(self.refills_pending());
        for _ in 0..owed {
            let signals = self.program_cycle(false);
            self.hand_over(bus, master, PendingCycle::Refill { signals });
        }
        let full = 4 * self.transfers + internal;

        // Everything is measured from the clock the body ran on, because the
        // loader has already spent clocks in front of it. Saturating rather
        // than asserting: a wrong row in `leading_internal` would otherwise
        // underflow here, and the symptom should be a rung-3 miss on that row
        // rather than a panic on a board.
        let from_body = full.saturating_sub(self.exec_clock);

        // Nothing outstanding: the body ran every cycle itself.
        if self.pending_len == 0 {
            self.finish(from_body);
            return;
        }

        // The cycles the body ran on *this* clock occupy the clocks after it,
        // four each, so the first handed-over cycle belongs on the clock after
        // the last of them. Only this clock's are counted: the ones the body
        // ran on earlier attempts are already inside `exec_clock`, spent when
        // each of those attempts unwound.
        let mut delay = 4 * self.tick_cycles;
        if internal_first {
            // Whatever of the internal time was not already burned up front as
            // address arithmetic, nor already placed on the list by the body,
            // runs here ahead of the fetches it computes.
            delay += internal
                .saturating_sub(self.lead_burned)
                .saturating_sub(self.outstanding_idle());
        }
        // Each outstanding entry occupies its own clocks, one after the last,
        // so `tail` is measured from the clock the *final* one runs on.
        let (outstanding, last) = self.outstanding_clocks();
        let tail = from_body
            .saturating_sub(delay)
            .saturating_sub(outstanding - last);
        if delay == 0 {
            self.drive_next_pending(bus, master, tail);
            return;
        }
        self.state = ExecState::TrailingRefill { delay, tail };
    }

    /// Drive the next outstanding cycle on this clock, and either queue the one
    /// after it or end the instruction.
    fn drive_next_pending<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster, tail: u32) {
        let cycle = self.pending[usize::from(self.pending_pos)];
        self.pending_pos += 1;
        self.drive_pending_cycle(bus, master, cycle);
        if self.pending_pos < self.pending_len {
            self.state = ExecState::TrailingRefill {
                delay: cycle.clocks(),
                tail,
            };
        } else {
            self.pending_len = 0;
            self.pending_pos = 0;
            self.finish(tail);
        }
    }

    /// How many refills the body has already handed over, so the finish does
    /// not ask for them twice.
    fn refills_pending(&self) -> u8 {
        self.pending[usize::from(self.pending_pos)..usize::from(self.pending_len)]
            .iter()
            .filter(|c| matches!(c, PendingCycle::Refill { .. }))
            .count() as u8
    }

    /// Complete an instruction that leaves the prefetch queue as it found it.
    ///
    /// `STOP` is the only user and the recorded trace is why: a supervisor
    /// `STOP` is four clocks with no bus cycle at all, and its recorded final
    /// state has the queue and PC exactly where they started. The part stops
    /// before issuing the refill, so charging one here would put `STOP` at
    /// eight clocks and fetch a word nothing executes.
    pub(crate) fn finish_without_refill(&mut self, internal: u32) {
        self.in_body = false;
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

    /// A bus that keeps the clock each cycle was driven on, counting one clock
    /// per tick the way a board does.
    struct ClockBus {
        inner: WordBus,
        clock: u32,
        seen: Vec<(u32, u32, bool)>,
    }

    impl Bus for ClockBus {
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
            self.seen.push((self.clock, addr, signals.is_write));
        }
    }

    impl Bus16 for ClockBus {
        fn read_byte(&mut self, m: BusMaster, addr: u32) -> u8 {
            self.inner.read_byte(m, addr)
        }
        fn write_byte(&mut self, m: BusMaster, addr: u32, data: u8) {
            self.inner.write_byte(m, addr, data);
        }
    }

    /// Run one instruction from a full queue, returning the clock each bus
    /// cycle was driven on and how long the instruction took.
    fn cycles_of(program: &[u8], setup: impl FnOnce(&mut M68000)) -> (Vec<(u32, u32, bool)>, u32) {
        let mut cpu = M68000::new();
        let mut bus = ClockBus {
            inner: WordBus::new(),
            clock: 0,
            seen: Vec::new(),
        };
        bus.inner.load(0x1000, program);
        cpu.set_pc_flush(0x1000);
        setup(&mut cpu);
        // Seed the queue the way the part reaches an instruction: already
        // holding the two words at PC, so the fetches this counts are the
        // instruction's own.
        cpu.fill_prefetch(&mut bus, BusMaster::Cpu(0));
        bus.seen.clear();

        let mut ticks = 0;
        loop {
            bus.clock = ticks;
            ticks += 1;
            if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
                break;
            }
            assert!(ticks < 200, "the instruction must retire");
        }
        (bus.seen, ticks)
    }

    /// Two operand reads in a row are two bus cycles four clocks apart, not two
    /// accesses on one clock.
    ///
    /// The part drives one cycle every four clocks and has nothing to do
    /// between the halves of a long read: its microcode reads the high word,
    /// spends four clocks, and reads the low word. `UNLK A0` is the smallest
    /// instruction that shows it whole, at twelve clocks and three cycles:
    /// the two halves of the long at (A0), then the refill behind the opcode.
    ///
    /// This is what an instruction body running in one tick cannot do, and the
    /// reason a body is unwound and run again: see [`M68000::run_body`].
    #[test]
    fn the_two_halves_of_a_long_read_are_four_clocks_apart() {
        // UNLK A0 = 0x4E58, then a word for the refill to fetch.
        let (seen, ticks) = cycles_of(&[0x4E, 0x58, 0x4E, 0x71, 0x00, 0x00], |cpu| {
            cpu.a[0] = 0x2000;
        });
        assert_eq!(
            seen,
            vec![(0, 0x2000, false), (4, 0x2002, false), (8, 0x1004, false),],
            "the long's two words, then the refill, one every four clocks"
        );
        assert_eq!(ticks, 12, "spreading the reads must not change the length");
    }

    /// Unwinding a body must not make it access the bus twice.
    ///
    /// The bus is asked for exactly as many cycles as the instruction has, and
    /// each at its own address: a body that re-ran its earlier reads for real
    /// would still produce the right register state, because the memory has not
    /// changed, and only the access count would say so.
    #[test]
    fn a_body_that_runs_again_does_not_access_the_bus_again() {
        // MOVE.l (A0)+, D0 = 0x2018: two operand reads and one refill.
        let (seen, _) = cycles_of(&[0x20, 0x18, 0x4E, 0x71, 0x00, 0x00], |cpu| {
            cpu.a[0] = 0x2000;
        });
        assert_eq!(
            seen,
            vec![(0, 0x2000, false), (4, 0x2002, false), (8, 0x1004, false),],
            "three cycles for a three-cycle instruction, however often the body ran"
        );
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
