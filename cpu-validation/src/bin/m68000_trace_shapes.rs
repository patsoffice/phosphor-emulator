//! Where the 68000 refills its prefetch queue, read off the recorded traces.
//!
//! An instruction's recorded trace contains no fetch of its own opcode: that
//! word came out of the prefetch queue before the instruction started. What it
//! contains instead are the *refills*, one program-space read for each word the
//! instruction consumed, and their position among the data accesses is the
//! thing a per-instruction timing table cannot express. This tool reduces every
//! case to the ordered sequence of what its bus did, ignoring durations and
//! idle gaps, and reports how many cases of each instruction have each shape:
//!
//! ```text
//! MOVE.w        4238  F.W.F   3f4f [MOVE.w A7, (d16, A7)] 4
//!               2130  W.F     3a8e [MOVE.w A6, (A5)] 1
//! ```
//!
//! `F` is a program-space transfer (a queue refill), `R` and `W` data reads and
//! writes, `T` the indivisible read-modify-write, and `E` a cycle run with the
//! address strobe never asserted, which is how an address error appears.
//!
//! Run it against either corpus, or both:
//!
//! ```text
//! cargo run -p phosphor-cpu-validation --bin m68000_trace_shapes -- [680x0|m68000]
//! cargo run -p phosphor-cpu-validation --bin m68000_trace_shapes -- --shapes 6 MOVE
//! ```
//!
//! `--dump N` prints the first `N` cases of each matching file in full instead:
//! pc, queue and trace before and after, which is what a question about queue
//! state at an instruction boundary has to be answered from.

use std::collections::BTreeMap;
use std::io::Read;

use phosphor_cpu_validation::{BusTxnKind, M68000TestCase, m68000_bin, vector_dir};

/// One case reduced to what its bus did, in order.
fn shape(tc: &M68000TestCase) -> String {
    let mut out = String::new();
    for t in &tc.transactions {
        let c = match t.kind {
            BusTxnKind::Idle => continue,
            // Program space is function code 6 (supervisor) or 2 (user); data
            // space is 5 and 1. The low bit is what separates them.
            BusTxnKind::Read if t.fc & 1 == 0 => 'F',
            BusTxnKind::Read => 'R',
            BusTxnKind::Write => 'W',
            BusTxnKind::Tas => 'T',
            BusTxnKind::ReadAddressError | BusTxnKind::WriteAddressError => 'E',
        };
        if !out.is_empty() {
            out.push('.');
        }
        out.push(c);
    }
    if out.is_empty() {
        out.push('-');
    }
    out
}

/// Print one case in full: what it started from, what it did, where it ended.
///
/// `lead` is how far this suite's `pc` runs ahead of the execution point (0 for
/// `680x0`, one prefetch for `m68000`), so both print the address the
/// instruction actually starts at.
fn dump(tc: &M68000TestCase, lead: u32) {
    let i = &tc.initial;
    let f = &tc.final_state;
    println!("  {}", tc.name);
    println!(
        "    initial pc {:#08x} (exec {:#08x})  prefetch [{:#06x}, {:#06x}]  sr {:#06x}",
        i.pc,
        i.pc.wrapping_sub(lead),
        i.prefetch[0],
        i.prefetch[1],
        i.sr
    );
    println!(
        "    final   pc {:#08x} (exec {:#08x})  prefetch [{:#06x}, {:#06x}]  sr {:#06x}",
        f.pc,
        f.pc.wrapping_sub(lead),
        f.prefetch[0],
        f.prefetch[1],
        f.sr
    );
    println!("    length {}  shape {}", tc.length, shape(tc));
    for t in &tc.transactions {
        match t.kind {
            BusTxnKind::Idle => println!("      idle {:>3}", t.clocks),
            _ => println!(
                "      {:?} {:>3} fc{} {:#08x} {:?} {:#06x}",
                t.kind, t.clocks, t.fc, t.addr, t.size, t.data
            ),
        }
    }
}

