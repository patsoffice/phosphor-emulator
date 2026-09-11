//! M68000 per-cycle gate: this core's timing and bus activity against the
//! recorded traces, measured before any per-cycle conversion work.
//!
//! This reports rather than asserts. Today's core is atomic: it decodes and
//! applies an instruction's whole effect on its first clock, then burns the
//! remaining documented cycles as idle, so its bus accesses all land on one
//! clock and in whatever order the interpreter makes them. The number this
//! prints is how far that is from the part, and it is the evidence the
//! conversion decision is taken on. See `docs/designs/cycle-accurate-m68000.md`.
//!
//! # The rungs
//!
//! 1. **`length`**: our clock count against the recording's.
//! 2. **Transfer kinds and order**: reads and writes in sequence, positions
//!    ignored.
//! 3. **Positions**: every transfer starts on the clock the recording starts it
//!    on.
//! 4. **Operands**: address, size and data, per transfer.
//! 5. **Function code**: program against data, supervisor against user.
//! 6. **Final PC and prefetch queue**, asserted rather than reported.
//!
//! Rungs 3, 4 and 5 are conditioned on rung 2 and reported as a share of *all*
//! cases, which makes each rung an upper bound on the next: comparing a
//! position element by element against a sequence that is not the same sequence
//! would pair transfers that are not counterparts and report their
//! disagreement as a positional error.
//!
//! **Rung 3 is the one an atomic core cannot answer**, and until M4 this gate
//! did not report it for that reason: a core that applies an instruction's
//! whole effect on one clock puts every transfer on clock zero, so the number
//! would have said "0%" for a structural reason rather than a timing one. It is
//! reported now because the structure is changing underneath it, and what it
//! reads before the change is the baseline the change is measured against. The
//! cases it passes today are the ones whose recorded activity is a single
//! transfer at clock zero, and that is not an achievement, it is arithmetic.
//!
//! # Two suites, and what can and cannot be compared across them
//!
//! Both vector sets are read. **They are not the same cases**: they were
//! randomly generated separately, with different initial states and different
//! names, so no vector in one has a counterpart in the other and a per-case
//! cross-set diff is impossible. What is comparable is our agreement *rate* per
//! instruction file. Where our rate against one set differs materially from our
//! rate against the other for the same instruction, the two oracles disagree
//! about that instruction, and that is the divergence worth chasing.

use std::collections::BTreeMap;
use std::io::Read;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{self, M68000};
use phosphor_cpu_validation::{
    BusTxnKind, M68000Regs, M68000TestCase, RecordingBus68k, TxnSize, m68000_bin,
};

const ADDR_MASK: u32 = 0x00FF_FFFF;

/// Ceiling on how long one instruction may run before the harness gives up.
/// The longest documented 68000 instruction is well under this.
const TICK_LIMIT: u32 = 500;

// ---------------------------------------------------------------------------
// Running one case
// ---------------------------------------------------------------------------

/// How this core's run compared with the recording, for one case.
#[derive(Default)]
struct CaseResult {
    length_exact: bool,
    /// Ours minus the recording, in clocks.
    length_delta: i64,
    kinds_exact: bool,
    /// Rung 3: every transfer starts on the clock the recording starts it on.
    positions_exact: bool,
    /// Rung 4: address, size and data agree on every transfer.
    operands_exact: bool,
    /// Rung 5: the function code agrees on every transfer.
    fc_exact: bool,
    /// Whether we ran the same *number* of transfers, order aside.
    count_exact: bool,
    /// Our PC at the boundary against the recorded one.
    pc_exact: bool,
    /// Both queue words against the recorded final pair (rung 6).
    prefetch_exact: bool,
    /// The queue holds the words that are actually at `pc` and `pc + 2`.
    ///
    /// This is the check that catches a control transfer which forgot to
    /// discard the queue: it would leave words from the old instruction stream
    /// in it, and they would no longer be the words memory holds at the new PC.
    /// It is asserted rather than reported, because there is no legitimate way
    /// for it to fail.
    invariant_holds: bool,
    /// The instruction wrote over the words the queue holds, so the invariant
    /// above cannot be applied to it.
    stream_written: bool,
    /// What the recording's own clocks say the instruction spent away from the
    /// bus: `length` less four clocks for each transfer it ran.
    recorded_internal: i64,
    /// The case could not be run at all (it hit the tick limit).
    ran: bool,
    /// For a case whose transfer sequence differs: the recorded shape and
    /// ours, so the residual can be counted by shape rather than described.
    mismatch: Option<(String, String)>,
    /// For a case whose sequence matches but whose transfers land on the wrong
    /// clocks: the recorded positions against ours.
    ///
    /// A rung-3 rate says how much is left and the population split says
    /// whether it is reachable at all; neither says *which* clock is wrong, and
    /// with the loader placing several things per instruction that is the only
    /// question worth asking of a miss.
    position_fault: Option<(String, String)>,
    /// The mechanism behind that miss, named from the first clock that
    /// disagrees rather than from the instruction it happened in.
    ///
    /// A shape list says which instructions are wrong and a rate says how much
    /// is left; neither says how many *different* things are wrong. This does:
    /// a transfer that landed on the clock of the one before it is a body that
    /// ran two bus cycles in one tick, and that is a different defect from a
    /// transfer that is merely shifted, however similar the two shape lists
    /// look. See [`classify_position_fault`].
    position_class: Option<&'static str>,
    /// For a case whose sequence matches but whose function codes do not: the
    /// first transfer that differs, with both codes.
    fc_fault: Option<String>,
    /// For a case whose sequence matches but whose transfers do not: the first
    /// transfer that differs and the field it differs in.
    operand_fault: Option<String>,
    /// Words the executor took out of the queue, against what the length table
    /// predicted from the opcode alone. Equal on every completed case, or a
    /// loader built on the table would fetch the wrong number of words.
    words_consumed: u32,
    words_predicted: u32,
    /// The same for the words taken without a refill behind them, which a
    /// loader also has to know from the opcode before it fetches anything.
    no_refill_actual: u32,
    no_refill_predicted: u32,
}

fn load_initial(
    cpu: &mut M68000,
    bus: &mut RecordingBus68k,
    st: &M68000Regs,
    execution_pc: u32,
    loaded: &mut Vec<u32>,
) {
    cpu.d = st.d();
    cpu.a[..7].copy_from_slice(&st.a());
    cpu.a[7] = st.active_sp();
    cpu.usp = st.usp;
    cpu.ssp = st.ssp;
    cpu.sr = st.sr;
    cpu.set_pc_flush(execution_pc);
    // The queue starts full, holding the two words the recording says the part
    // had already fetched. Seeding it is what makes the transfer counts
    // comparable at all: an instruction that had to fetch its own opcode would
    // run one transfer ahead of every recorded trace, because none of them
    // contains that fetch.
    cpu.load_prefetch_queue(st.prefetch);

    for &(addr, val) in &st.ram {
        let a = addr & ADDR_MASK;
        bus.memory[a as usize] = val;
        loaded.push(a);
    }
    // The queued words are also placed in memory: they are not necessarily
    // present in ram[], and a refetch after a flush reads them again.
    for (i, &word) in st.prefetch.iter().enumerate() {
        let a = (execution_pc.wrapping_add(2 * i as u32)) & ADDR_MASK & !1;
        bus.memory[a as usize] = (word >> 8) as u8;
        bus.memory[(a + 1) as usize] = word as u8;
        loaded.push(a);
        loaded.push(a + 1);
    }
}

/// One recorded transfer, reduced to the things this core can be asked about.
///
/// The duration is not among them, and that is a fact rather than an omission:
/// every transfer this part makes is four clocks at immediate DTACK, which M2
/// checked against all 1,317,560 cases of both corpora, so a duration says
/// nothing a position does not already say. The position does say something,
/// and the entries' exact tiling of `length` is what fixes it.
struct RecordedTxn {
    /// The clock this transfer starts on, counted from the instruction's first.
    start: u32,
    write: bool,
    /// The byte the transfer touched, resolved out of whichever way its suite
    /// posts the address.
    addr: u32,
    /// One byte behind a single strobe rather than a word behind both.
    byte: bool,
    data: u16,
    /// The function code FC2..FC0 the part drove with the address.
    fc: u8,
}

/// The recorded transfers, in order, each with the clock it starts on.
///
/// Idle spans and the two address-error kinds drop out of the *sequence*: a
/// cycle run with the address strobe never asserted commits no transfer, and
/// this core does not run one. They stay in the running clock total, because
/// they are time the part spent and every transfer behind them starts that
/// much later. Dropping them from the total instead would have moved every
/// position after an address error by the width of the aborted cycle.
fn recorded_transfers(tc: &M68000TestCase) -> Vec<RecordedTxn> {
    let mut out = Vec::with_capacity(tc.transactions.len());
    let mut clock = 0;
    for t in &tc.transactions {
        match t.kind {
            BusTxnKind::Read | BusTxnKind::Write | BusTxnKind::Tas => out.push(RecordedTxn {
                start: clock,
                write: t.kind == BusTxnKind::Write,
                addr: t.byte_address() & ADDR_MASK,
                byte: t.size == TxnSize::Byte,
                data: if t.size == TxnSize::Byte {
                    t.byte_value() as u16
                } else {
                    t.data as u16
                },
                fc: t.fc as u8,
            }),
            BusTxnKind::Idle | BusTxnKind::ReadAddressError | BusTxnKind::WriteAddressError => {}
        }
        clock += t.clocks;
    }
    out
}

