use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::{Bus, BusMaster};
use serde::{Deserialize, Serialize};

pub mod m68000_bin;

// --- Test-data availability ---

/// Setting this to anything turns a missing vector directory from a skip into a
/// panic. CI's validation job sets it.
pub const REQUIRE_VECTORS_ENV: &str = "PHOSPHOR_REQUIRE_VECTORS";

/// Where a validator's vectors live, resolved against this crate's root rather
/// than the current directory.
///
/// `relative` is the path under `cpu-validation/test_data/`, e.g. `"m6800"` or
/// `"65x02/6502/v1"`.
///
/// It has to be absolute, because the two halves of this crate run from
/// different directories. Cargo runs an integration test with the current
/// directory set to the crate root, so `Path::new("test_data/m6800")` resolved
/// there and found the vectors. It runs a *binary* with the current directory
/// wherever the user invoked cargo, so the generators resolved the same literal
/// against the repo root and wrote the vectors one level too high.
///
/// The command the validators print on a skip is
/// `cargo run -p phosphor-cpu-validation --bin gen_m6800_tests -- all`, which
/// is run from the repo root by anyone reading it, and the failure was silent
/// in the worst way: the generator reported success, the validator then found
/// nothing and skipped, and libtest hides a skip message for a passing test. A
/// green suite that had validated nothing. `CARGO_MANIFEST_DIR` is fixed at
/// compile time and is the same for both halves, so the literal cannot mean two
/// places again.
pub fn vector_dir(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_data")
        .join(relative)
}

/// Check for a validator's vector directory, reporting how to obtain it when
/// it is missing.
///
/// Returns `true` when `dir` exists and the caller should run. Every vector
/// directory lives under the gitignored `cpu-validation/test_data/` — the
/// SingleStepTests suites are git submodules and the M6800/M6809 sets are
/// generated — so a fresh checkout has none of them. Panicking there would
/// make a clean clone (and CI) fail for an environment reason rather than a
/// code one, so the validators skip instead and say what to run.
///
/// **A skip is green and quiet**, because libtest captures stderr for a passing
/// test, so the suite reports success while validating nothing. That is a real
/// hazard rather than a note: it is the same defect shape as a check whose two
/// sides are both silent. Set [`REQUIRE_VECTORS_ENV`] where the data is
/// supposed to be present and the skip becomes a failure that names the
/// directory. CI's validation job sets it, which is what makes that job's green
/// mean something; treat a permanently-skipping validator as an unvalidated CPU.
pub fn require_test_data(dir: &std::path::Path, how_to_obtain: &str) -> bool {
    vectors_available(
        dir,
        how_to_obtain,
        std::env::var_os(REQUIRE_VECTORS_ENV).is_some(),
    )
}

/// The decision behind [`require_test_data`], with the environment lifted into
/// an argument.
///
/// Split out so the guard can be tested without `set_var`, which is unsafe in
/// this edition and process-global besides, so a test using it would race every
/// other test in the binary.
fn vectors_available(dir: &std::path::Path, how_to_obtain: &str, required: bool) -> bool {
    if dir.exists() {
        return true;
    }
    if required {
        panic!(
            "no vectors at {} — {how_to_obtain}\n{REQUIRE_VECTORS_ENV} is set, \
             so this is a failure rather than a skip: something was supposed to \
             have put them there.",
            dir.display()
        );
    }
    eprintln!(
        "skipping: no vectors at {} — {how_to_obtain}",
        dir.display()
    );
    false
}

// --- Vector suite harness ---

/// Mismatches found while replaying one test case.
///
/// Collected rather than asserted so that a case reports every field that moved
/// instead of only the first, which is the difference between "CC and the last
/// two bus cycles" and three separate debugging rounds.
#[derive(Default)]
pub struct Mismatches(Vec<String>);

impl Mismatches {
    /// Record `actual` against `expected`, naming the field.
    pub fn check<T: PartialEq + std::fmt::Debug>(
        &mut self,
        actual: T,
        expected: T,
        what: std::fmt::Arguments<'_>,
    ) {
        if actual != expected {
            self.0
                .push(format!("{what}: got {actual:?} expected {expected:?}"));
        }
    }

    /// `None` when nothing mismatched, otherwise one line naming the case.
    pub fn into_report(self, case_name: &str) -> Option<String> {
        if self.0.is_empty() {
            None
        } else {
            Some(format!("{case_name}: {}", self.0.join("; ")))
        }
    }
}

/// Replay every vector in a suite directory, reporting failures per opcode file.
///
/// `run_case` returns `None` for a pass and a description of the mismatches for
/// a failure. Collecting rather than panicking on the first one is what keeps a
/// single bad opcode from hiding every opcode that sorts after it, which matters
/// when a suite is hundreds of files.
///
/// Returns without running anything when the vectors are absent and optional.
/// See [`require_test_data`] for why that is a skip rather than a failure, and
/// for the flag that turns it into one.
pub fn run_vector_suite<T, F>(suite: &str, how_to_obtain: &str, mut run_case: F)
where
    T: serde::de::DeserializeOwned,
    F: FnMut(&T) -> Option<String>,
{
    /// Failing cases quoted per opcode file; the rest are counted only.
    const EXAMPLES_PER_FILE: usize = 3;

    let dir = vector_dir(suite);
    if !require_test_data(&dir, how_to_obtain) {
        return;
    }

    let mut json_files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension().and_then(|e| e.to_str()) == Some("json")).then_some(path)
        })
        .collect();
    json_files.sort();

    // A directory that exists but holds no vectors would otherwise pass here
    // for the same reason a missing one used to: nothing ran, and nothing said so.
    assert!(
        !json_files.is_empty(),
        "no JSON vectors in {}: {how_to_obtain}",
        dir.display()
    );

    let mut total_cases = 0;
    // (opcode file stem, failed, total, first few descriptions)
    let mut failing: Vec<(String, usize, usize, Vec<String>)> = Vec::new();

    for path in &json_files {
        let json = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let cases: Vec<T> = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
        assert!(!cases.is_empty(), "{} holds no cases", path.display());

        let mut failed = 0;
        let mut examples = Vec::new();
        for case in &cases {
            if let Some(msg) = run_case(case) {
                failed += 1;
                if examples.len() < EXAMPLES_PER_FILE {
                    examples.push(msg);
                }
            }
        }
        if failed > 0 {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            failing.push((stem, failed, cases.len(), examples));
        }
        total_cases += cases.len();
    }

    if !failing.is_empty() {
        let failed_cases: usize = failing.iter().map(|(_, n, _, _)| n).sum();
        let mut report = format!(
            "{} of {} opcode files failed ({failed_cases} of {total_cases} cases):\n",
            failing.len(),
            json_files.len(),
        );
        for (stem, failed, total, examples) in &failing {
            report.push_str(&format!("\n  {stem}: {failed}/{total} failed\n"));
            for example in examples {
                report.push_str(&format!("    {example}\n"));
            }
            if *failed > examples.len() {
                report.push_str(&format!("    ... and {} more\n", failed - examples.len()));
            }
        }
        panic!("{report}");
    }

    eprintln!(
        "Validated {total_cases} tests across {} opcode files",
        json_files.len()
    );
}

