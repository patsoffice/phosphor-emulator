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
    /// What the recording's own clocks say the instruction spent away from the
    /// bus: `length` less four clocks for each transfer it ran.
    recorded_internal: i64,
    /// The case could not be run at all (it hit the tick limit).
    ran: bool,
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
    cpu.pc = execution_pc;

    for &(addr, val) in &st.ram {
        let a = addr & ADDR_MASK;
        bus.memory[a as usize] = val;
        loaded.push(a);
    }
    // The instruction stream lives in the prefetch queue and is not necessarily
    // present in ram[]: place the two queued words where they are fetched from.
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

fn run_case(
    tc: &M68000TestCase,
    execution_pc: u32,
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
        CaseResult {
            length_exact: ticks == tc.length,
            length_delta: ticks as i64 - tc.length as i64,
            kinds_exact: ours == recorded,
            count_exact: ours.len() == recorded.len(),
            count_delta: ours.len() as i64 - recorded.len() as i64,
            recorded_internal,
            ran: true,
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
    }
}

fn report(label: &str, p: &Populations) {
    eprintln!("\n{label}");
    eprintln!(
        "  {:<16} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "population", "cases", "length", "kinds", "count", "mean d", "mean dN", "timeout"
    );
    for (name, t) in [
        ("all", &p.all),
        ("supervisor", &p.supervisor),
        ("user", &p.user),
        ("touches memory", &p.touches_memory),
        ("registers only", &p.registers_only),
    ] {
        eprintln!(
            "  {:<16} {:>9} {:>8.2}% {:>8.2}% {:>8.2}% {:>9.2} {:>9.2} {:>8}",
            name,
            t.cases,
            t.length_pct(),
            t.kinds_pct(),
            t.count_pct(),
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
}

// ---------------------------------------------------------------------------
// The two corpora
// ---------------------------------------------------------------------------

/// Per-file rates, so the two suites can be set side by side.
type FileRates = std::collections::BTreeMap<String, (f64, f64, usize)>;

/// Strip a vector file's name down to the instruction it covers, so the same
/// instruction can be found in both suites. `680x0` uses `ADD.b.json.gz` and
/// `m68000` uses `ADD.b.json.bin`.
fn instruction_of(filename: &str) -> String {
    filename
        .trim_end_matches(".json.gz")
        .trim_end_matches(".json.bin")
        .to_string()
}

fn run_680x0(cpu: &mut M68000, bus: &mut RecordingBus68k, rates: &mut FileRates) -> Populations {
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

        let mut file_tally = Tally::default();
        for tc in &tests {
            // This suite's pc is the execution point.
            let r = run_case(tc, tc.initial.pc, cpu, bus);
            pops.add(tc, &r);
            file_tally.add(&r);
        }
        rates.insert(
            instruction_of(&name),
            (
                file_tally.length_pct(),
                file_tally.kinds_pct(),
                file_tally.cases,
            ),
        );
    }

    pops
}

fn run_m68000(cpu: &mut M68000, bus: &mut RecordingBus68k, rates: &mut FileRates) -> Populations {
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

        let mut file_tally = Tally::default();
        for t in &tests {
            // This suite's pc leads the execution point by one prefetch.
            let r = run_case(&t.case, t.execution_pc(), cpu, bus);
            pops.add(&t.case, &r);
            file_tally.add(&r);
        }
        rates.insert(
            instruction_of(&name),
            (
                file_tally.length_pct(),
                file_tally.kinds_pct(),
                file_tally.cases,
            ),
        );
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

    let pops_680x0 = run_680x0(&mut cpu, &mut bus, &mut rates_680x0);
    let pops_m68000 = run_m68000(&mut cpu, &mut bus, &mut rates_m68000);

    report("680x0 (documentation-derived)", &pops_680x0);
    report("m68000 (microcode-derived)", &pops_m68000);

    // Where our agreement rate against one suite differs materially from our
    // rate against the other for the same instruction, the two oracles are
    // saying different things about that instruction. The suites share no
    // cases, so this rate comparison is the only cross-set signal available.
    let mut divergent: Vec<(f64, String)> = Vec::new();
    for (instr, &(len_a, kinds_a, n_a)) in &rates_680x0 {
        let Some(&(len_b, kinds_b, n_b)) = rates_m68000.get(instr) else {
            continue;
        };
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

    // Nothing is asserted yet. The design doc's Decision 3 calls for a floor
    // that ratchets, and a floor set before the conversion starts would only
    // pin how wrong an atomic core is. It goes in once the rows are done.
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
}