/// Name the mechanism behind one rung-3 miss, from the first transfer whose
/// clock disagrees.
///
/// The distinction that matters is **bunching**: a transfer on the same clock
/// as the one before it was run by a body that did two bus cycles inside one
/// tick, and the fix for that is suspending the body. A transfer that is on a
/// clock of its own but the wrong one is a placement or internal-time question
/// and has nothing to do with suspension. Reading a shape list cannot tell
/// those apart, because both print as two lists of numbers that differ.
///
/// The kinds of the bunched pair are part of the class, because the three
/// combinations have different answers: a write behind a write is a bus unit
/// driving its list too fast, a read behind a read cannot be handed to the bus
/// unit at all, and a read behind a write is an ordering flush.
fn classify_position_fault(ours: &[(bool, u32)], recorded: &[(bool, u32)]) -> &'static str {
    let Some(i) = (0..ours.len()).find(|&i| ours[i].1 != recorded[i].1) else {
        return "no disagreement";
    };
    if i == 0 {
        return if ours[0].1 < recorded[0].1 {
            "first transfer early"
        } else {
            "first transfer late"
        };
    }
    if ours[i].1 == ours[i - 1].1 {
        return match (ours[i - 1].0, ours[i].0) {
            (false, false) => "read bunched onto a read",
            (true, false) => "read bunched onto a write",
            (false, true) => "write bunched onto a read",
            (true, true) => "write bunched onto a write",
        };
    }
    if ours[i].1 < recorded[i].1 {
        "on its own clock, early"
    } else {
        "on its own clock, late"
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::classify_position_fault;

    /// `(kind, clock)` pairs from a printed shape list like `R0 R4 R8`.
    fn list(s: &str) -> Vec<(bool, u32)> {
        s.split_whitespace()
            .map(|t| {
                let (k, c) = t.split_at(1);
                (k == "W", c.parse().expect("a clock"))
            })
            .collect()
    }

    #[test]
    fn a_second_read_on_the_first_read_s_clock_is_named_as_bunching() {
        // UNLK: the two halves of a long operand read, run in one tick.
        assert_eq!(
            classify_position_fault(&list("R0 R0 R8"), &list("R0 R4 R8")),
            "read bunched onto a read"
        );
        // SBCD: the source and destination byte reads, run in one tick, with
        // the write after them already on its recorded clock.
        assert_eq!(
            classify_position_fault(&list("R2 R2 R10 W14"), &list("R2 R6 R10 W14")),
            "read bunched onto a read"
        );
    }

    #[test]
    fn a_transfer_on_a_clock_of_its_own_is_not_named_as_bunching() {
        // PEA: both reads are on clocks of their own and the second is two
        // clocks early, which is internal time missing between them rather
        // than a body running two cycles at once. Calling this bunching would
        // have put a placement question in with the suspension ones.
        assert_eq!(
            classify_position_fault(&list("R2 R6 W12 W16"), &list("R2 R8 W12 W16")),
            "on its own clock, early"
        );
        assert_eq!(
            classify_position_fault(&list("R0 R4 R8"), &list("R0 R4 R8")),
            "no disagreement"
        );
    }

    #[test]
    fn the_first_transfer_is_classified_apart_from_the_ones_behind_it() {
        // Nothing precedes transfer 0, so it cannot be bunched onto anything
        // and the class has to say which direction it moved instead.
        assert_eq!(
            classify_position_fault(&list("R0 R4"), &list("R2 R6")),
            "first transfer early"
        );
        assert_eq!(
            classify_position_fault(&list("W4 W8"), &list("W2 W6")),
            "first transfer late"
        );
    }
}