// --- TracingBus: flat 64KB memory with cycle-by-cycle recording ---

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BusOp {
    Read,
    Write,
    Internal,
}

#[derive(Clone, Debug)]
pub struct BusCycle {
    pub addr: u16,
    pub data: u8,
    pub op: BusOp,
}

pub struct TracingBus {
    pub memory: [u8; 0x10000],
    pub cycles: Vec<BusCycle>,
    /// Queue of (port_addr, data, direction) for I/O port reads/writes.
    /// Populated from test case `ports` field; io_read pops 'r' entries.
    pub port_queue: Vec<(u16, u8, char)>,
    pub port_index: usize,
}

impl TracingBus {
    pub fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            cycles: Vec::new(),
            port_queue: Vec::new(),
            port_index: 0,
        }
    }

    pub fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }

    pub fn clear_cycles(&mut self) {
        self.cycles.clear();
    }
}

impl Default for TracingBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TracingBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        let data = self.memory[addr as usize];
        self.cycles.push(BusCycle {
            addr,
            data,
            op: BusOp::Read,
        });
        data
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.memory[addr as usize] = data;
        self.cycles.push(BusCycle {
            addr,
            data,
            op: BusOp::Write,
        });
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        // Return next port read value from the queue
        while self.port_index < self.port_queue.len() {
            let (_, data, dir) = self.port_queue[self.port_index];
            self.port_index += 1;
            if dir == 'r' {
                return data;
            }
        }
        0xFF // fallback
    }

    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {
        // Advance past the next 'w' entry in the port queue
        while self.port_index < self.port_queue.len() {
            let (_, _, dir) = self.port_queue[self.port_index];
            self.port_index += 1;
            if dir == 'w' {
                return;
            }
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// --- JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub initial: CpuState,
    #[serde(rename = "final")]
    pub final_state: CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuState {
    pub pc: u16,
    pub s: u16,
    pub u: u16,
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub x: u16,
    pub y: u16,
    pub cc: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- M6502 JSON test vector types (SingleStepTests/65x02 format) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6502TestCase {
    pub name: String,
    pub initial: M6502CpuState,
    #[serde(rename = "final")]
    pub final_state: M6502CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6502CpuState {
    pub pc: u16,
    pub s: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub p: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- Z80 JSON test vector types (SingleStepTests/z80 format) ---

#[derive(Debug, Clone, Deserialize)]
pub struct Z80TestCase {
    pub name: String,
    pub initial: Z80CpuState,
    #[serde(rename = "final")]
    pub final_state: Z80CpuState,
    pub cycles: Vec<(Option<u16>, Option<u8>, String)>,
    #[serde(default)]
    pub ports: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Z80CpuState {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub f: u8,
    pub h: u8,
    pub l: u8,
    pub i: u8,
    pub r: u8,
    pub ei: u8,
    pub wz: u16,
    pub ix: u16,
    pub iy: u16,
    #[serde(rename = "af_")]
    pub af_prime: u16,
    #[serde(rename = "bc_")]
    pub bc_prime: u16,
    #[serde(rename = "de_")]
    pub de_prime: u16,
    #[serde(rename = "hl_")]
    pub hl_prime: u16,
    pub im: u8,
    pub p: u8,
    pub q: u8,
    pub iff1: u8,
    pub iff2: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- M6800 JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6800TestCase {
    pub name: String,
    pub initial: M6800CpuState,
    #[serde(rename = "final")]
    pub final_state: M6800CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6800CpuState {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub cc: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- I8035 (MCS-48) JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I8035TestCase {
    pub name: String,
    pub initial: I8035CpuState,
    #[serde(rename = "final")]
    pub final_state: I8035CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I8035CpuState {
    pub a: u8,
    pub pc: u16,
    pub psw: u8,
    pub f1: bool,
    pub t: u8,
    pub dbbb: u8,
    pub p1: u8,
    pub p2: u8,
    pub a11: bool,
    pub a11_pending: bool,
    pub timer_enabled: bool,
    pub counter_enabled: bool,
    pub timer_overflow: bool,
    pub int_enabled: bool,
    pub tcnti_enabled: bool,
    pub in_interrupt: bool,
    /// External bus memory (program memory + I/O mapped via io_read/io_write).
    pub ram: Vec<(u16, u8)>,
    /// Internal CPU RAM (64 bytes for 8035). Sparse (addr, value) pairs.
    pub internal_ram: Vec<(u8, u8)>,
}

// --- I8088 JSON test vector types (SingleStepTests/8088 v2 format) ---
//
// The 8088 test format uses 20-bit physical addresses and a sparse final
// state: only *changed* registers appear in the final state. We deserialize
// final regs as `Option<T>` and fall back to the initial value for comparison.

/// A single 8088 test vector.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088TestCase {
    pub name: String,
    pub bytes: Vec<u8>,
    pub initial: I8088InitialState,
    #[serde(rename = "final")]
    pub final_state: I8088FinalState,
    /// Per-cycle bus and queue trace, recorded from the hardware. One entry per
    /// CPU cycle (T-state), not per bus cycle. See [`I8088Cycle`].
    #[serde(default)]
    pub cycles: Vec<I8088Cycle>,
    // hash and idx are present but not used for validation
}

/// One recorded CPU cycle: the eleven fields the suite documents, in order.
///
/// Deserialized from a heterogeneous JSON array, e.g.
/// `[1, 205194, "--", "---", "---", 0, 0, "CODE", "T1", "-", 0]`. The string
/// fields become enums and bitfields here rather than `String`s, because there
/// are on the order of fifty million of them across the suite.
///
/// Which fields are *valid* on a given cycle is not uniform, and comparing one
/// where it is not valid produces a failure that means nothing:
///
/// - `bus` holds a valid address only while [`ale`](Self::ale) is asserted, on
///   T1. On other cycles it is whatever the multiplexed pins happen to carry.
/// - `data` is valid on T3, or on the last Tw when wait states are inserted.
/// - `queue_byte` is valid only when `queue_op` is not [`QueueOp::Idle`].
/// - `queue_op` reports an operation that happened on the *previous* cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct I8088Cycle(
    /// Pin bitfield: bit 0 = ALE, bit 1 = INTR, bit 2 = NMI.
    pub u8,
    /// The 20-bit multiplexed address/data bus, latched on ALE.
    pub u32,
    /// S3/S4: which segment register computed the address.
    pub SegmentStatus,
    /// i8288 memory command lines, as asserted bits (see [`CommandLines`]).
    pub CommandLines,
    /// i8288 I/O command lines, as asserted bits (see [`CommandLines`]).
    pub CommandLines,
    /// BHE. An 8086 pin that does not exist on the 8088; always 0 here.
    pub u8,
    /// The low 8 bits of the multiplexed bus. Valid on T3 or the last Tw.
    pub u8,
    /// S0-S2: what kind of bus cycle this is.
    pub BusStatus,
    /// Which T-state of the bus cycle this is.
    pub TState,
    /// QS0/QS1: what the EU did to the queue on the previous cycle.
    pub QueueOp,
    /// The byte read out of the queue, valid when `queue_op` is not `Idle`.
    pub u8,
);

impl I8088TestCase {
    /// The bytes the recording shows the part reading from I/O ports, in order.
    ///
    /// Taken from the data pins on the T3 of every IOR cycle, which is where
    /// this format keeps them: an `IN`'s result is nowhere in `initial.ram`,
    /// and without this a replay reads whatever the harness's memory happens to
    /// hold and then disagrees with `final.regs.ax` for a reason that is about
    /// the harness.
    pub fn port_reads(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pending = false;
        for c in &self.cycles {
            if c.address().is_some() {
                pending = c.status() == BusStatus::IOR;
            }
            if pending && c.t_state() == TState::T3 {
                out.push(c.6);
                pending = false;
            }
        }
        out
    }

    /// And the writes, as (port, byte) pairs, for a harness that wants to check
    /// what went out rather than only what came back.
    pub fn port_writes(&self) -> Vec<(u16, u8)> {
        let mut out = Vec::new();
        let mut pending: Option<u16> = None;
        for c in &self.cycles {
            if let Some(addr) = c.address() {
                pending = (c.status() == BusStatus::IOW).then_some(addr as u16);
            }
            if c.t_state() == TState::T3
                && let Some(port) = pending.take()
            {
                out.push((port, c.6));
            }
        }
        out
    }
}

impl I8088Cycle {
    /// True when ALE is asserted, which is the only time field 1 is an address.
    pub fn ale(&self) -> bool {
        self.0 & 1 != 0
    }

    /// The latched 20-bit address. `None` on any cycle where the bus does not
    /// carry one, so a caller cannot accidentally compare a stale value.
    pub fn address(&self) -> Option<u32> {
        self.ale().then_some(self.1 & 0xF_FFFF)
    }

    /// What kind of bus cycle this is.
    pub fn status(&self) -> BusStatus {
        self.7
    }

    /// Which T-state this is.
    pub fn t_state(&self) -> TState {
        self.8
    }

    /// What the EU did to the queue on the previous cycle, and the byte it
    /// read. `None` when the queue was untouched.
    pub fn queue_op(&self) -> Option<(QueueOp, u8)> {
        match self.9 {
            QueueOp::Idle => None,
            op => Some((op, self.10)),
        }
    }
}

/// S3/S4: which segment register the CPU used to compute the current address.
/// `None` on a cycle that is not driving an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SegmentStatus {
    #[serde(rename = "--")]
    None,
    ES,
    SS,
    CS,
    DS,
}

/// S0-S2: the type of bus cycle in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum BusStatus {
    /// Interrupt acknowledge.
    INTA,
    /// I/O read.
    IOR,
    /// I/O write.
    IOW,
    /// Memory read (data).
    MEMR,
    /// Memory write.
    MEMW,
    /// Halt acknowledge.
    HALT,
    /// Instruction fetch.
    CODE,
    /// Passive: no bus cycle in progress.
    PASV,
}

/// The T-state of the bus cycle. `Ti` is idle, `Tw` is a wait state inserted
/// between T3 and T4. The suite's own tests incur no wait states, so `Tw` does
/// not appear in the recorded data, but the format carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TState {
    T1,
    T2,
    T3,
    T4,
    Tw,
    Ti,
}

/// QS0/QS1: what the EU did to the prefetch queue. Reported one cycle late, so
/// the operation happened on the cycle *before* the one carrying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum QueueOp {
    /// First byte of an instruction or of an instruction prefix. This is what
    /// delimits a test: a case's cycles run from one `First` to the next.
    #[serde(rename = "F")]
    First,
    /// A subsequent byte: ModR/M, displacement or immediate.
    #[serde(rename = "S")]
    Subsequent,
    /// The queue was emptied, that is, flushed by a control transfer.
    #[serde(rename = "E")]
    Emptied,
    /// Nothing was read from the queue on the previous cycle.
    #[serde(rename = "-")]
    Idle,
}

/// The i8288's three command lines for one address space, as a bitfield of
/// *asserted* lines.
///
/// The recorded field is a three-character string in the shape `RAW`, with a
/// letter where a line is asserted and `-` where it is not, e.g. `"R--"` or
/// `"-AW"`. The lines themselves are active low on the part; this type stores
/// "asserted", so [`Self::read`] is true exactly when the recorded string has an
/// `R`, with no inversion left for a caller to get backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandLines(u8);

impl CommandLines {
    /// MRDC for memory, IORC for I/O: a read is in progress.
    pub fn read(&self) -> bool {
        self.0 & 1 != 0
    }

    /// AMWC for memory, AIOWC for I/O: the advanced write command.
    pub fn advanced_write(&self) -> bool {
        self.0 & 2 != 0
    }

    /// MWTC for memory, IOWC for I/O: the normal write command.
    pub fn write(&self) -> bool {
        self.0 & 4 != 0
    }

    /// True when no line is asserted, the `"---"` case.
    pub fn is_idle(&self) -> bool {
        self.0 == 0
    }
}

impl<'de> Deserialize<'de> for CommandLines {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = CommandLines;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a three-character i8288 status string in the shape RAW")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<CommandLines, E> {
                let b = s.as_bytes();
                if b.len() != 3 {
                    return Err(E::invalid_length(b.len(), &self));
                }
                let mut bits = 0u8;
                for (i, (asserted, idle)) in [(b'R', b'-'), (b'A', b'-'), (b'W', b'-')]
                    .into_iter()
                    .enumerate()
                {
                    match b[i] {
                        c if c == asserted => bits |= 1 << i,
                        c if c == idle => {}
                        _ => return Err(E::invalid_value(serde::de::Unexpected::Str(s), &self)),
                    }
                }
                Ok(CommandLines(bits))
            }
        }
        d.deserialize_str(V)
    }
}

/// Full initial CPU state (all registers present).
#[derive(Debug, Clone, Deserialize)]
pub struct I8088InitialState {
    pub regs: I8088Regs,
    pub ram: Vec<(u32, u8)>,
    #[serde(default)]
    pub queue: Vec<u8>,
}

/// Sparse final CPU state (only changed registers present).
#[derive(Debug, Clone, Deserialize)]
pub struct I8088FinalState {
    pub regs: I8088SparseRegs,
    pub ram: Vec<(u32, u8)>,
    #[serde(default)]
    pub queue: Vec<u8>,
}

/// Full register set for initial state.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088Regs {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub cs: u16,
    pub ss: u16,
    pub ds: u16,
    pub es: u16,
    pub sp: u16,
    pub bp: u16,
    pub si: u16,
    pub di: u16,
    pub ip: u16,
    pub flags: u16,
}

