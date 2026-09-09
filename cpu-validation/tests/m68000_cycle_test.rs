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
//! # Two rungs, and why only two
//!
//! The design doc's ladder has six rungs, widening from a total to a full
//! positional comparison. Only the first two can mean anything against an
//! atomic core:
//!
//! 1. **`length`**: our clock count against the recording's.
//! 2. **Transfer kinds and order**: reads and writes in sequence, durations and
//!    positions ignored.
//!
//! Rungs 3 to 6 compare *when* a cycle runs, and an atomic core has no answer.
//! Reporting them now would produce a number that says "0%" for a structural
//! reason rather than a timing one, which reads like a measurement and is not.
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

use std::io::Read;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::M68000;
use phosphor_cpu_validation::{
    BusTxnKind, M68000Regs, M68000TestCase, OurAccess, RecordingBus68k, m68000_bin,
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
    /// Whether we ran the same *number* of transfers, order aside.
    count_exact: bool,
    /// Ours minus the recording, in transfers.
    count_delta: i64,
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

/// The recorded transfer sequence, as read/write kinds in order.
///
/// Idle spans drop out (an atomic core has none to compare) and so do the two
/// address-error kinds, which are cycles the part runs with AS never asserted:
/// no transfer is committed, and this core does not run them at all.
fn recorded_kinds(tc: &M68000TestCase) -> Vec<bool> {
    tc.transactions
        .iter()
        .filter_map(|t| match t.kind {
            BusTxnKind::Read | BusTxnKind::Tas => Some(false),
            BusTxnKind::Write => Some(true),
            BusTxnKind::Idle | BusTxnKind::ReadAddressError | BusTxnKind::WriteAddressError => None,
        })
        .collect()
}

fn our_kinds(log: &[OurAccess]) -> Vec<bool> {
    log.iter().map(|a| a.write).collect()
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
    let mut ticks: u32 = 0;
    let mut timed_out = false;
    loop {
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
        let recorded = recorded_kinds(tc);
        let ours = our_kinds(&bus.log);
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
            kinds_exact: ours == recorded,
            count_exact: ours.len() == recorded.len(),
            count_delta: ours.len() as i64 - recorded.len() as i64,
            pc_exact: cpu.pc() == final_pc,
            prefetch_exact: queue_len == 2 && queue == tc.final_state.prefetch,
            invariant_holds,
            stream_written,
            recorded_internal,
            ran: true,
            mismatch: (ours != recorded).then(|| (shape_of(&recorded), shape_of(&ours))),
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
    count_exact: usize,
    pc_exact: usize,
    prefetch_exact: usize,
    /// Cases where the queue did not hold the words at PC. Must be zero.
    invariant_broken: usize,
    /// Cases the invariant cannot be applied to, because the instruction wrote
    /// over its own instruction stream.
    invariant_untestable: usize,
    length_delta_sum: i64,
    count_delta_sum: i64,
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
        self.count_delta_sum += r.count_delta;
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

    fn count_pct(&self) -> f64 {
        Self::pct(self.count_exact, self.cases)
    }

    fn pc_pct(&self) -> f64 {
        Self::pct(self.pc_exact, self.cases)
    }

    fn prefetch_pct(&self) -> f64 {
        Self::pct(self.prefetch_exact, self.cases)
    }

    fn mean_count_delta(&self) -> f64 {
        if self.ran == 0 {
            0.0
        } else {
            self.count_delta_sum as f64 / self.ran as f64
        }
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
    address_error: Tally,
    completed: Tally,
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
    }
}

fn report(label: &str, p: &Populations) {
    eprintln!("\n{label}");
    eprintln!(
        "  {:<16} {:>9} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7}",
        "population",
        "cases",
        "length",
        "kinds",
        "count",
        "pc",
        "queue",
        "mean d",
        "mean dN",
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
    ] {
        eprintln!(
            "  {:<16} {:>9} {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>8.2} {:>8.2} {:>7}",
            name,
            t.cases,
            t.length_pct(),
            t.kinds_pct(),
            t.count_pct(),
            t.pc_pct(),
            t.prefetch_pct(),
            t.mean_delta(),
            t.mean_count_delta(),
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
}

// ---------------------------------------------------------------------------
// The two corpora
// ---------------------------------------------------------------------------

/// Per-file tallies, so the two suites can be set side by side and the weakest
/// rows named rather than described.
type FileRates = std::collections::BTreeMap<String, Tally>;

/// Transfer-sequence mismatches counted by (recorded shape, our shape).
///
/// The residual this milestone leaves is a placement residual, and this is what
/// measures it: which sequences we still get in the wrong order, and how many
/// cases each accounts for. Reporting "some orders are wrong" instead would be
/// the mistake the ladder exists to prevent.
type ShapeMismatches = std::collections::BTreeMap<(String, String), (usize, String)>;

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

fn note_mismatch(into: &mut ShapeMismatches, instr: &str, r: &CaseResult) {
    if let Some((recorded, ours)) = &r.mismatch {
        let e = into
            .entry((recorded.clone(), ours.clone()))
            .or_insert_with(|| (0, instr.to_string()));
        e.0 += 1;
    }
}

/// Strip a vector file's name down to the instruction it covers, so the same
/// instruction can be found in both suites. `680x0` uses `ADD.b.json.gz` and
/// `m68000` uses `ADD.b.json.bin`.
/// The instructions this core agrees with least, so a residual is named.
///
/// Reported on transfer order and on clock count separately, because they fail
/// for different reasons: a wrong order is a placement error inside an
/// instruction whose bus activity is right, and a wrong length with a right
/// count is an internal time that does not match.
fn weakest_rows(label: &str, rates: &FileRates) {
    let mut rows: Vec<_> = rates
        .iter()
        .map(|(name, t)| {
            (
                t.kinds_pct(),
                t.count_pct(),
                t.prefetch_pct(),
                t.length_pct(),
                t.mean_delta(),
                name,
            )
        })
        .collect();

    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    eprintln!("\n{label}: the twelve weakest instructions on transfer order");
    for (kinds, count, queue, _, _, name) in rows.iter().take(12) {
        eprintln!("  {name:<14} kinds {kinds:6.2}%  count {count:6.2}%  queue {queue:6.2}%");
    }

    rows.sort_by(|a, b| a.3.total_cmp(&b.3));
    eprintln!("\n{label}: the twelve weakest instructions on clock count");
    for (_, count, _, length, mean, name) in rows.iter().take(12) {
        eprintln!("  {name:<14} length {length:6.2}%  count {count:6.2}%  mean d {mean:+6.2}");
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
    for ((recorded, ours), (n, example)) in rows.iter().take(15) {
        eprintln!("  {n:>8}  recorded {recorded:<22} ours {ours:<22} e.g. {example}");
    }
}

fn instruction_of(filename: &str) -> String {
    filename
        .trim_end_matches(".json.gz")
        .trim_end_matches(".json.bin")
        .to_string()
}

fn run_680x0(
    cpu: &mut M68000,
    bus: &mut RecordingBus68k,
    rates: &mut FileRates,
    shapes: &mut ShapeMismatches,
    queue_failures: &mut QueueFailures,
) -> Populations {
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
        let mut file_tally = Tally::default();
        for tc in &tests {
            // This suite's pc is the execution point.
            let r = run_case(tc, tc.initial.pc, tc.final_state.pc, cpu, bus);
            pops.add(tc, &r);
            file_tally.add(&r);
            note_mismatch(shapes, &instr, &r);
            note_queue_failure(queue_failures, &instr, &tc.name, &r);
        }
        rates.insert(instr, file_tally);
    }

    pops
}

fn run_m68000(
    cpu: &mut M68000,
    bus: &mut RecordingBus68k,
    rates: &mut FileRates,
    shapes: &mut ShapeMismatches,
    queue_failures: &mut QueueFailures,
) -> Populations {
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
        let mut file_tally = Tally::default();
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
            file_tally.add(&r);
            note_mismatch(shapes, &instr, &r);
            note_queue_failure(queue_failures, &instr, &t.case.name, &r);
        }
        rates.insert(instr, file_tally);
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

    let mut rates_680x0 = FileRates::new();
    let mut rates_m68000 = FileRates::new();
    let mut shapes_680x0 = ShapeMismatches::new();
    let mut shapes_m68000 = ShapeMismatches::new();
    let mut queue_680x0 = QueueFailures::new();
    let mut queue_m68000 = QueueFailures::new();

    let pops_680x0 = run_680x0(
        &mut cpu,
        &mut bus,
        &mut rates_680x0,
        &mut shapes_680x0,
        &mut queue_680x0,
    );
    let pops_m68000 = run_m68000(
        &mut cpu,
        &mut bus,
        &mut rates_m68000,
        &mut shapes_m68000,
        &mut queue_m68000,
    );

    report("680x0 (documentation-derived)", &pops_680x0);
    report("m68000 (microcode-derived)", &pops_m68000);

    for (label, rates, shapes, queue) in [
        ("680x0", &rates_680x0, &shapes_680x0, &queue_680x0),
        ("m68000", &rates_m68000, &shapes_m68000, &queue_m68000),
    ] {
        weakest_rows(label, rates);
        residual_shapes(label, shapes);
        report_queue_failures(label, queue);
    }

    // Where our agreement rate against one suite differs materially from our
    // rate against the other for the same instruction, the two oracles are
    // saying different things about that instruction. The suites share no
    // cases, so this rate comparison is the only cross-set signal available.
    let mut divergent: Vec<(f64, String)> = Vec::new();
    for (instr, tally_a) in &rates_680x0 {
        let Some(tally_b) = rates_m68000.get(instr) else {
            continue;
        };
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

    let only_m68000: Vec<_> = rates_m68000
        .keys()
        .filter(|k| !rates_680x0.contains_key(*k))
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
    let unexpected: Vec<&String> = queue_m68000.keys().filter(|k| *k != "STOP").collect();
    assert!(
        unexpected.is_empty(),
        "m68000: instructions other than STOP end with a different PC or queue: {unexpected:?}"
    );

    // --- Floors that ratchet -------------------------------------------------
    //
    // Set to what this milestone measured, so any regression fails and any
    // improvement is a deliberate edit here. They are floors on rungs 1 and 2,
    // and the residual behind each is named in the milestone's issue comment:
    // TAS's indivisible cycle, MOVEM's trailing read, the mul/div data-dependent
    // timing, and exception-entry internal time, all of which are M5.
    // Each is the measured rate rounded *down* to two places: the reported
    // figure is rounded to nearest, so a floor set from it fails against the
    // run it was taken from.
    let floors = [
        ("680x0 length", pops_680x0.all.length_pct(), 88.33),
        ("680x0 kinds", pops_680x0.all.kinds_pct(), 98.50),
        ("680x0 count", pops_680x0.all.count_pct(), 98.86),
        ("m68000 length", pops_m68000.all.length_pct(), 78.65),
        ("m68000 kinds", pops_m68000.all.kinds_pct(), 98.13),
        ("m68000 count", pops_m68000.all.count_pct(), 99.14),
    ];
    for (name, actual, floor) in floors {
        assert!(
            actual >= floor,
            "{name} fell to {actual:.2}%, below the {floor:.2}% floor this milestone set"
        );
    }
}