/// A transfer sequence as a printable string: `R` read, `W` write.
///
/// Program reads are not distinguished from data reads here, because this core
/// does not tell the bus which it is making. The recorded side is rendered the
/// same way so the two are comparable; `m68000_trace_shapes` is the tool that
/// shows the recorded side with its prefetches marked.
fn shape_of(kinds: &[bool]) -> String {
    kinds
        .iter()
        .map(|&w| if w { 'W' } else { 'R' })
        .collect::<Vec<_>>()
        .chunks(1)
        .map(|c| c[0].to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// How many words out of the queue a loader should expect this case to consume:
/// the opcode, plus the extension words the encoding names.
///
/// A privileged instruction executed in user mode consumes only its opcode,
/// because the part settles privilege at decode and vectors without running.
fn predicted_words(tc: &M68000TestCase) -> u32 {
    let opcode = tc.initial.prefetch[0];
    if m68000::format::privileged(opcode) && !tc.initial.is_supervisor() {
        return 1;
    }
    1 + u32::from(m68000::format::extension_words(opcode))
}

/// How many of this case's extension words a loader should take without
/// refilling behind them.
///
/// Zero unless the instruction is one that discards the queue, and zero for
/// those too when the encoding has no extension word: a byte-displacement
/// `Bcc` suppresses nothing because it consumes nothing.
fn predicted_words_without_refill(tc: &M68000TestCase) -> u32 {
    let opcode = tc.initial.prefetch[0];
    if !m68000::format::suppresses_refill(opcode) {
        return 0;
    }
    if m68000::format::privileged(opcode) && !tc.initial.is_supervisor() {
        return 0;
    }
    u32::from(m68000::format::extension_words(opcode))
}

/// The word memory holds at `addr`, read behind the bus's back so the check
/// does not appear in the access log it is checking.
fn peek_word(bus: &RecordingBus68k, addr: u32) -> u16 {
    let i = (addr & ADDR_MASK & !1) as usize;
    u16::from_be_bytes([bus.memory[i], bus.memory[i + 1]])
}

fn run_case(
    tc: &M68000TestCase,
    execution_pc: u32,
    final_pc: u32,
    cpu: &mut M68000,
    bus: &mut RecordingBus68k,
) -> CaseResult {
    // Start every case from a fresh CPU, not just fresh registers.
    //
    // `stopped` and `halted` are sticky and no register load clears them, so a
    // single case that executes STOP parks the CPU for every case after it:
    // nothing here supplies an interrupt to wake it, so they all run to the
    // tick limit and report zero. That is exactly what the first run of this
    // gate did, from `STOP.json.bin` onward, which is 17 files of the m68000
    // corpus reading a clean 0.00% for a harness reason. The 680x0 set has no
    // STOP vectors at all, so its coverage had been hiding this.
    *cpu = M68000::new();

    let mut loaded = Vec::new();
    load_initial(cpu, bus, &tc.initial, execution_pc, &mut loaded);

    bus.start_recording();
    // The clock is published to the bus *before* each tick, so every access the
    // CPU makes during that tick is stamped with the clock it happened on. That
    // stamp is the whole of rung 3 on our side: the recording's positions come
    // from its entries tiling `length`, and this is the counterpart.
    let mut ticks: u32 = 0;
    let mut timed_out = false;
    loop {
        bus.clock = ticks;
        ticks += 1;
        if cpu.tick_with_bus(bus, BusMaster::Cpu(0)) {
            break;
        }
        if ticks > TICK_LIMIT {
            timed_out = true;
            break;
        }
    }
    bus.stop_recording();

    let result = if timed_out {
        CaseResult::default()
    } else {
        let recorded = recorded_transfers(tc);
        let recorded_shape: Vec<bool> = recorded.iter().map(|t| t.write).collect();
        let our_shape: Vec<bool> = bus.log.iter().map(|a| a.write).collect();
        let kinds_exact = our_shape == recorded_shape;
        // Rungs 3, 4 and 5 refine rung 2 rather than standing beside it.
        // Comparing a position, an address or a function code element by
        // element only means anything once the two sequences are the same
        // sequence; against a differing one it would pair up transfers that are
        // not counterparts and report their disagreement as a positional error.
        // So each is conditioned on rung 2, which also makes each rung an upper
        // bound on the next and gives the ladder a self-check.
        let paired = || bus.log.iter().zip(recorded.iter());
        let positions_exact = kinds_exact && paired().all(|(a, b)| a.clock == b.start);
        let operands_exact = kinds_exact
            && paired().all(|(a, b)| a.addr == b.addr && a.byte == b.byte && a.data == b.data);
        let fc_exact = kinds_exact && paired().all(|(a, b)| a.fc == b.fc);
        // The clocks each side put its transfers on, rendered with the kind so
        // a misplaced prefetch is distinguishable from a misplaced operand.
        let clock_list = |render: &dyn Fn() -> Vec<(char, u32)>| {
            render()
                .into_iter()
                .map(|(k, c)| format!("{k}{c}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        // Which transfer named the wrong address space, and what each side
        // called it. The codes are 1 user data, 2 user program, 5 supervisor
        // data, 6 supervisor program, so the pair says at a glance whether the
        // disagreement is about the space or about the privilege.
        let fc_fault = (kinds_exact && !fc_exact).then(|| {
            let (i, (a, b)) = paired()
                .enumerate()
                .find(|(_, (a, b))| a.fc != b.fc)
                .expect("a differing transfer, since the rung failed");
            format!(
                "transfer {i} ({}): recorded fc{} ours fc{}",
                if b.write { "write" } else { "read" },
                b.fc,
                a.fc
            )
        });
        let position_class = (kinds_exact && !positions_exact).then(|| {
            let ours: Vec<(bool, u32)> = bus.log.iter().map(|a| (a.write, a.clock)).collect();
            let theirs: Vec<(bool, u32)> = recorded.iter().map(|t| (t.write, t.start)).collect();
            classify_position_fault(&ours, &theirs)
        });
        let position_fault = (kinds_exact && !positions_exact).then(|| {
            (
                clock_list(&|| {
                    recorded
                        .iter()
                        .map(|t| (if t.write { 'W' } else { 'R' }, t.start))
                        .collect()
                }),
                clock_list(&|| {
                    bus.log
                        .iter()
                        .map(|a| (if a.write { 'W' } else { 'R' }, a.clock))
                        .collect()
                }),
            )
        });
        // The first transfer that disagrees, named by the field that disagrees.
        // A rate says how much of rung 4 is left; this says what to look at, and
        // the distinction between a wrong address, a wrong width and a wrong
        // value is the whole diagnosis: the first is an addressing defect, the
        // second a strobe defect, and the third often neither, because a value
        // read out of memory the harness never seeded is a fault in the harness.
        let operand_fault = (kinds_exact && !operands_exact).then(|| {
            let (i, (a, b)) = paired()
                .enumerate()
                .find(|(_, (a, b))| a.addr != b.addr || a.byte != b.byte || a.data != b.data)
                .expect("a differing transfer, since the rung failed");
            let field = if a.addr != b.addr {
                "addr"
            } else if a.byte != b.byte {
                "size"
            } else {
                "data"
            };
            format!(
                "{field} at transfer {i}: recorded {}{} {:#08x}={:#06x} ours {}{} {:#08x}={:#06x}",
                if b.write { "W" } else { "R" },
                if b.byte { ".b" } else { ".w" },
                b.addr,
                b.data,
                if a.write { "W" } else { "R" },
                if a.byte { ".b" } else { ".w" },
                a.addr,
                a.data,
            )
        });
        // Every transfer on this part is four clocks with immediate DTACK, and
        // the recording's entries tile its length, so whatever is left over is
        // the time the instruction spent away from the bus.
        let recorded_internal = tc.length as i64 - 4 * recorded.len() as i64;
        let (queue, queue_len) = cpu.prefetch_queue();
        // The queue must hold what memory holds at PC. A missed flush is
        // exactly what breaks this, and nothing else does, except an
        // instruction that writes over the words the queue holds, which the
        // corpus does contain: `NOT.l (xxx).w` case 820 of the 680x0 set
        // overwrites its own instruction stream, and the queue rightly holds
        // what was there when it fetched. Those cases are excluded and counted
        // rather than passed, so the exemption cannot quietly grow.
        let stream = [cpu.pc(), cpu.pc().wrapping_add(2)];
        let stream_written = bus
            .log
            .iter()
            .any(|a| a.write && stream.contains(&(a.addr & ADDR_MASK & !1)));
        let invariant_holds = stream_written
            || (0..queue_len as u32)
                .all(|i| queue[i as usize] == peek_word(bus, cpu.pc().wrapping_add(2 * i)));
        CaseResult {
            length_exact: ticks == tc.length,
            length_delta: ticks as i64 - tc.length as i64,
            kinds_exact,
            positions_exact,
            operands_exact,
            fc_exact,
            count_exact: our_shape.len() == recorded_shape.len(),
            pc_exact: cpu.pc() == final_pc,
            prefetch_exact: queue_len == 2 && queue == tc.final_state.prefetch,
            invariant_holds,
            stream_written,
            recorded_internal,
            ran: true,
            mismatch: (!kinds_exact).then(|| (shape_of(&recorded_shape), shape_of(&our_shape))),
            position_fault,
            position_class,
            fc_fault,
            operand_fault,
            words_consumed: cpu.words_consumed(),
            words_predicted: predicted_words(tc),
            no_refill_actual: cpu.words_without_refill(),
            no_refill_predicted: predicted_words_without_refill(tc),
        }
    };

    for &a in &loaded {
        bus.memory[a as usize] = 0;
    }
    for &a in &bus.dirty_writes {
        bus.memory[a as usize] = 0;
        bus.memory[a as usize + 1] = 0;
    }
    bus.dirty_writes.clear();

    result
}

// ---------------------------------------------------------------------------
// Tallies
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Tally {
    cases: usize,
    ran: usize,
    length_exact: usize,
    kinds_exact: usize,
    positions_exact: usize,
    operands_exact: usize,
    fc_exact: usize,
    count_exact: usize,
    pc_exact: usize,
    prefetch_exact: usize,
    /// Cases where the queue did not hold the words at PC. Must be zero.
    invariant_broken: usize,
    /// Cases the invariant cannot be applied to, because the instruction wrote
    /// over its own instruction stream.
    invariant_untestable: usize,
    length_delta_sum: i64,
    /// The extreme clock deltas seen, so a group whose errors cancel is not
    /// reported as a group that is nearly right.
    ///
    /// `ADD.l`'s `An` source group is why this exists: it is exact on none of
    /// its 771 cases and its mean is -0.03, which reads like a rounding
    /// artifact and is in fact two populations of equal size missing in
    /// opposite directions. A mean is the wrong summary for a residual and can
    /// only be trusted once the range says the group is one population.
    /// `None` until a case has been counted, so an all-negative group is not
    /// reported as reaching zero because zero is what the field started at.
    length_delta_range: Option<(i64, i64)>,
    /// How many cases missed by each amount.
    ///
    /// A range says a group is one population or two; it does not say how the
    /// cases are distributed between the ends, and for a residual that is
    /// supposed to be one constant that is the whole question. The address-error
    /// population is what this was built for: a corpus disagreement worth
    /// resolving is one where every case misses by the same amount, and one
    /// where the amounts are spread is a mechanism hiding behind an average.
    length_deltas: BTreeMap<i64, usize>,
    /// Cases whose recorded internal time is negative, which would mean the
    /// four-clock transfer model does not hold for them.
    impossible_internal: usize,
}

impl Tally {
    /// Cases that hit the tick limit instead of retiring.
    ///
    /// Counted separately and reported, because a case that never ran is not
    /// the same as one that ran and disagreed. Folding the two together is how
    /// a harness fault reads as a timing result: the first run of this gate
    /// reported a clean 0.00% for 17 files that had simply never executed.
    fn timed_out(&self) -> usize {
        self.cases - self.ran
    }

    fn add(&mut self, r: &CaseResult) {
        self.cases += 1;
        if !r.ran {
            return;
        }
        self.ran += 1;
        if r.length_exact {
            self.length_exact += 1;
        }
        if r.kinds_exact {
            self.kinds_exact += 1;
        }
        if r.positions_exact {
            self.positions_exact += 1;
        }
        if r.operands_exact {
            self.operands_exact += 1;
        }
        if r.fc_exact {
            self.fc_exact += 1;
        }
        if r.count_exact {
            self.count_exact += 1;
        }
        if r.pc_exact {
            self.pc_exact += 1;
        }
        if r.prefetch_exact {
            self.prefetch_exact += 1;
        }
        if !r.invariant_holds {
            self.invariant_broken += 1;
        }
        if r.stream_written {
            self.invariant_untestable += 1;
        }
        if r.recorded_internal < 0 {
            self.impossible_internal += 1;
        }
        self.length_delta_sum += r.length_delta;
        *self.length_deltas.entry(r.length_delta).or_default() += 1;
        self.length_delta_range = Some(match self.length_delta_range {
            Some((lo, hi)) => (lo.min(r.length_delta), hi.max(r.length_delta)),
            None => (r.length_delta, r.length_delta),
        });
    }

    fn pct(part: usize, whole: usize) -> f64 {
        if whole == 0 {
            0.0
        } else {
            100.0 * part as f64 / whole as f64
        }
    }

    fn length_pct(&self) -> f64 {
        Self::pct(self.length_exact, self.cases)
    }

    fn kinds_pct(&self) -> f64 {
        Self::pct(self.kinds_exact, self.cases)
    }

    fn positions_pct(&self) -> f64 {
        Self::pct(self.positions_exact, self.cases)
    }

    fn operands_pct(&self) -> f64 {
        Self::pct(self.operands_exact, self.cases)
    }

    fn fc_pct(&self) -> f64 {
        Self::pct(self.fc_exact, self.cases)
    }

    fn count_pct(&self) -> f64 {
        Self::pct(self.count_exact, self.cases)
    }

    fn pc_pct(&self) -> f64 {
        Self::pct(self.pc_exact, self.cases)
    }

    fn prefetch_pct(&self) -> f64 {
        Self::pct(self.prefetch_exact, self.cases)
    }

    fn mean_delta(&self) -> f64 {
        if self.ran == 0 {
            0.0
        } else {
            self.length_delta_sum as f64 / self.ran as f64
        }
    }
}

/// The population splits reported alongside every aggregate.
///
/// Reporting one number was the I8088 conversion's most expensive mistake: a
/// fifteenfold gap between two halves of that corpus sat inside a single rate
/// for several sessions. These are the splits available before any conversion
/// work, and each one is a property this core might plausibly get wrong on one
/// side and right on the other.
#[derive(Default)]
struct Populations {
    all: Tally,
    supervisor: Tally,
    user: Tally,
    touches_memory: Tally,
    registers_only: Tally,
    /// Cases that end in an address error, split out because the two corpora
    /// disagree about what one costs: `680x0` records six clocks of internal
    /// time and no cycle for the aborted access, `m68000` records ten and the
    /// aborted cycle as well, a difference of exactly eight clocks on every
    /// such case. Without this split that disagreement is invisible inside a
    /// single length rate.
    ///
    /// **M5 resolved it against the microcode, which charges the attempt.** So
    /// this population is exact against `m68000` and misses by a flat eight
    /// against `680x0`, and the delta histogram beside the table is what makes
    /// the difference between a disagreement and a defect readable.
    address_error: Tally,
    completed: Tally,
    /// Cases whose recorded trace makes at most one *data* transfer.
    ///
    /// This is the ceiling on rung 3 for any core whose executor is still
    /// atomic, and it is reported so the ceiling is on the record before the
    /// work rather than discovered after it. An atomic executor applies its
    /// whole effect on one clock, so it cannot put two data transfers on two
    /// different clocks without ceasing to be atomic. Where a case makes one or
    /// none, every other clock in it belongs to the prefetch queue, and a
    /// per-clock prefetch unit can place those with the executor untouched.
    ///
    /// So a per-clock queue in front of an atomic executor should drive this
    /// row towards 100% on rung 3 and leave the row below it near zero, and
    /// the aggregate should land near this row's share of the corpus. Coming
    /// out much above that would mean something is being credited that has not
    /// been built.
    at_most_one_data_txn: Tally,
    several_data_txns: Tally,
}

/// Whether a recorded case ends in an address error.
///
/// The `m68000` set says so directly with its `re`/`we` cycle kinds. The
/// `680x0` set has no such kind, so the signature there is the vector-3 fetch:
/// a pair of reads at 0x0C and 0x0E, which is the address-error vector and
/// nothing else in these corpora addresses.
fn is_address_error(tc: &M68000TestCase) -> bool {
    if tc.transactions.iter().any(|t| {
        matches!(
            t.kind,
            BusTxnKind::ReadAddressError | BusTxnKind::WriteAddressError
        )
    }) {
        return true;
    }
    let vector_word = |addr: u32| {
        tc.transactions
            .iter()
            .any(|t| t.kind == BusTxnKind::Read && t.byte_address() == addr)
    };
    vector_word(0x0C) && vector_word(0x0E)
}

impl Populations {
    fn add(&mut self, tc: &M68000TestCase, r: &CaseResult) {
        self.all.add(r);
        if tc.initial.is_supervisor() {
            self.supervisor.add(r);
        } else {
            self.user.add(r);
        }
        // A case whose recording has any transfer beyond its instruction
        // fetches reaches memory for an operand.
        let transfers = tc.transactions.iter().filter(|t| t.is_transfer()).count();
        let data_transfers = tc
            .transactions
            .iter()
            .filter(|t| t.is_transfer() && t.fc & 1 != 0)
            .count();
        if data_transfers > 0 || transfers > 2 {
            self.touches_memory.add(r);
        } else {
            self.registers_only.add(r);
        }
        if is_address_error(tc) {
            self.address_error.add(r);
        } else {
            self.completed.add(r);
        }
        if data_transfers <= 1 {
            self.at_most_one_data_txn.add(r);
        } else {
            self.several_data_txns.add(r);
        }
    }
}

fn report(label: &str, p: &Populations) {
    eprintln!("\n{label}");
    eprintln!(
        "  {:<16} {:>9} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7}",
        "population",
        "cases",
        "length",
        "kinds",
        "count",
        "clocks",
        "operand",
        "fc",
        "pc",
        "queue",
        "mean d",
        "timeout"
    );
    for (name, t) in [
        ("all", &p.all),
        ("supervisor", &p.supervisor),
        ("user", &p.user),
        ("touches memory", &p.touches_memory),
        ("registers only", &p.registers_only),
        ("address error", &p.address_error),
        ("completed", &p.completed),
        ("<=1 data txn", &p.at_most_one_data_txn),
        (">1 data txn", &p.several_data_txns),
    ] {
        eprintln!(
            "  {:<16} {:>9} {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% \
             {:>7.2}% {:>7.2}% {:>8.2} {:>7}",
            name,
            t.cases,
            t.length_pct(),
            t.kinds_pct(),
            t.count_pct(),
            t.positions_pct(),
            t.operands_pct(),
            t.fc_pct(),
            t.pc_pct(),
            t.prefetch_pct(),
            t.mean_delta(),
            t.timed_out()
        );
    }
    // Every transfer is four clocks with immediate DTACK, so `length` less four
    // per transfer is the instruction's internal time and cannot be negative.
    // If it ever is, the four-clock model is wrong for that case and the whole
    // basis for charging time from bus activity goes with it.
    eprintln!(
        "  cases whose recorded length is less than four clocks per transfer: {}",
        p.all.impossible_internal
    );
    eprintln!(
        "  cases that wrote over their own instruction stream, so the queue invariant \
         does not apply: {}",
        p.all.invariant_untestable
    );
    report_length_deltas("address error", &p.address_error);
}

/// How one population's clock misses are distributed, rather than averaged.
///
/// A mean over a population that is 57% exact says nothing about the 43%, and a
/// range says only where the ends are. This says how many cases sit at each
/// amount, which is the difference between "one constant, so a corpus
/// disagreement to resolve" and "a spread, so a mechanism to find".
fn report_length_deltas(name: &str, t: &Tally) {
    if t.ran == 0 {
        return;
    }
    let mut rows: Vec<_> = t.length_deltas.iter().map(|(d, n)| (*n, *d)).collect();
    rows.sort_by_key(|&(n, d)| (std::cmp::Reverse(n), d));
    let shown = rows.len().min(8);
    let listed: usize = rows[..shown].iter().map(|(n, _)| n).sum();
    let body = rows[..shown]
        .iter()
        .map(|(n, d)| format!("{d:+} x {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "  {name} clock deltas, commonest first: {body}{}",
        if shown < rows.len() {
            format!(" (+{} more values, {} cases)", rows.len() - shown, t.ran - listed)
        } else {
            String::new()
        }
    );
}

// ---------------------------------------------------------------------------
// The two corpora
// ---------------------------------------------------------------------------

/// The addressing mode an instruction's EA field names.
///
/// The opcode word is `initial.prefetch[0]`, by the invariant every case
/// asserts: the queue holds the word at PC, and the word at PC is the
/// instruction about to run. Taking it from there rather than from the case's
/// name is what makes this work on both suites, whose names are formatted
/// differently (`5e4a [ADD.w Q, A2] 1` against `001 STOP # 4e72`).
///
/// Bits 5..0 are where every instruction with an EA carries the field.
fn ea_mode_label(opcode: u16) -> &'static str {
    match ((opcode >> 3) & 7, opcode & 7) {
        (0, _) => "Dn",
        (1, _) => "An",
        (2, _) => "(An)",
        (3, _) => "(An)+",
        (4, _) => "-(An)",
        (5, _) => "(d16,An)",
        (6, _) => "(d8,An,Xn)",
        (7, 0) => "(xxx).w",
        (7, 1) => "(xxx).l",
        (7, 2) => "(d16,PC)",
        (7, 3) => "(d8,PC,Xn)",
        (7, 4) => "#imm",
        _ => "mode 7.5+",
    }
}

/// The group one case belongs to: the opcode's line, its bits 8..6, and its
/// addressing mode.
///
/// All three are needed, and each was added because leaving it out mixed
/// populations that have nothing to do with each other.
///
/// - **Bits 8..6** are the opmode on the ALU lines, which selects direction and
///   size; the destination mode on the `MOVE` lines; part of the sub-op on line
///   4. Without them `ADD.l`'s source form and its destination form share a row.
/// - **The line** is needed because a vector file is named for a *mnemonic
///   family*, not an encoding: `ADD.l.json.gz` contains line 5 `ADDQ.l` cases
///   as well as line D `ADD.l` ones, and those agree in bits 8..6 and 5..3 while
///   being different instructions. That collision is why `An op2` first read as
///   one group spanning -2 to +2 clocks: it was `ADD.l An,Dn` at -2 and
///   `ADDQ.l #,An` at +2, averaged into a mean of -0.03 that described neither.
///
/// What is deliberately *not* in the key is any register number. A group is a
/// shape of instruction, and if a residual ever splits by which register an
/// instruction names, that is worth finding out by other means rather than by
/// growing this key until every case is its own group.
fn case_group(opcode: u16) -> String {
    format!(
        "line {:X} op{} {}",
        opcode >> 12,
        (opcode >> 6) & 7,
        ea_mode_label(opcode)
    )
}

/// One vector file's rates, whole and split by addressing mode.
///
/// The split is the instrument M4 turns on. A row that is right on transfer
/// count and wrong on clock count has the right bus activity and the wrong
/// internal time, and the aggregate cannot say which of its addressing modes is
/// responsible: a mean of -0.82 clocks is not any mode being wrong by -0.82,
/// it is a *subset* of them being wrong by a whole number and the rest being
/// right. Grouping separates the two readings. A residual uniform across every
/// mode is a wrong constant; one that splits by something structural is a
/// missing mechanism, and only the second is worth a milestone.
#[derive(Default)]
struct FileTally {
    all: Tally,
    by_mode: std::collections::BTreeMap<String, Tally>,
    /// Up to a few named cases per group that missed on clock count, with the
    /// delta each missed by. A group's rate says how much is left and its range
    /// says whether it is one population; only a case name says what to read.
    examples: std::collections::BTreeMap<String, Vec<(String, i64)>>,
}

impl FileTally {
    fn add(&mut self, tc: &M68000TestCase, r: &CaseResult) {
        self.all.add(r);
        // Completed cases only. A case that ends in an address error spends
        // most of its length inside exception entry, which belongs to no
        // addressing mode, so counting one under its mode's row reports the
        // entry sequence's timing as if it were the mode's. That is not a
        // theoretical contamination: it is why `ADDA.l`'s `-(An)`, `(d8,An,Xn)`
        // and `(d8,PC,Xn)` rows first read about half exact with a mean of -1,
        // which is not any mode being wrong by one clock, it is two populations
        // averaged. Exception-entry timing is M5's and has its own split in the
        // population table.
        if !is_address_error(tc) {
            let group = case_group(tc.initial.prefetch[0]);
            self.by_mode.entry(group.clone()).or_default().add(r);
            if r.ran && !r.length_exact {
                let e = self.examples.entry(group).or_default();
                if e.len() < 3 {
                    e.push((tc.name.clone(), r.length_delta));
                }
            }
        }
    }
}

/// Per-file tallies, so the two suites can be set side by side and every row
/// named rather than described.
type FileRates = std::collections::BTreeMap<String, FileTally>;

/// Address-error cases grouped by the shape of the instruction that faulted.
///
/// The by-addressing-mode report deliberately excludes these, because an
/// aborted instruction spends most of its length inside exception entry and
/// counting that under a mode reports the entry sequence as if it were the
/// mode's. That exclusion left the faulting side with no breakdown at all: a
/// single rate and a single mean over 178,089 cases, which is how a residual
/// that is really two separate things reads as one blurred one. This is the
/// same key the completed side uses, applied to the population the completed
/// side throws away.
/// Each group keeps a few named cases per delta it missed by, because a group
/// that splits two ways cannot be read from its counts: the question is what
/// separates the halves, and only a case says.
type FaultGroups = std::collections::BTreeMap<String, (Tally, BTreeMap<i64, Vec<String>>)>;

fn note_fault_group(into: &mut FaultGroups, tc: &M68000TestCase, r: &CaseResult) {
    if !is_address_error(tc) {
        return;
    }
    let e = into
        .entry(case_group(tc.initial.prefetch[0]))
        .or_default();
    e.0.add(r);
    if r.ran {
        let names = e.1.entry(r.length_delta).or_default();
        if names.len() < 2 {
            names.push(tc.name.clone());
        }
    }
}

fn report_fault_groups(label: &str, groups: &FaultGroups) {
    let inexact: Vec<_> = groups
        .iter()
        .filter(|(_, (t, _))| t.length_exact < t.ran)
        .collect();
    if inexact.is_empty() {
        eprintln!("\n{label}: every address-error case is exact on clock count");
        return;
    }
    let missed: usize = inexact.iter().map(|(_, (t, _))| t.ran - t.length_exact).sum();
    eprintln!(
        "\n{label}: address-error clock residual by instruction shape, \
         {missed} cases in {} groups",
        inexact.len()
    );
    let mut rows: Vec<_> = inexact.into_iter().collect();
    rows.sort_by_key(|(_, (t, _))| std::cmp::Reverse(t.ran - t.length_exact));
    for (group, (t, names)) in rows.iter().take(20) {
        report_length_deltas(&format!("{group:<24} {:>6}", t.ran), t);
        for (delta, cases) in names.iter() {
            eprintln!("      {delta:+3}  e.g. {}", cases.join(" | "));
        }
    }
}

/// Transfer-sequence mismatches counted by (instruction, recorded shape, our
/// shape).
///
/// The residual this milestone leaves is a placement residual, and this is what
/// measures it: which sequences we still get in the wrong order, and how many
/// cases each accounts for. Reporting "some orders are wrong" instead would be
/// the mistake the ladder exists to prevent.
///
/// **The instruction is part of the key, and keying without it hid a residual
/// this milestone had been asked to fix.** A shape like eleven reads against
/// ten is one row of this map however many instructions reach it, and the row
/// was labelled with whichever vector file happened to be read first. `MOVEM.l`
/// sorts before `MOVE.w`, so every `MOVE` case sharing a shape with it was
/// counted under its name and the `MOVE` rows never appeared at all.
type ShapeMismatches = std::collections::BTreeMap<(String, String, String), (usize, String)>;

/// The first few cases per instruction whose final queue or PC disagrees.
///
/// A rate says how much is left; a case name says what to look at. This rung
/// is meant to reach 100%, so anything it reports is either a defect or a
/// convention difference that has to be named, and neither is diagnosable from
/// a percentage.
type QueueFailures = std::collections::BTreeMap<String, (usize, Vec<String>)>;

fn note_queue_failure(into: &mut QueueFailures, instr: &str, name: &str, r: &CaseResult) {
    if r.ran && r.prefetch_exact && r.pc_exact && r.invariant_holds {
        return;
    }
    let e = into.entry(instr.to_string()).or_default();
    e.0 += 1;
    if e.1.len() < 3 {
        e.1.push(name.to_string());
    }
}

fn report_queue_failures(label: &str, failures: &QueueFailures) {
    if failures.is_empty() {
        eprintln!("\n{label}: every case ends with the recorded PC and queue");
        return;
    }
    let total: usize = failures.values().map(|(n, _)| n).sum();
    eprintln!("\n{label}: {total} cases end with a different PC or queue");
    for (instr, (n, names)) in failures {
        eprintln!("  {instr:<14} {n:>6}  e.g. {}", names.join(" | "));
    }
}

/// Where the length table and the executor disagree about how many words an
/// instruction is, counted by opcode shape with an example.
///
/// Only completed cases are asked. An instruction that aborts on an address
/// error stops consuming words at the faulting access, so it legitimately takes
/// fewer than its encoding names, and counting those would bury the real
/// disagreements under a much larger population of correct ones.
type WordCountFaults = std::collections::BTreeMap<(String, u32, u32), (usize, String)>;

fn note_word_count(
    into: &mut WordCountFaults,
    instr: &str,
    name: &str,
    tc: &M68000TestCase,
    r: &CaseResult,
) {
    if !r.ran || is_address_error(tc) {
        return;
    }
    let (predicted, actual, what) = if r.words_consumed != r.words_predicted {
        (r.words_predicted, r.words_consumed, "words")
    } else if r.no_refill_actual != r.no_refill_predicted {
        (r.no_refill_predicted, r.no_refill_actual, "unrefilled")
    } else {
        return;
    };
    let e = into
        .entry((format!("{instr} {what}"), predicted, actual))
        .or_insert_with(|| (0, name.to_string()));
    e.0 += 1;
}

fn report_word_counts(label: &str, faults: &WordCountFaults) {
    if faults.is_empty() {
        eprintln!("\n{label}: the length table agrees with the executor on every completed case");
        return;
    }
    let total: usize = faults.values().map(|(n, _)| n).sum();
    eprintln!(
        "\n{label}: {total} completed cases where the length table and the executor disagree, \
         in {} shapes",
        faults.len()
    );
    let mut rows: Vec<_> = faults.iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for ((instr, predicted, actual), (n, example)) in rows.iter().take(25) {
        eprintln!(
            "  {instr:<14} {n:>7}  table says {predicted} words, executor took {actual}  e.g. {example}"
        );
    }
}

/// Rung 3's residual, counted by which clocks disagree, keyed by instruction
/// and by the pair of position lists.
type PositionFaults = std::collections::BTreeMap<(String, String, String), usize>;

fn note_position_fault(into: &mut PositionFaults, instr: &str, r: &CaseResult) {
    if let Some((recorded, ours)) = &r.position_fault {
        *into
            .entry((instr.to_string(), recorded.clone(), ours.clone()))
            .or_insert(0) += 1;
    }
}

/// Rung 3's residual counted by mechanism, split by whether the case ends in an
/// address error.
///
/// The split is not decoration. Half of this rung's residual on the
/// documentation-derived corpus is cases that fault, and those spend their
/// clocks inside exception entry, where a position is a statement about the
/// frame rather than about the instruction. Counting the two together lets a
/// change to one look like a change to the other.
type PositionClasses = std::collections::BTreeMap<(bool, &'static str), usize>;

fn note_position_class(into: &mut PositionClasses, faulted: bool, r: &CaseResult) {
    if let Some(class) = r.position_class {
        *into.entry((faulted, class)).or_insert(0) += 1;
    }
}

fn report_position_classes(label: &str, classes: &PositionClasses) {
    let total: usize = classes.values().sum();
    if total == 0 {
        eprintln!("\n{label}: no rung-3 miss to classify");
        return;
    }
    eprintln!("\n{label}: {total} rung-3 misses by mechanism");
    let mut rows: Vec<_> = classes.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for ((faulted, class), n) in rows {
        let where_ = if *faulted { "faults" } else { "completes" };
        eprintln!("  {n:>8}  {where_:<10} {class}");
    }
}

fn report_position_faults(label: &str, faults: &PositionFaults) {
    if faults.is_empty() {
        eprintln!("\n{label}: every matching transfer sequence is on the recorded clocks");
        return;
    }
    let total: usize = faults.values().sum();
    let mut rows: Vec<_> = faults.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!(
        "\n{label}: {total} cases whose transfers are in the right order on the wrong clocks, \
         in {} shapes",
        faults.len()
    );
    for ((instr, recorded, ours), n) in rows.iter().take(28) {
        eprintln!("  {n:>8}  {instr:<12} recorded [{recorded}]  ours [{ours}]");
    }
}

/// Rung 5's residual, counted by instruction and by the pair of codes.
type FcFaults = std::collections::BTreeMap<(String, String), (usize, String)>;

fn note_fc_fault(into: &mut FcFaults, instr: &str, name: &str, r: &CaseResult) {
    let Some(fault) = &r.fc_fault else {
        return;
    };
    // Key on the codes rather than the transfer index, so one mechanism is one
    // row however many different instructions reach it.
    let codes = fault
        .split_once("): ")
        .map_or(fault.clone(), |(_, rest)| rest.to_string());
    let e = into
        .entry((instr.to_string(), codes))
        .or_insert_with(|| (0, format!("{name}: {fault}")));
    e.0 += 1;
}

fn report_fc_faults(label: &str, faults: &FcFaults) {
    if faults.is_empty() {
        eprintln!("\n{label}: every matching transfer names the recorded address space");
        return;
    }
    let total: usize = faults.values().map(|(n, _)| n).sum();
    let mut rows: Vec<_> = faults.iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    eprintln!(
        "\n{label}: {total} cases naming the wrong address space, in {} shapes",
        faults.len()
    );
    for ((instr, codes), (n, example)) in rows.iter().take(12) {
        eprintln!("  {n:>7}  {instr:<12} {codes}  e.g. {example}");
    }
}

/// Rung 4's residual, counted by which field disagrees and on which
/// instruction, with an example transfer for each.
type OperandFaults = std::collections::BTreeMap<(String, String), (usize, String)>;

fn note_operand_fault(into: &mut OperandFaults, instr: &str, name: &str, r: &CaseResult) {
    let Some(fault) = &r.operand_fault else {
        return;
    };
    let field = fault.split(' ').next().unwrap_or("?").to_string();
    let e = into
        .entry((instr.to_string(), field))
        .or_insert_with(|| (0, format!("{name}: {fault}")));
    e.0 += 1;
}

fn report_operand_faults(label: &str, faults: &OperandFaults) {
    if faults.is_empty() {
        eprintln!("\n{label}: every matching transfer sequence agrees on address, size and data");
        return;
    }
    let total: usize = faults.values().map(|(n, _)| n).sum();
    // Grouped by field first, because the three fields fail for unrelated
    // reasons and a list ordered by volume would interleave them.
    let mut by_field: std::collections::BTreeMap<&str, Vec<(&String, usize, &String)>> =
        Default::default();
    for ((instr, field), (n, example)) in faults {
        by_field
            .entry(field.as_str())
            .or_default()
            .push((instr, *n, example));
    }
    eprintln!("\n{label}: {total} cases whose transfers agree in order but not in content");
    for (field, mut rows) in by_field {
        rows.sort_by_key(|(_, n, _)| std::cmp::Reverse(*n));
        let sum: usize = rows.iter().map(|(_, n, _)| n).sum();
        eprintln!("  {field}: {sum} cases across {} instructions", rows.len());
        for (instr, n, example) in rows.iter().take(8) {
            eprintln!("    {instr:<14} {n:>7}  e.g. {example}");
        }
    }
}

fn note_mismatch(into: &mut ShapeMismatches, instr: &str, name: &str, r: &CaseResult) {
    if let Some((recorded, ours)) = &r.mismatch {
        let e = into
            .entry((instr.to_string(), recorded.clone(), ours.clone()))
            .or_insert_with(|| (0, name.to_string()));
        e.0 += 1;
    }
}

/// Strip a vector file's name down to the instruction it covers, so the same
/// instruction can be found in both suites. `680x0` uses `ADD.b.json.gz` and
/// `m68000` uses `ADD.b.json.bin`.
/// Every vector file's rates, weakest first.
///
/// Enumerated rather than topped-and-tailed, deliberately. A "twelve weakest"
/// list answers which rows are worst and silently drops the question of how
/// many rows are imperfect at all, and a residual that has been described
/// rather than counted is the thing this ladder exists to prevent. The rows
/// below the perfect ones are the milestone's work list, and the count of
/// perfect ones is the part that has to keep not moving.
fn per_file_rows(label: &str, rates: &FileRates) {
    let mut rows: Vec<_> = rates.iter().map(|(name, f)| (name, &f.all)).collect();
    rows.sort_by(|a, b| {
        a.1.length_pct()
            .total_cmp(&b.1.length_pct())
            .then(a.1.kinds_pct().total_cmp(&b.1.kinds_pct()))
            .then(a.0.cmp(b.0))
    });
    let perfect = rows
        .iter()
        .filter(|(_, t)| t.length_exact == t.cases && t.kinds_exact == t.cases)
        .count();
    eprintln!(
        "\n{label}: every instruction, weakest first ({perfect} of {} exact on both length \
         and transfer order)",
        rows.len()
    );
    eprintln!(
        "  {:<14} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "instruction", "cases", "length", "kinds", "count", "clocks", "operand", "fc", "mean d"
    );
    for (name, t) in &rows {
        eprintln!(
            "  {:<14} {:>7} {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>+8.2}",
            name,
            t.cases,
            t.length_pct(),
            t.kinds_pct(),
            t.count_pct(),
            t.positions_pct(),
            t.operands_pct(),
            t.fc_pct(),
            t.mean_delta()
        );
    }
}

/// The clock-count residual of every imperfect row, grouped by addressing mode.
///
/// This is the reading M4's issue asks for before a constant is touched. A row
/// whose modes all miss by the same amount is one wrong number; a row where
/// some modes are exact and others miss by two clocks is a mechanism that only
/// some modes reach, and the mean over the row is an average of the two that
/// describes neither.
fn by_addressing_mode(label: &str, rates: &FileRates) {
    // A row is listed when any of its *completed* groups is imperfect. Judging
    // on the whole-row rate instead would list every row that only fails on its
    // address errors and then show it as all-exact underneath, which reads like
    // an instrument fault.
    let imperfect: Vec<_> = rates
        .iter()
        .filter(|(_, f)| f.by_mode.values().any(|t| t.length_exact != t.cases))
        .collect();
    eprintln!(
        "\n{label}: clock-count residual by addressing mode and opcode field, completed \
         cases only, for the {} rows with an imperfect group",
        imperfect.len()
    );
    for (name, f) in imperfect {
        let completed: usize = f.by_mode.values().map(|t| t.cases).sum();
        let exact: usize = f.by_mode.values().map(|t| t.length_exact).sum();
        eprintln!(
            "  {name}: {exact} of {completed} completed cases exact ({} counting its address \
             errors)",
            f.all.cases
        );
        let mut modes: Vec<_> = f.by_mode.iter().collect();
        modes.sort_by(|a, b| a.1.length_pct().total_cmp(&b.1.length_pct()));
        for (mode, t) in modes {
            // A group missing by one constant amount is a wrong number and can
            // be fixed by changing it. A group whose deltas span a range is a
            // mechanism, and changing a number would only move where its two
            // halves sit. The range is what tells them apart, so it is the
            // verdict rather than an extra column.
            let flag = match t.length_delta_range {
                _ if t.length_exact == t.cases => "exact".to_string(),
                Some((lo, hi)) if lo == hi => format!("all miss by {lo:+}"),
                Some((lo, hi)) => format!("spread {lo:+} to {hi:+}"),
                None => "no cases".to_string(),
            };
            eprintln!(
                "      {mode:<26} {:>6} cases  length {:>6.2}%  count {:>6.2}%  \
                 mean d {:>+6.2}  {flag}",
                t.cases,
                t.length_pct(),
                t.count_pct(),
                t.mean_delta()
            );
            if let Some(examples) = f.examples.get(mode).filter(|_| t.length_exact != t.cases) {
                for (name, delta) in examples {
                    eprintln!("          {delta:+4}  {name}");
                }
            }
        }
    }
}

/// The transfer-sequence residual, counted by shape pair.
fn residual_shapes(label: &str, shapes: &ShapeMismatches) {
    let mut rows: Vec<_> = shapes.iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    let total: usize = shapes.values().map(|(n, _)| n).sum();
    eprintln!(
        "\n{label}: transfer sequences that differ, {total} cases in {} shapes",
        shapes.len()
    );
    // With an example case, because a shape pair does not say which encoding
    // reached it and the answer is usually a fact about the addressing mode:
    // the same mnemonic runs different sequences for different source modes,
    // and reading a row without one invites fixing the wrong half.
    for ((instr, recorded, ours), (n, example)) in rows.iter().take(20) {
        eprintln!("  {n:>8}  {instr:<12} recorded {recorded:<22} ours {ours:<22} e.g. {example}");
    }
}

fn instruction_of(filename: &str) -> String {
    filename
        .trim_end_matches(".json.gz")
        .trim_end_matches(".json.bin")
        .to_string()
}

/// Everything a corpus run accumulates besides the population tallies: one
/// bundle per suite, so the two are never accidentally crossed.
#[derive(Default)]
struct Reports {
    rates: FileRates,
    shapes: ShapeMismatches,
    queue: QueueFailures,
    faults: OperandFaults,
    positions: PositionFaults,
    position_classes: PositionClasses,
    fcs: FcFaults,
    words: WordCountFaults,
    fault_groups: FaultGroups,
}

fn run_680x0(cpu: &mut M68000, bus: &mut RecordingBus68k, out: &mut Reports) -> Populations {
    let dir = phosphor_cpu_validation::vector_dir("680x0/68000/v1");
    let mut pops = Populations::default();

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the 680x0 vector directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "gz"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let gz = std::fs::read(entry.path()).expect("read a vector file");
        let mut json = String::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut json)
            .expect("decompress a vector file");
        let tests: Vec<M68000TestCase> = serde_json::from_str(&json).expect("parse a vector file");

        let instr = instruction_of(&name);
        let mut file_tally = FileTally::default();
        for tc in &tests {
            // This suite's pc is the execution point.
            let r = run_case(tc, tc.initial.pc, tc.final_state.pc, cpu, bus);
            pops.add(tc, &r);
            file_tally.add(tc, &r);
            note_mismatch(&mut out.shapes, &instr, &tc.name, &r);
            note_queue_failure(&mut out.queue, &instr, &tc.name, &r);
            note_operand_fault(&mut out.faults, &instr, &tc.name, &r);
            note_position_fault(&mut out.positions, &instr, &r);
            note_position_class(&mut out.position_classes, is_address_error(tc), &r);
            note_fc_fault(&mut out.fcs, &instr, &tc.name, &r);
            note_word_count(&mut out.words, &instr, &tc.name, tc, &r);
            note_fault_group(&mut out.fault_groups, tc, &r);
        }
        out.rates.insert(instr, file_tally);
    }

    pops
}

fn run_m68000(cpu: &mut M68000, bus: &mut RecordingBus68k, out: &mut Reports) -> Populations {
    let dir = phosphor_cpu_validation::vector_dir("m68000/v1");
    let mut pops = Populations::default();

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the m68000 vector directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "bin"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let tests = m68000_bin::decode_file(&entry.path()).expect("decode a vector file");

        let instr = instruction_of(&name);
        let mut file_tally = FileTally::default();
        for t in &tests {
            // This suite's pc leads the execution point by one prefetch, in the
            // final state as well as the initial one.
            let final_pc = t
                .case
                .final_state
                .pc
                .wrapping_sub(phosphor_cpu_validation::m68000_bin::PC_PREFETCH_LEAD);
            let r = run_case(&t.case, t.execution_pc(), final_pc, cpu, bus);
            pops.add(&t.case, &r);
            file_tally.add(&t.case, &r);
            note_mismatch(&mut out.shapes, &instr, &t.case.name, &r);
            note_queue_failure(&mut out.queue, &instr, &t.case.name, &r);
            note_operand_fault(&mut out.faults, &instr, &t.case.name, &r);
            note_position_fault(&mut out.positions, &instr, &r);
            note_position_class(&mut out.position_classes, is_address_error(&t.case), &r);
            note_fc_fault(&mut out.fcs, &instr, &t.case.name, &r);
            note_word_count(&mut out.words, &instr, &t.case.name, &t.case, &r);
            note_fault_group(&mut out.fault_groups, &t.case, &r);
        }
        out.rates.insert(instr, file_tally);
    }

    pops
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn test_m68000_cycle_gate() {
    let dir_680x0 = phosphor_cpu_validation::vector_dir("680x0/68000/v1");
    let dir_m68000 = phosphor_cpu_validation::vector_dir("m68000/v1");
    if !phosphor_cpu_validation::require_test_data(
        &dir_680x0,
        "run: git submodule update --init cpu-validation/test_data/680x0",
    ) {
        return;
    }
    if !phosphor_cpu_validation::require_test_data(
        &dir_m68000,
        "run: git submodule update --init cpu-validation/test_data/m68000",
    ) {
        return;
    }

    let mut cpu = M68000::new();
    let mut bus = RecordingBus68k::new();

    let mut out_680x0 = Reports::default();
    let mut out_m68000 = Reports::default();

    let pops_680x0 = run_680x0(&mut cpu, &mut bus, &mut out_680x0);
    let pops_m68000 = run_m68000(&mut cpu, &mut bus, &mut out_m68000);

    report("680x0 (documentation-derived)", &pops_680x0);
    report("m68000 (microcode-derived)", &pops_m68000);

    for (label, out) in [("680x0", &out_680x0), ("m68000", &out_m68000)] {
        per_file_rows(label, &out.rates);
        by_addressing_mode(label, &out.rates);
        report_fault_groups(label, &out.fault_groups);
        residual_shapes(label, &out.shapes);
        report_position_classes(label, &out.position_classes);
        report_position_faults(label, &out.positions);
        report_fc_faults(label, &out.fcs);
        report_operand_faults(label, &out.faults);
        report_word_counts(label, &out.words);
        report_queue_failures(label, &out.queue);
    }

    // Where our agreement rate against one suite differs materially from our
    // rate against the other for the same instruction, the two oracles are
    // saying different things about that instruction. The suites share no
    // cases, so this rate comparison is the only cross-set signal available.
    let mut divergent: Vec<(f64, String)> = Vec::new();
    for (instr, file_a) in &out_680x0.rates {
        let Some(file_b) = out_m68000.rates.get(instr) else {
            continue;
        };
        let (tally_a, tally_b) = (&file_a.all, &file_b.all);
        let (len_a, kinds_a, n_a) = (tally_a.length_pct(), tally_a.kinds_pct(), tally_a.cases);
        let (len_b, kinds_b, n_b) = (tally_b.length_pct(), tally_b.kinds_pct(), tally_b.cases);
        let gap = (len_a - len_b).abs().max((kinds_a - kinds_b).abs());
        if gap >= 5.0 {
            divergent.push((
                gap,
                format!(
                    "  {instr:<14} 680x0 len {len_a:6.2}% kinds {kinds_a:6.2}% (n={n_a})  \
                     m68000 len {len_b:6.2}% kinds {kinds_b:6.2}% (n={n_b})"
                ),
            ));
        }
    }
    divergent.sort_by(|a, b| b.0.total_cmp(&a.0));

    eprintln!(
        "\ninstructions where the two suites disagree by 5 points or more: {}",
        divergent.len()
    );
    for (_, line) in divergent.iter().take(30) {
        eprintln!("{line}");
    }

    let only_m68000: Vec<_> = out_m68000
        .rates
        .keys()
        .filter(|k| !out_680x0.rates.contains_key(*k))
        .cloned()
        .collect();
    eprintln!("\ncovered only by m68000: {}", only_m68000.join(", "));

    assert!(
        pops_680x0.all.cases > 0 && pops_m68000.all.cases > 0,
        "both corpora must contribute cases, or this gate measured nothing"
    );

    // A timeout is a case that never executed, so its zero is a harness fault
    // wearing a measurement's clothes. There is no reason for one here: every
    // 68000 instruction retires well inside the limit from a fresh CPU.
    let timeouts = pops_680x0.all.timed_out() + pops_m68000.all.timed_out();
    assert_eq!(
        timeouts, 0,
        "{timeouts} cases hit the {TICK_LIMIT}-tick limit; their rates are not measurements"
    );

    // --- The length table, against the executor, with no tolerance -----------
    //
    // `format::extension_words` is a second statement of something the
    // instruction bodies already know, which is the shape that drifts. It is
    // asserted here rather than reported because a per-clock loader will fetch
    // exactly what it says: a row that is wrong by one word would issue a bus
    // cycle the part never runs, or miss one it does, and the symptom would
    // surface as a timing residual a long way from the table.
    //
    // Both corpora, every case that completes. An instruction aborted by an
    // address error stops consuming at the faulting access and is excluded;
    // a privileged instruction in user mode consumes only its opcode, which
    // `predicted_words` accounts for.
    let word_faults: usize = out_680x0
        .words
        .values()
        .chain(out_m68000.words.values())
        .map(|(n, _)| n)
        .sum();
    assert_eq!(
        word_faults, 0,
        "{word_faults} completed cases where the length table and the executor disagree \
         about how many words the instruction is"
    );

    // --- The queue's own invariant, with no tolerance ------------------------
    //
    // The queue must hold the words memory holds at PC, on every case of both
    // corpora. Only one thing breaks this: a control transfer that loads PC
    // without discarding the queue, which leaves words from the old
    // instruction stream in it. That is the failure mode a comparison of
    // addresses cannot catch, so this is the check that stands in its place.
    let invariant_broken = pops_680x0.all.invariant_broken + pops_m68000.all.invariant_broken;
    assert_eq!(
        invariant_broken, 0,
        "{invariant_broken} cases ended with a prefetch queue that is not the memory at PC, \
         which means a PC was loaded without flushing"
    );

    // --- Rung 6: the recorded final queue, with no tolerance where it can be -
    //
    // Every one of the 680x0 set's 1,000,060 cases must end with the PC and the
    // two queue words the recording ends with.
    assert_eq!(
        pops_680x0.all.prefetch_exact,
        pops_680x0.all.cases,
        "680x0: {} cases end with a different PC or prefetch queue",
        pops_680x0.all.cases - pops_680x0.all.prefetch_exact
    );

    // The m68000 set has one instruction this cannot hold for, and it is a
    // difference of convention rather than of behavior. Its `STOP` cases record
    // the part frozen mid-instruction: PC and the queue are exactly as they were
    // before the instruction, with the immediate word already loaded into SR.
    // This core retires STOP, so its PC is past the immediate. Asserting the
    // *set of instructions* rather than a rate is what keeps this from becoming
    // a tolerance that hides the next regression.
    let unexpected: Vec<&String> = out_m68000.queue.keys().filter(|k| *k != "STOP").collect();
    assert!(
        unexpected.is_empty(),
        "m68000: instructions other than STOP end with a different PC or queue: {unexpected:?}"
    );

    // --- Rung 1 on the faulting path, asserted rather than floored -----------
    //
    // Every case that ends in an address error is exact on clock count against
    // the microcode-derived corpus, all 55,607 of them, and the delta histogram
    // beside the population table is what says so: one bucket at zero rather
    // than a rate that rounds to 100. An equality assertion is safe for that
    // reason and is the stronger check, because this population is one
    // mechanism and a single case sliding off it is a defect rather than a
    // residual. Watched failing twice on the way here, at 0 of 55,607 and then
    // at 31,879.
    //
    // **The other corpus reads 2 of 178,089 on the same population and that is
    // not a regression.** The two disagree about what the access that faults
    // costs, by exactly eight clocks on every case, and the microcode settles
    // it: see `ABORTED_ACCESS_CLOCKS`. The histogram is what keeps that
    // readable, because a single bucket at +8 is a corpus disagreement and a
    // spread would be a defect. The two exact cases are `MOVEM.l` loads whose
    // finish saturates past the replay cap.
    assert_eq!(
        pops_m68000.address_error.length_exact, pops_m68000.address_error.ran,
        "m68000: {} address-error cases disagree on clock count",
        pops_m68000.address_error.ran - pops_m68000.address_error.length_exact
    );

    // --- Floors that ratchet -------------------------------------------------
    //
    // Set to what this milestone measured, so any regression fails and any
    // improvement is a deliberate edit here. The residual behind each is named
    // in the milestone's issue comment: TAS's indivisible cycle, MOVEM's
    // trailing read, the mul/div data-dependent timing, and exception entry,
    // all of which are M5. Each is the measured rate rounded *down* to two
    // places: the reported figure is rounded to nearest, so a floor set from it
    // fails against the run it was taken from.
    //
    // **Rungs 3, 4 and 5 have floors from M4 on.** A rung that is reported but
    // not floored can slide back to where it started without failing anything,
    // and rung 3 is the rung this conversion exists to move. Its population
    // splits are floored too: the aggregate can be held up by the cases that
    // touch no memory, which were already near-exact before any of this, so a
    // floor on the aggregate alone would not notice the operand path regressing.
    let floors = [
        // **The one floor in this conversion that has come down.** It stood at
        // 96.80 for the length of one commit, with the faulting population
        // exact against this corpus. Charging the aborted access what the
        // microcode charges it takes that population to zero here and to exact
        // against the other corpus, and the two cannot both be satisfied: the
        // disagreement is a constant eight clocks and the microcode settles it.
        // Lowered deliberately and labeled, rather than the change being
        // declined to keep a number up. See `ABORTED_ACCESS_CLOCKS`.
        ("680x0 length", pops_680x0.all.length_pct(), 78.99),
        ("680x0 kinds", pops_680x0.all.kinds_pct(), 98.58),
        ("680x0 count", pops_680x0.all.count_pct(), 98.86),
        ("680x0 positions", pops_680x0.all.positions_pct(), 78.28),
        (
            "680x0 positions, completed",
            pops_680x0.completed.positions_pct(),
            95.24,
        ),
        (
            "680x0 positions, >1 data transaction",
            pops_680x0.several_data_txns.positions_pct(),
            57.99,
        ),
        ("680x0 operands", pops_680x0.all.operands_pct(), 79.97),
        ("680x0 function codes", pops_680x0.all.fc_pct(), 98.13),
        // The faulting path's transfer *sequence*, which M4 finished. One case
        // of 178,089 still differs and it is a `MOVEM`, whose trailing read is
        // the residual M3 named and M5 owns. Floored rather than asserted
        // equal, because the report prints this as 100.00% and it is 99.9994%:
        // a rate reaches its printed ceiling before it reaches its real one,
        // and an assertion of 100.0 fails against the run it was taken from.
        // What is left on this path is entirely *when* the transfers happen,
        // which is exception entry.
        (
            "680x0 kinds, address error",
            pops_680x0.address_error.kinds_pct(),
            99.99,
        ),
        (
            "m68000 kinds, address error",
            pops_m68000.address_error.kinds_pct(),
            99.99,
        ),
        ("m68000 length", pops_m68000.all.length_pct(), 96.90),
        ("m68000 kinds", pops_m68000.all.kinds_pct(), 98.22),
        ("m68000 count", pops_m68000.all.count_pct(), 99.14),
        ("m68000 positions", pops_m68000.all.positions_pct(), 95.48),
        // **The number M5's exception-entry work exists to move**, and it had
        // no floor because it had no value: a structural 0.00% while entry
        // drove all eleven of its cycles on one clock. It is floored against
        // the microcode-derived corpus alone, because the other one omits the
        // eight clocks the aborted access costs and so puts every transfer
        // behind the fault eight clocks earlier than this core does. That is
        // the corpus disagreement, not a placement error, and floors on the
        // 680x0 faulting population would measure it rather than this core.
        (
            "m68000 positions, address error",
            pops_m68000.address_error.positions_pct(),
            97.14,
        ),
        (
            "m68000 positions, completed",
            pops_m68000.completed.positions_pct(),
            95.13,
        ),
        (
            "m68000 positions, >1 data transaction",
            pops_m68000.several_data_txns.positions_pct(),
            93.51,
        ),
        ("m68000 operands", pops_m68000.all.operands_pct(), 80.60),
        ("m68000 function codes", pops_m68000.all.fc_pct(), 98.02),
    ];
    for (name, actual, floor) in floors {
        assert!(
            actual >= floor,
            "{name} fell to {actual:.2}%, below the {floor:.2}% floor this milestone set"
        );
    }
}