/// Sparse register set for final state — only changed values present.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct I8088SparseRegs {
    pub ax: Option<u16>,
    pub bx: Option<u16>,
    pub cx: Option<u16>,
    pub dx: Option<u16>,
    pub cs: Option<u16>,
    pub ss: Option<u16>,
    pub ds: Option<u16>,
    pub es: Option<u16>,
    pub sp: Option<u16>,
    pub bp: Option<u16>,
    pub si: Option<u16>,
    pub di: Option<u16>,
    pub ip: Option<u16>,
    pub flags: Option<u16>,
}

/// Per-opcode metadata from metadata.json.
/// Some opcodes have nested `reg` sub-keys for ModR/M group opcodes.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088OpcodeMetadata {
    pub status: Option<String>,
    #[serde(default)]
    pub flags: Option<String>,
    #[serde(default, rename = "flags-mask")]
    pub flags_mask: Option<u16>,
    /// Nested per-reg metadata for group opcodes (80, D0, F6, etc.)
    #[serde(default)]
    pub reg: Option<std::collections::HashMap<String, I8088SubOpcodeMetadata>>,
}

/// Sub-opcode metadata within a ModR/M group.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088SubOpcodeMetadata {
    pub status: Option<String>,
    #[serde(default)]
    pub flags: Option<String>,
    #[serde(default, rename = "flags-mask")]
    pub flags_mask: Option<u16>,
}