/// Shapes seen for one instruction file, with a case name for each.
#[derive(Default)]
struct Shapes {
    cases: usize,
    /// shape -> (count, an example case name)
    seen: BTreeMap<String, (usize, String)>,
}

impl Shapes {
    fn add(&mut self, tc: &M68000TestCase) {
        self.cases += 1;
        let entry = self
            .seen
            .entry(shape(tc))
            .or_insert_with(|| (0, tc.name.clone()));
        entry.0 += 1;
    }

    /// Shapes most common first.
    fn ranked(&self) -> Vec<(&String, usize, &String)> {
        let mut v: Vec<_> = self
            .seen
            .iter()
            .map(|(s, (n, name))| (s, *n, name))
            .collect();
        v.sort_by_key(|&(_, n, _)| std::cmp::Reverse(n));
        v
    }
}

fn instruction_of(filename: &str) -> String {
    filename
        .trim_end_matches(".json.gz")
        .trim_end_matches(".json.bin")
        .to_string()
}

fn read_680x0(filter: &Option<String>, dump_n: usize) -> BTreeMap<String, Shapes> {
    let dir = vector_dir("680x0/68000/v1");
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no 680x0 corpus at {}", dir.display());
        return out;
    };
    let mut entries: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "gz"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let name = instruction_of(&entry.file_name().to_string_lossy());
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let gz = std::fs::read(entry.path()).expect("read a vector file");
        let mut json = String::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut json)
            .expect("decompress a vector file");
        let tests: Vec<M68000TestCase> = serde_json::from_str(&json).expect("parse a vector file");
        if dump_n > 0 {
            println!("--- 680x0 {name}");
            for tc in tests.iter().take(dump_n) {
                dump(tc, 0);
            }
        }
        let shapes: &mut Shapes = out.entry(name).or_default();
        for tc in &tests {
            shapes.add(tc);
        }
    }
    out
}

fn read_m68000(filter: &Option<String>, dump_n: usize) -> BTreeMap<String, Shapes> {
    let dir = vector_dir("m68000/v1");
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no m68000 corpus at {}", dir.display());
        return out;
    };
    let mut entries: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "bin"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let name = instruction_of(&entry.file_name().to_string_lossy());
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let tests = m68000_bin::decode_file(&entry.path()).expect("decode a vector file");
        if dump_n > 0 {
            println!("--- m68000 {name}");
            for t in tests.iter().take(dump_n) {
                dump(
                    &t.case,
                    phosphor_cpu_validation::m68000_bin::PC_PREFETCH_LEAD,
                );
            }
        }
        let shapes: &mut Shapes = out.entry(name).or_default();
        for t in &tests {
            shapes.add(&t.case);
        }
    }
    out
}

fn report(label: &str, files: &BTreeMap<String, Shapes>, top: usize) {
    println!("\n=== {label}");
    for (instr, shapes) in files {
        let ranked = shapes.ranked();
        println!("{instr}  ({} cases, {} shapes)", shapes.cases, ranked.len());
        for (shape, n, example) in ranked.iter().take(top) {
            let pct = 100.0 * *n as f64 / shapes.cases as f64;
            println!("  {n:>7} {pct:>5.1}%  {shape:<28} {example}");
        }
    }
}

fn main() {
    let mut which = String::from("both");
    let mut top = 4usize;
    let mut dump_n = 0usize;
    let mut filter: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shapes" => top = args.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            "--dump" => dump_n = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "680x0" | "m68000" | "both" => which = arg,
            other => filter = Some(other.to_string()),
        }
    }

    if which != "m68000" {
        let files = read_680x0(&filter, dump_n);
        if dump_n == 0 {
            report("680x0 (documentation-derived)", &files, top);
        }
    }
    if which != "680x0" {
        let files = read_m68000(&filter, dump_n);
        if dump_n == 0 {
            report("m68000 (microcode-derived)", &files, top);
        }
    }
}