/// Top-level metadata.json structure.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088Metadata {
    pub version: String,
    pub cpu: String,
    pub opcodes: std::collections::HashMap<String, I8088OpcodeMetadata>,
}

impl I8088Metadata {
    /// Look up the flags mask for a given opcode file stem (e.g. "D0.4", "00").
    /// Returns 0xFFFF if no mask is specified (all flags defined).
    pub fn flags_mask_for(&self, file_stem: &str) -> u16 {
        // File stems like "D0.4" → opcode "D0", sub "4"
        if let Some((opcode, sub)) = file_stem.split_once('.')
            && let Some(meta) = self.opcodes.get(opcode)
        {
            // Check nested reg metadata first
            if let Some(reg_map) = &meta.reg
                && let Some(sub_meta) = reg_map.get(sub)
            {
                return sub_meta.flags_mask.unwrap_or(0xFFFF);
            }
            // Fall back to parent flags_mask
            return meta.flags_mask.unwrap_or(0xFFFF);
        }
        // Simple opcode like "00"
        if let Some(meta) = self.opcodes.get(file_stem) {
            return meta.flags_mask.unwrap_or(0xFFFF);
        }
        0xFFFF
    }
}

// --- 1MB TracingBus for 8088 (20-bit address space) ---

/// A bus with 1MB of memory for 8088 validation (20-bit physical addresses).
pub struct TracingBus20 {
    pub memory: Box<[u8; 0x10_0000]>,
    /// What an I/O read should return, in the order the recording shows the
    /// reads happening, and how many have been served.
    ///
    /// I/O is the one thing the 8088 vectors do not put in `initial.ram`: a
    /// port read's data appears only in the cycle trace, on the T3 of an IOR
    /// cycle. That is why the eight `IN`/`OUT` files were skipped for as long
    /// as this harness read only the state. Fill this from
    /// [`I8088TestCase::port_reads`] and an `IN` returns what the part saw.
    pub port_reads: Vec<u8>,
    pub port_index: usize,
    /// The port writes the CPU performed, in order, for comparison against the
    /// recorded IOW cycles.
    pub port_writes: Vec<(u16, u8)>,
}

impl TracingBus20 {
    pub fn new() -> Self {
        Self {
            memory: Box::new([0; 0x10_0000]),
            port_reads: Vec::new(),
            port_index: 0,
            port_writes: Vec::new(),
        }
    }
}

impl Default for TracingBus20 {
    fn default() -> Self {
        Self::new()
    }
}

// --- M68000 JSON test vector types (SingleStepTests/680x0 format) ---
//
// Each test holds a full flat register file before and after one
// instruction. A7 is implicit: the SR supervisor bit selects whether `ssp`
// or `usp` is the active stack pointer. `pc` is the address of the
// instruction under test and `prefetch` holds the two words the real CPU
// has already fetched from `pc`/`pc+2` (they are not necessarily present in
// `ram`). RAM is sparse byte (address, value) pairs.

/// A single 68000 test vector.
#[derive(Debug, Clone, Deserialize)]
pub struct M68000TestCase {
    pub name: String,
    pub initial: M68000Regs,
    #[serde(rename = "final")]
    pub final_state: M68000Regs,
    /// Execution length in clock cycles.
    pub length: u32,
    /// The per-cycle bus trace: every transfer and every idle gap, in order,
    /// tiling the instruction exactly. The state-only gate ignores this; the
    /// per-cycle gate is built on it.
    #[serde(default)]
    pub transactions: BusTrace,
}

impl M68000TestCase {
    /// Total clocks accounted for by the trace.
    pub fn traced_clocks(&self) -> u32 {
        self.transactions.iter().map(|t| t.clocks).sum()
    }

    /// Whether the trace tiles the recorded length exactly.
    ///
    /// This is the harness's own check on its oracle rather than a check on
    /// this emulator, and it has to hold before any positional comparison
    /// means anything: entries that do not tile cannot say which clock a bus
    /// cycle starts on. `length` is stored alongside the trace rather than
    /// derived from it in both suites, so the two can disagree.
    pub fn tiles(&self) -> bool {
        self.traced_clocks() == self.length
    }
}

/// Full 68000 register file + memory state (initial and final use the same
/// shape; the final state is complete, not sparse).
#[derive(Debug, Clone, Deserialize)]
pub struct M68000Regs {
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    pub d4: u32,
    pub d5: u32,
    pub d6: u32,
    pub d7: u32,
    pub a0: u32,
    pub a1: u32,
    pub a2: u32,
    pub a3: u32,
    pub a4: u32,
    pub a5: u32,
    pub a6: u32,
    pub usp: u32,
    pub ssp: u32,
    pub sr: u16,
    pub pc: u32,
    /// The two instruction words already in the prefetch queue.
    pub prefetch: [u16; 2],
    /// Sparse byte memory: (24-bit address, value) pairs.
    pub ram: Vec<(u32, u8)>,
}

impl M68000Regs {
    /// Data registers as an array (mirrors `M68000::d`).
    pub fn d(&self) -> [u32; 8] {
        [
            self.d0, self.d1, self.d2, self.d3, self.d4, self.d5, self.d6, self.d7,
        ]
    }

    /// Address registers A0-A6 (A7 lives in `usp`/`ssp` per the SR S bit).
    pub fn a(&self) -> [u32; 7] {
        [
            self.a0, self.a1, self.a2, self.a3, self.a4, self.a5, self.a6,
        ]
    }

    /// True if the SR supervisor bit selects SSP as the active A7.
    pub fn is_supervisor(&self) -> bool {
        self.sr & 0x2000 != 0
    }

    /// The active stack pointer (what `M68000::a[7]` should hold).
    pub fn active_sp(&self) -> u32 {
        if self.is_supervisor() {
            self.ssp
        } else {
            self.usp
        }
    }
}

/// What the 68000 was doing on the bus for the span of one recorded entry.
///
/// The two suites agree on the first four and only the `m68000` set emits the
/// last two. An address error still runs its bus cycle on the real part: AS is
/// simply never asserted, so the transfer is not committed. The set records
/// those cycles rather than dropping them so an address error is recognizable
/// from the trace alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusTxnKind {
    /// The bus is idle for the recorded number of clocks.
    Idle,
    /// A read transfer.
    Read,
    /// A write transfer.
    Write,
    /// The indivisible read-modify-write cycle `TAS` runs.
    Tas,
    /// A read that faulted on an odd address; AS never asserted.
    ReadAddressError,
    /// A write that faulted on an odd address; AS never asserted.
    WriteAddressError,
}

/// The width of one transfer. The 68000 selects a byte with one of the two
/// data strobes and a word with both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnSize {
    Byte,
    Word,
}

/// One entry of a recorded bus trace.
///
/// The entries tile the instruction exactly: their clocks sum to the case's
/// `length`, so the trace fixes not only which cycles ran but where each one
/// starts. Idle entries carry only `clocks`; everything else is meaningful
/// solely on a transfer.
///
/// **The two suites post `addr` differently and the difference is not
/// cosmetic.** The `680x0` set posts the unaligned byte address and leaves the
/// strobe to be inferred from bit 0. The `m68000` set posts the true word
/// address and carries UDS and LDS as separate signals, which is what the part
/// does: it has no A0 pin. [`Self::byte_address`] resolves both to the byte the
/// transfer actually touched, so a comparison never has to care which set it
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusTxn {
    pub kind: BusTxnKind,
    /// Duration in clock cycles.
    pub clocks: u32,
    /// Function code FC2..FC0: which address space the transfer names.
    pub fc: u32,
    /// The address posted on the bus, as the source suite posts it.
    pub addr: u32,
    pub size: TxnSize,
    /// The value on the data bus, as the source suite posts it. See
    /// [`Self::byte_value`] for why this is not directly comparable.
    pub data: u32,
    /// Upper data strobe, set when the even byte is selected. The `680x0` set
    /// does not record it and it is derived from the address there.
    pub uds: bool,
    /// Lower data strobe, set when the odd byte is selected.
    pub lds: bool,
}

impl BusTxn {
    /// True when this entry is a transfer rather than idle time.
    pub fn is_transfer(&self) -> bool {
        self.kind != BusTxnKind::Idle
    }

    /// The byte address the transfer actually touched.
    ///
    /// A word transfer names its even base. A byte transfer names the even byte
    /// under UDS and the odd one under LDS, which is how the part addresses a
    /// half without an A0 pin.
    pub fn byte_address(&self) -> u32 {
        match self.size {
            TxnSize::Word => self.addr & !1,
            TxnSize::Byte if self.lds && !self.uds => (self.addr & !1) | 1,
            TxnSize::Byte => self.addr & !1,
        }
    }

    /// The byte a byte-sized transfer carried, normalized out of its bus half.
    ///
    /// The `m68000` set posts the data bus as the part drives it, so the byte
    /// `0xB3` reads `0xB300` under UDS and `0x00B3` under LDS. The `680x0` set
    /// normalizes to 0..255 instead. Comparing the raw field across the two
    /// would fail every odd-address access for the wrong reason.
    pub fn byte_value(&self) -> u8 {
        if self.uds && !self.lds {
            (self.data >> 8) as u8
        } else {
            self.data as u8
        }
    }
}

/// A recorded trace's entries, in order.
///
/// The clocks sum to the case's `length`. [`M68000TestCase::tiles`] is the
/// self-check on that, and it is asserted rather than assumed: a trace whose
/// entries do not tile its length cannot place a bus cycle in time, so every
/// positional comparison built on it would be meaningless.
pub type BusTrace = Vec<BusTxn>;

/// Deserialize one trace entry from either suite's array encoding.
///
/// `["n", 4]` is idle. A transfer is
/// `[kind, clocks, fc, addr, size, data]` in the `680x0` set and
/// `[kind, clocks, fc, addr, size, data, uds, lds]` in the `m68000` set. The
/// two extra fields are read when present, and derived from the posted
/// address's low bit when absent, so both encodings land in the same struct.
impl<'de> Deserialize<'de> for BusTxn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, IgnoredAny, SeqAccess, Visitor};

        struct TxnVisitor;

        impl<'de> Visitor<'de> for TxnVisitor {
            type Value = BusTxn;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a bus transaction array of 2, 6 or 8 elements")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<BusTxn, A::Error> {
                macro_rules! next {
                    ($t:ty, $what:expr) => {
                        seq.next_element::<$t>()?.ok_or_else(|| {
                            A::Error::custom(concat!("transaction missing ", $what))
                        })?
                    };
                }

                let tag = next!(String, "kind");
                let clocks = next!(u32, "clock count");

                let kind = match tag.as_str() {
                    "n" => {
                        // Drain so a trailing element cannot pass unnoticed.
                        while seq.next_element::<IgnoredAny>()?.is_some() {}
                        return Ok(BusTxn {
                            kind: BusTxnKind::Idle,
                            clocks,
                            fc: 0,
                            addr: 0,
                            size: TxnSize::Word,
                            data: 0,
                            uds: false,
                            lds: false,
                        });
                    }
                    "r" => BusTxnKind::Read,
                    "w" => BusTxnKind::Write,
                    "t" => BusTxnKind::Tas,
                    "re" => BusTxnKind::ReadAddressError,
                    "we" => BusTxnKind::WriteAddressError,
                    other => {
                        return Err(A::Error::custom(format!(
                            "unknown transaction kind {other:?}"
                        )));
                    }
                };

                let fc = next!(u32, "function code");
                let addr = next!(u32, "address");
                let size_tag = next!(String, "size");
                let size = match size_tag.as_str() {
                    ".w" => TxnSize::Word,
                    ".b" => TxnSize::Byte,
                    other => return Err(A::Error::custom(format!("unknown size {other:?}"))),
                };
                let data = next!(u32, "data");

                // The m68000 set records the strobes; the 680x0 set does not,
                // and posts the odd byte address in their place.
                let (uds, lds) = match (seq.next_element::<u32>()?, seq.next_element::<u32>()?) {
                    (Some(u), Some(l)) => (u != 0, l != 0),
                    _ => match size {
                        TxnSize::Word => (true, true),
                        TxnSize::Byte if addr & 1 != 0 => (false, true),
                        TxnSize::Byte => (true, false),
                    },
                };
                while seq.next_element::<IgnoredAny>()?.is_some() {}

                Ok(BusTxn {
                    kind,
                    clocks,
                    fc,
                    addr,
                    size,
                    data,
                    uds,
                    lds,
                })
            }
        }

        d.deserialize_seq(TxnVisitor)
    }
}

// --- MB88XX JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mb88xxTestCase {
    pub name: String,
    pub initial: Mb88xxCpuState,
    #[serde(rename = "final")]
    pub final_state: Mb88xxCpuState,
    pub cycles: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mb88xxCpuState {
    pub pc: u8,
    pub pa: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub si: u8,
    pub st: u8,
    pub zf: u8,
    pub cf: u8,
    pub vf: u8,
    pub sf: u8,
    pub nf: u8,
    pub pio: u8,
    pub th: u8,
    pub tl: u8,
    pub tp: u8,
    pub sb: u8,
    pub stack: [u16; 4],
    pub rom: Vec<(u16, u8)>,
    pub ram: Vec<(u8, u8)>,
    pub io: Vec<(u8, u8)>,
}

impl Bus for TracingBus20 {
    type Address = u32;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.memory[(addr & 0xF_FFFF) as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.memory[(addr & 0xF_FFFF) as usize] = data;
    }

    /// I/O is a separate address space on the 8088, so this does not fall back
    /// to memory the way the trait's default does. An unfilled queue returns
    /// 0xFF, which is what an unclaimed port reads as on a real board.
    fn io_read(&mut self, _master: BusMaster, _addr: u32) -> u8 {
        let value = self
            .port_reads
            .get(self.port_index)
            .copied()
            .unwrap_or(0xFF);
        self.port_index += 1;
        value
    }

    fn io_write(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.port_writes.push((addr as u16, data));
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// --- 16MB word bus for 68000 (24-bit address space, 16-bit data) ---

/// A bus with 16 MB of byte memory for 68000 validation, served as 16-bit
/// big-endian words at even addresses (the 68000 bus transaction width).
pub struct TracingBus68k {
    pub memory: Box<[u8]>,
    /// Masked word addresses written through the `Bus` trait. Harnesses
    /// reuse one 16 MB bus across thousands of test cases and zero only the
    /// touched words between cases instead of memsetting the whole array.
    pub dirty_writes: Vec<u32>,
}

impl TracingBus68k {
    pub fn new() -> Self {
        Self {
            memory: vec![0; 0x100_0000].into_boxed_slice(),
            dirty_writes: Vec::new(),
        }
    }
}

impl Default for TracingBus68k {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TracingBus68k {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0x00FF_FFFE) as usize;
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0x00FF_FFFE) as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
        self.dirty_writes.push(i as u32);
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

/// One access this emulator made, in the order it made it.
///
/// Deliberately *not* shaped like [`BusTxn`]. This core drives the bus a word
/// at a time, so it has no byte transfers and no clock positions to record; a
/// struct that could express those would invite writing a comparison that
/// quietly credits this core with resolution it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OurAccess {
    pub write: bool,
    /// The word address presented to the bus.
    pub addr: u32,
    pub data: u16,
}

/// [`TracingBus68k`] with an access log, for comparing this core's bus activity
/// against a recorded trace.
///
/// Recording is off until [`Self::start_recording`], so the harness's own
/// setup writes never enter the log.
pub struct RecordingBus68k {
    pub memory: Box<[u8]>,
    pub dirty_writes: Vec<u32>,
    pub log: Vec<OurAccess>,
    recording: bool,
}

impl RecordingBus68k {
    pub fn new() -> Self {
        Self {
            memory: vec![0; 0x100_0000].into_boxed_slice(),
            dirty_writes: Vec::new(),
            log: Vec::new(),
            recording: false,
        }
    }

    /// Begin a fresh recording for one test case.
    pub fn start_recording(&mut self) {
        self.log.clear();
        self.recording = true;
    }

    pub fn stop_recording(&mut self) {
        self.recording = false;
    }
}

impl Default for RecordingBus68k {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for RecordingBus68k {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0x00FF_FFFE) as usize;
        let data = u16::from_be_bytes([self.memory[i], self.memory[i + 1]]);
        if self.recording {
            self.log.push(OurAccess {
                write: false,
                addr: i as u32,
                data,
            });
        }
        data
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0x00FF_FFFE) as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
        self.dirty_writes.push(i as u32);
        if self.recording {
            self.log.push(OurAccess {
                write: true,
                addr: i as u32,
                data,
            });
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The whole point of `vector_dir`: the same literal must mean one place
    /// whichever directory the caller happens to be in. Cargo runs this crate's
    /// integration tests from the crate root and its binaries from wherever the
    /// user invoked cargo, and a relative path meant two different directories
    /// to those two halves.
    #[test]
    fn a_vector_directory_is_absolute_so_it_cannot_mean_two_places() {
        let dir = vector_dir("m6800");
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(dir.ends_with(Path::new("cpu-validation/test_data/m6800")));
        // Nested suite paths land in the same tree rather than being rebased.
        assert!(vector_dir("65x02/6502/v1").ends_with(Path::new("test_data/65x02/6502/v1")),);
    }

    /// A missing directory is a skip by default, because a fresh clone has no
    /// vectors at all and failing there would be an environment complaint
    /// rather than a finding.
    #[test]
    fn a_missing_directory_skips_when_the_vectors_are_optional() {
        assert!(!vectors_available(
            Path::new("/nonexistent/vectors"),
            "run: the generator",
            false
        ));
    }

    /// And a failure where something was supposed to have put them there. This
    /// is the guard on the hazard the skip creates: libtest hides stderr for a
    /// passing test, so without it a validator that found nothing is green and
    /// silent, and the suite reports success having validated nothing.
    #[test]
    #[should_panic(expected = "PHOSPHOR_REQUIRE_VECTORS is set")]
    fn a_missing_directory_fails_when_the_vectors_are_required() {
        vectors_available(
            Path::new("/nonexistent/vectors"),
            "run: the generator",
            true,
        );
    }

    /// A directory that exists is available either way, and says so without
    /// consulting the flag.
    #[test]
    fn an_existing_directory_is_available_however_it_was_asked_for() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(vectors_available(here, "unused", false));
        assert!(vectors_available(here, "unused", true));
    }

    // --- I8088 cycle trace parsing ---
    //
    // The rows below are copied verbatim from the sample test in the suite's
    // own README (`test_data/8088/README.md`), which is the only description of
    // this format there is. Parsing it wrong is the failure mode that would
    // make the per-cycle gate compare our trace against a misreading of the
    // hardware's, so the parser is checked against the document rather than
    // against what our CPU happens to produce.

    fn cycle(json: &str) -> I8088Cycle {
        serde_json::from_str(json).expect("cycle row parses")
    }

    /// The T1 of a code fetch: ALE asserted, so the bus carries a real address.
    #[test]
    fn a_t1_cycle_latches_an_address_because_ale_is_asserted() {
        let c = cycle(r#"[1, 205194, "--", "---", "---", 0, 0, "CODE", "T1", "-", 0]"#);
        assert!(c.ale());
        assert_eq!(c.address(), Some(205194));
        assert_eq!(c.status(), BusStatus::CODE);
        assert_eq!(c.t_state(), TState::T1);
        assert_eq!(c.queue_op(), None);
    }

    /// And the T2 that follows it does not, even though the bus field still
    /// holds a number. Reading that number as an address is exactly the
    /// mistake `address()` exists to prevent.
    #[test]
    fn a_cycle_without_ale_has_no_address_however_full_the_bus_field_looks() {
        let c = cycle(r#"[0, 139658, "CS", "R--", "---", 0, 0, "CODE", "T2", "-", 0]"#);
        assert!(!c.ale());
        assert_eq!(c.address(), None);
        assert_eq!(c.2, SegmentStatus::CS);
        assert!(c.3.read(), "MRDC is asserted on the memory lines");
        assert!(c.4.is_idle(), "the I/O lines are not");
    }

    /// A queue read is reported one cycle after it happened, and only then is
    /// the queue byte meaningful.
    #[test]
    fn a_queue_read_carries_its_byte_and_an_idle_one_does_not() {
        let read = cycle(r#"[0, 139659, "CS", "R--", "---", 0, 0, "CODE", "T2", "S", 156]"#);
        assert_eq!(read.queue_op(), Some((QueueOp::Subsequent, 156)));

        let idle = cycle(r#"[0, 72724, "--", "---", "---", 0, 0, "PASV", "Ti", "-", 0]"#);
        assert_eq!(idle.queue_op(), None);
        assert_eq!(idle.t_state(), TState::Ti);
        assert_eq!(idle.status(), BusStatus::PASV);
    }

    /// The suite defines a test's span by the queue status lines: from the
    /// First Byte of this instruction to the First Byte of the next. So `F` has
    /// to survive parsing as something distinguishable from `S`.
    #[test]
    fn a_first_byte_is_what_delimits_a_test() {
        let c = cycle(r#"[0, 62369, "--", "---", "---", 0, 0, "PASV", "Ti", "F", 0]"#);
        assert_eq!(c.queue_op(), Some((QueueOp::First, 0)));
    }

    /// The i8288 status strings are stored as asserted lines, so no caller has
    /// an active-low inversion left to get backwards.
    #[test]
    fn command_lines_record_which_lines_are_asserted() {
        let write = cycle(r#"[0, 72924, "SS", "-AW", "---", 0, 220, "PASV", "T3", "-", 0]"#);
        assert!(!write.3.read());
        assert!(write.3.advanced_write());
        assert!(write.3.write());
        assert_eq!(write.6, 220, "the data bus is valid on T3");

        let none = cycle(r#"[0, 72724, "--", "---", "---", 0, 0, "PASV", "Ti", "-", 0]"#);
        assert!(none.3.is_idle());
        assert!(none.4.is_idle());
    }

    /// An I/O cycle asserts the other set of lines, and nothing in the parser
    /// conflates the two.
    #[test]
    fn io_lines_are_a_separate_field_from_memory_lines() {
        let c = cycle(r#"[0, 100, "DS", "---", "R--", 0, 255, "IOR", "T3", "-", 0]"#);
        assert!(c.3.is_idle(), "memory lines idle during an I/O read");
        assert!(c.4.read());
        assert_eq!(c.status(), BusStatus::IOR);
    }

    /// A status string that is not three characters, or carries a letter in the
    /// wrong column, is a parse failure rather than a silently wrong bitfield.
    #[test]
    fn a_malformed_status_string_does_not_parse() {
        assert!(serde_json::from_str::<CommandLines>(r#""RA""#).is_err());
        assert!(serde_json::from_str::<CommandLines>(r#""RAWX""#).is_err());
        assert!(
            serde_json::from_str::<CommandLines>(r#""W--""#).is_err(),
            "W in the MRDC column is not a valid encoding"
        );
    }

    /// The whole point of deserializing `cycles` at all: a test case now knows
    /// how many cycles the hardware took, which is the M1 gate.
    #[test]
    fn a_test_case_carries_its_cycle_trace() {
        let tc: I8088TestCase = serde_json::from_str(
            r#"{
                "name": "nop",
                "bytes": [144],
                "initial": {"regs": {"ax":0,"bx":0,"cx":0,"dx":0,"cs":0,"ss":0,
                    "ds":0,"es":0,"sp":0,"bp":0,"si":0,"di":0,"ip":0,"flags":0},
                    "ram": [[0, 144]], "queue": []},
                "final": {"regs": {}, "ram": []},
                "cycles": [
                    [1, 0, "--", "---", "---", 0, 0, "CODE", "T1", "-", 0],
                    [0, 0, "CS", "R--", "---", 0, 0, "CODE", "T2", "-", 0],
                    [0, 0, "CS", "R--", "---", 0, 144, "PASV", "T3", "-", 0]
                ]
            }"#,
        )
        .expect("test case parses");
        assert_eq!(tc.cycles.len(), 3);
        assert_eq!(tc.cycles[2].6, 144);
    }

    /// And a case with no `cycles` key at all still parses, so the state-only
    /// gate does not become dependent on the trace being present.
    #[test]
    fn a_test_case_without_a_trace_still_parses() {
        let tc: I8088TestCase = serde_json::from_str(
            r#"{
                "name": "nop",
                "bytes": [144],
                "initial": {"regs": {"ax":0,"bx":0,"cx":0,"dx":0,"cs":0,"ss":0,
                    "ds":0,"es":0,"sp":0,"bp":0,"si":0,"di":0,"ip":0,"flags":0},
                    "ram": [], "queue": []},
                "final": {"regs": {}, "ram": []}
            }"#,
        )
        .expect("test case parses");
        assert!(tc.cycles.is_empty());
    }

    // --- 68000 bus trace parsing ---
    //
    // Two suites encode the same events differently, and the whole value of
    // having both is lost if the parser quietly normalizes one into the other's
    // mistakes. Every row below is a real recorded entry, and the pair of
    // `MOVE.b` rows is the point: the same byte, on opposite halves of the bus,
    // from the two different encodings.

    fn txn(json: &str) -> BusTxn {
        serde_json::from_str(json).expect("transaction parses")
    }

    /// The `680x0` encoding: six fields, no strobes, byte value normalized to
    /// 0..255, and the odd byte address posted directly.
    #[test]
    fn a_680x0_byte_read_infers_its_strobe_from_the_posted_address() {
        let t = txn(r#"["r", 4, 5, 8480815, ".b", 49]"#);
        assert_eq!(t.kind, BusTxnKind::Read);
        assert_eq!(t.clocks, 4);
        assert_eq!(t.size, TxnSize::Byte);
        // 8480815 is odd, so the transfer is on the lower half.
        assert!(t.lds && !t.uds);
        assert_eq!(t.byte_address(), 8480815);
        assert_eq!(t.byte_value(), 49);
    }

    /// The `m68000` encoding of an upper-half byte read: eight fields, the true
    /// (even) word address, and the byte sitting in the high half of the data
    /// bus exactly as the part drives it.
    #[test]
    fn an_m68000_uds_byte_read_carries_its_value_in_the_upper_half() {
        let t = txn(r#"["r", 4, 1, 4272488, ".b", 45824, 1, 0]"#);
        assert!(t.uds && !t.lds);
        assert_eq!(t.addr & 1, 0, "the part cannot post an odd address");
        assert_eq!(t.byte_address(), 4272488);
        // 45824 is 0xB300: the byte is 0xB3, not 0x00.
        assert_eq!(t.byte_value(), 0xB3);
    }

    /// The matching write from the same recorded case, on the *lower* half. The
    /// posted address is still even and only LDS says the odd byte was touched,
    /// which is the whole reason `byte_address` exists.
    #[test]
    fn an_m68000_lds_byte_write_touches_the_odd_byte_of_an_even_address() {
        let t = txn(r#"["w", 4, 1, 12788194, ".b", 179, 0, 1]"#);
        assert!(t.lds && !t.uds);
        assert_eq!(t.addr & 1, 0);
        assert_eq!(t.byte_address(), 12788195, "LDS selects the odd byte");
        assert_eq!(t.byte_value(), 0xB3);
    }

    /// And the two encodings agree once normalized, which is what makes a
    /// cross-suite comparison possible at all. Reading the raw `data` field
    /// instead would make these two disagree by a byte swap.
    #[test]
    fn the_two_encodings_of_the_same_byte_normalize_to_the_same_value() {
        let uds_half = txn(r#"["r", 4, 1, 4272488, ".b", 45824, 1, 0]"#);
        let lds_half = txn(r#"["w", 4, 1, 12788194, ".b", 179, 0, 1]"#);
        assert_eq!(uds_half.byte_value(), lds_half.byte_value());
        assert_ne!(
            uds_half.data, lds_half.data,
            "the raw fields differ; only the normalized bytes match"
        );
    }

    /// Idle time carries a duration and nothing else. Treating its zeroed
    /// address as a real one is the 68000 equivalent of reading an address off
    /// a cycle with no ALE.
    #[test]
    fn an_idle_entry_is_a_duration_and_not_a_transfer() {
        let t = txn(r#"["n", 122]"#);
        assert_eq!(t.kind, BusTxnKind::Idle);
        assert_eq!(t.clocks, 122);
        assert!(!t.is_transfer());
    }

    /// A word transfer asserts both strobes and names its even base.
    #[test]
    fn a_word_transfer_asserts_both_strobes() {
        let t = txn(r#"["r", 4, 6, 3076, ".w", 1657]"#);
        assert_eq!(t.size, TxnSize::Word);
        assert!(t.uds && t.lds);
        assert_eq!(t.byte_address(), 3076);
    }

    /// The `m68000` set's two extra kinds. These are bus cycles the part runs
    /// with AS never asserted, so the transfer is not committed; dropping them
    /// would lose the only trace-level evidence that an address error happened.
    #[test]
    fn the_address_error_kinds_parse_and_are_transfers() {
        let r = txn(r#"["re", 4, 1, 100, ".w", 0, 1, 1]"#);
        let w = txn(r#"["we", 4, 1, 100, ".w", 0, 1, 1]"#);
        assert_eq!(r.kind, BusTxnKind::ReadAddressError);
        assert_eq!(w.kind, BusTxnKind::WriteAddressError);
        assert!(r.is_transfer() && w.is_transfer());
    }

    /// An unknown kind is an error rather than a silently dropped entry: a new
    /// cycle type appearing upstream must stop the gate, not shorten its traces.
    #[test]
    fn an_unknown_transaction_kind_does_not_parse() {
        assert!(serde_json::from_str::<BusTxn>(r#"["x", 4, 1, 100, ".w", 0]"#).is_err());
    }

    /// A trace tiles its case's length, and a case whose entries do not sum to
    /// it is detectable. This is the harness's check on its oracle rather than
    /// on the emulator, so it has to be able to fail: the second half proves it
    /// does.
    #[test]
    fn a_trace_tiles_its_length_and_a_short_one_is_caught() {
        let mut tc: M68000TestCase = serde_json::from_str(
            r#"{
                "name": "4e71 [NOP] 1",
                "initial": {"d0":0,"d1":0,"d2":0,"d3":0,"d4":0,"d5":0,"d6":0,"d7":0,
                    "a0":0,"a1":0,"a2":0,"a3":0,"a4":0,"a5":0,"a6":0,
                    "usp":0,"ssp":2048,"sr":9985,"pc":3072,"prefetch":[20081,10835],"ram":[]},
                "final": {"d0":0,"d1":0,"d2":0,"d3":0,"d4":0,"d5":0,"d6":0,"d7":0,
                    "a0":0,"a1":0,"a2":0,"a3":0,"a4":0,"a5":0,"a6":0,
                    "usp":0,"ssp":2048,"sr":9985,"pc":3074,"prefetch":[10835,1657],"ram":[]},
                "length": 4,
                "transactions": [["r", 4, 6, 3076, ".w", 1657]]
            }"#,
        )
        .expect("test case parses");

        assert_eq!(tc.traced_clocks(), 4);
        assert!(tc.tiles());

        tc.transactions[0].clocks = 2;
        assert!(!tc.tiles(), "a trace that no longer sums must be rejected");
    }

    /// And a 68000 case with no trace still parses, so the state-only gate does
    /// not become dependent on the per-cycle field being present.
    #[test]
    fn a_68000_case_without_a_trace_still_parses() {
        let tc: M68000TestCase = serde_json::from_str(
            r#"{
                "name": "no trace",
                "initial": {"d0":0,"d1":0,"d2":0,"d3":0,"d4":0,"d5":0,"d6":0,"d7":0,
                    "a0":0,"a1":0,"a2":0,"a3":0,"a4":0,"a5":0,"a6":0,
                    "usp":0,"ssp":0,"sr":0,"pc":0,"prefetch":[0,0],"ram":[]},
                "final": {"d0":0,"d1":0,"d2":0,"d3":0,"d4":0,"d5":0,"d6":0,"d7":0,
                    "a0":0,"a1":0,"a2":0,"a3":0,"a4":0,"a5":0,"a6":0,
                    "usp":0,"ssp":0,"sr":0,"pc":0,"prefetch":[0,0],"ram":[]},
                "length": 0
            }"#,
        )
        .expect("test case parses");
        assert!(tc.transactions.is_empty());
    }
}
