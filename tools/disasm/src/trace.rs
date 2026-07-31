//! Headless CPU/bus trace — the `disasm trace` subcommand.
//!
//! Boots a registered machine (via [`Harness`]), runs it for a frame window,
//! and observes the state streams the interactive debugger produces but that
//! were previously reachable only from the SDL frontend. Two loops, chosen by
//! the requested observers:
//!
//! **Frame loop** (`--events`/`--watch` only) — runs whole frames and drains,
//! per frame, the board's [`DebugTrace`] event ring (`--events <kinds|all>`:
//! device writes, bank switches, interrupt edges, watchdog kicks, …) and the
//! watchpoint hit queue (`--watch <cpu:addr:kind>`). Records are merged and
//! emitted cycle-sorted. Bounded memory.
//!
//! **Cycle loop** (`--cpu` instruction trace or `--break-pc`) — drives
//! `debug_tick()` per cycle, disassembles at each traced CPU's instruction
//! boundaries (optional `:regs` register columns), correlates any watchpoint
//! hits/events by cycle, and honors `--break-pc`/`--stop-on-watch`. It streams
//! straight to the output writer, so an instruction trace is unbounded by
//! design (never materialized in memory).
//!
//! Both loops emit human `text` or machine-diffable `jsonl`, and `--from-frame
//! N` seeks cheaply (runs fast to N with the observers off, then arms them) so
//! long runs need not emit from frame 0.

use std::collections::VecDeque;
use std::io::{BufWriter, Write};
use std::path::Path;

use clap::ValueEnum;
use phosphor_core::core::debug::DebugRegister;
use phosphor_core::core::debug_hang::{HangDetector, HangReport};
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind};
use phosphor_core::core::machine::FrontendMachine;
use phosphor_core::core::watchpoint::{
    DebugAccessSource, WatchpointCondition, WatchpointHit, WatchpointKind,
};
use phosphor_core::cpu::disasm::DisassembledInstruction;

use phosphor_harness::{Harness, PressSpec};

/// Output serialization for a trace run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum TraceFormat {
    /// One human-readable, columnar line per record.
    Text,
    /// One JSON object per line (greppable / diffable).
    Jsonl,
}

/// A parsed `--watch cpu:addr:kind[:cond]` request (may set two watchpoints
/// for `rw`; both share the condition).
struct WatchSpec {
    cpu: usize,
    addr: u32,
    read: bool,
    write: bool,
    condition: WatchpointCondition,
}

/// One observed record, tagged by which stream produced it.
///
/// Both variants carry a machine cycle; [`Self::cycle`] drives the merge sort.
enum Record {
    Event(DebugEvent),
    Watch(WatchpointHit),
}

impl Record {
    fn cycle(&self) -> u64 {
        match self {
            Record::Event(e) => e.cycle,
            Record::Watch(h) => h.cycle,
        }
    }
}

/// How many recent event bodies to carry as a hang report's context tail.
const HANG_CONTEXT: usize = 6;

/// Per-run hang detection: the [`HangDetector`] plus a rolling buffer of recent
/// event bodies (the "immediate causal context" the Dig Dug hunt relied on).
struct HangWatch {
    detector: HangDetector,
    recent: VecDeque<String>,
    format: TraceFormat,
}

impl HangWatch {
    fn new(format: TraceFormat) -> Self {
        Self {
            detector: HangDetector::new(),
            recent: VecDeque::new(),
            format,
        }
    }

    /// Record a recently-traced event body for the context tail (only called
    /// when `--events` is on).
    fn note_event(&mut self, body: String) {
        if self.recent.len() >= HANG_CONTEXT {
            self.recent.pop_front();
        }
        self.recent.push_back(body);
    }

    /// Sample every CPU's PC for `frame` and return a formatted line for each
    /// CPU that just crossed the hang threshold (usually none).
    fn observe_frame(&mut self, machine: &mut dyn FrontendMachine, frame: u64) -> Vec<String> {
        let snaps: Vec<(usize, u32)> = match machine.debug_bus() {
            Some(bus) => bus
                .cpus()
                .iter()
                .enumerate()
                .map(|(i, (_, cpu))| (i, cpu.debug_pc()))
                .collect(),
            None => return Vec::new(),
        };
        let mut lines = Vec::new();
        for (i, pc) in snaps {
            if let Some(report) = self.detector.observe(i, pc) {
                let regs = machine
                    .debug_bus()
                    .map(|bus| bus.cpus()[i].1.debug_registers())
                    .unwrap_or_default();
                let ctx: Vec<&str> = self.recent.iter().map(String::as_str).collect();
                lines.push(render_hang(&report, &regs, &ctx, frame, self.format));
            }
        }
        lines
    }
}

/// Format a hang report as a text banner or a jsonl object.
fn render_hang(
    report: &HangReport,
    regs: &[DebugRegister],
    context: &[&str],
    frame: u64,
    format: TraceFormat,
) -> String {
    match format {
        TraceFormat::Text => {
            let regcols = fmt_regs_text(regs);
            let mut s = format!(
                "=== HANG: cpu{} stuck in ${:04X}..${:04X} for {} frames \
                 (frame {frame}, pc={:04X} {regcols}) ===",
                report.cpu_index,
                report.window_lo,
                report.window_hi,
                report.frames_stuck,
                report.pc,
            );
            if !context.is_empty() {
                s.push_str(&format!("\n    context: {}", context.join(" | ")));
            }
            s
        }
        TraceFormat::Jsonl => {
            let ctx = context
                .iter()
                .map(|b| json_str(b))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"stream\":\"hang\",\"frame\":{frame},\"cpu\":{},\"pc\":\"{:X}\",\
                 \"window_lo\":\"{:X}\",\"window_hi\":\"{:X}\",\"frames_stuck\":{},\
                 \"regs\":{},\"context\":[{ctx}]}}",
                report.cpu_index,
                report.pc,
                report.window_lo,
                report.window_hi,
                report.frames_stuck,
                fmt_regs_json(regs),
            )
        }
    }
}

/// Boot `machine`, run `frames` frames, and emit the requested trace.
///
/// Two loops, chosen by the requested observers (they need different
/// granularity):
///
/// - **Frame loop** — `--events`/`--watch` only. Runs `run_frame()` per frame
///   and drains the event ring + watchpoint queue; records are merged and
///   emitted cycle-sorted. Bounded memory, cheap.
/// - **Cycle loop** — whenever `--cpu` (instruction trace) or `--break-pc` is
///   requested. Drives `debug_tick()` per cycle, disassembles at instruction
///   boundaries, and honors `--break-pc`/`--stop-on-watch`. Streams straight
///   to the output writer, so an instruction trace is unbounded by design.
///
/// At least one observer must be requested (`--cpu`, `--break-pc`, `--events`,
/// or `--watch`), else there is nothing to report.
#[allow(clippy::too_many_arguments)]
pub fn run_trace(
    machine: &str,
    frames: usize,
    from_frame: usize,
    coin_at: Option<usize>,
    press: Option<&str>,
    nvram: Option<&Path>,
    events: Option<&str>,
    watch: Option<&str>,
    cpu: Option<&str>,
    break_pc: Option<&str>,
    stop_on_watch: bool,
    hang: bool,
    stop_on_hang: bool,
    format: TraceFormat,
    out: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    let cycle_mode = cpu.is_some() || break_pc.is_some();
    if events.is_none() && watch.is_none() && !cycle_mode && !hang {
        return Err(
            "trace needs at least one observer: --cpu, --break-pc, --events, --watch, or --hang"
                .to_string(),
        );
    }
    if from_frame > frames {
        return Err(format!(
            "--from-frame {from_frame} is past --frames {frames}"
        ));
    }

    // Parse every spec up front so bad grammar fails before booting.
    let event_filter = events.map(parse_event_kinds).transpose()?;
    let watch_specs = match watch {
        Some(spec) => parse_watch_specs(spec)?,
        None => Vec::new(),
    };
    let cpu_specs = match cpu {
        Some(spec) => parse_cpu_specs(spec)?,
        None => Vec::new(),
    };
    let break_specs = match break_pc {
        Some(spec) => parse_break_specs(spec)?,
        None => Vec::new(),
    };
    let press_specs = match press {
        Some(spec) => parse_press_specs(spec)?,
        None => Vec::new(),
    };

    let mut harness = Harness::build(machine, path, nvram, coin_at, &press_specs)?;
    let cycles_per_frame = harness.machine_mut().cycles_per_frame();

    // Resolve CPU names/indices against the booted machine's bus (cycle mode).
    let traced = resolve_cpu_specs(&mut harness, &cpu_specs)?;
    let breaks = resolve_break_specs(&mut harness, &break_specs)?;

    // Seek: run fast to the observation window with observers still off.
    for _ in 0..from_frame {
        harness.run_frame();
    }

    // Arm observers at the start of the window.
    if event_filter.is_some() {
        harness.machine_mut().set_trace_enabled(true);
    }
    for w in &watch_specs {
        if w.read {
            harness.machine_mut().set_watchpoint_cond(
                w.cpu,
                w.addr,
                WatchpointKind::Read,
                w.condition,
            );
        }
        if w.write {
            harness.machine_mut().set_watchpoint_cond(
                w.cpu,
                w.addr,
                WatchpointKind::Write,
                w.condition,
            );
        }
    }

    let hang_watch = hang.then(|| HangWatch::new(format));

    if cycle_mode {
        run_cycle_loop(CycleLoop {
            harness: &mut harness,
            from_frame,
            frames,
            cycles_per_frame,
            traced: &traced,
            breaks: &breaks,
            stop_on_watch,
            hang: hang_watch,
            stop_on_hang,
            event_filter: &event_filter,
            format,
            out,
        })
    } else {
        run_frame_loop(FrameLoop {
            harness: &mut harness,
            from_frame,
            frames,
            cycles_per_frame,
            event_filter: &event_filter,
            hang: hang_watch,
            stop_on_hang,
            format,
            out,
        })
    }
}

/// Parameters for [`run_frame_loop`] (grouped to keep the arg list sane).
struct FrameLoop<'a> {
    harness: &'a mut Harness,
    from_frame: usize,
    frames: usize,
    cycles_per_frame: u64,
    event_filter: &'a Option<EventFilter>,
    hang: Option<HangWatch>,
    stop_on_hang: bool,
    format: TraceFormat,
    out: Option<&'a Path>,
}

/// The frame loop: run whole frames and collect the event ring + watchpoint
/// hits, emitting them cycle-sorted. Bounded memory. Hang detection (if on)
/// samples each CPU's PC per frame; reports are appended after the trace.
fn run_frame_loop(fl: FrameLoop) -> Result<String, String> {
    let FrameLoop {
        harness,
        from_frame,
        frames,
        cycles_per_frame,
        event_filter,
        mut hang,
        stop_on_hang,
        format,
        out,
    } = fl;

    // Watchpoint hits are drained every frame (the pending queue is shallow);
    // trace events are drained and cleared every frame so a run longer than the
    // ring capacity does not silently lose early events.
    let mut records: Vec<Record> = Vec::new();
    let mut hang_lines: Vec<String> = Vec::new();
    for frame_idx in from_frame..frames {
        harness.run_frame();

        while let Some(hit) = harness.machine_mut().take_watchpoint_hit() {
            records.push(Record::Watch(hit));
        }

        if let Some(filter) = event_filter {
            let machine = harness.machine_mut();
            for &e in machine.trace_events() {
                if filter.accepts(e.kind) {
                    if let Some(h) = hang.as_mut() {
                        h.note_event(event_body(&e));
                    }
                    records.push(Record::Event(e));
                }
            }
            machine.clear_trace_events();
        }

        if let Some(h) = hang.as_mut() {
            let lines = h.observe_frame(harness.machine_mut(), frame_idx as u64);
            let hit = !lines.is_empty();
            hang_lines.extend(lines);
            if hit && stop_on_hang {
                break;
            }
        }
    }

    // Stable merge by cycle; equal cycles keep collection order (events and
    // hits recorded in the same cycle stay in the order the board produced).
    records.sort_by_key(|r| r.cycle());

    let mut body = render(&records, cycles_per_frame, format);
    for line in &hang_lines {
        body.push_str(line);
        body.push('\n');
    }

    match out {
        Some(p) => {
            std::fs::write(p, &body).map_err(|e| format!("writing {}: {e}", p.display()))?;
            Ok(format!(
                "trace: {} record(s), {} hang report(s) over frames {from_frame}..{frames} -> {}\n",
                records.len(),
                hang_lines.len(),
                p.display()
            ))
        }
        None => Ok(body),
    }
}

// ---------------------------------------------------------------------------
// Cycle loop (instruction trace + break-pc)
// ---------------------------------------------------------------------------

/// A CPU selected for instruction tracing, with whether to append registers.
struct TracedCpu {
    index: usize,
    regs: bool,
}

/// Parameters for [`run_cycle_loop`] (grouped to keep the arg list sane).
struct CycleLoop<'a> {
    harness: &'a mut Harness,
    from_frame: usize,
    frames: usize,
    cycles_per_frame: u64,
    traced: &'a [TracedCpu],
    breaks: &'a [(usize, u32)],
    stop_on_watch: bool,
    hang: Option<HangWatch>,
    stop_on_hang: bool,
    event_filter: &'a Option<EventFilter>,
    format: TraceFormat,
    out: Option<&'a Path>,
}

/// Max instruction length across all supported CPUs (M68000 `MOVE.L #,abs.l`).
const MAX_INSN: usize = 10;

/// The cycle loop: drive `debug_tick()` per cycle, disassemble at instruction
/// boundaries for traced CPUs, log watchpoint hits and (optionally) events, and
/// honor `--break-pc`/`--stop-on-watch`. Streams line-by-line to `out` (or
/// stdout), so an instruction trace is never materialized in memory.
fn run_cycle_loop(cl: CycleLoop) -> Result<String, String> {
    let CycleLoop {
        harness,
        from_frame,
        frames,
        cycles_per_frame,
        traced,
        breaks,
        stop_on_watch,
        mut hang,
        stop_on_hang,
        event_filter,
        format,
        out,
    } = cl;

    if cycles_per_frame == 0 {
        return Err(
            "machine reports 0 cycles/frame; the cycle loop (--cpu/--break-pc) is unavailable"
                .to_string(),
        );
    }

    // Stream to a file or stdout; BufWriter keeps per-line writes cheap.
    let mut sink: Box<dyn Write> = match out {
        Some(p) => Box::new(BufWriter::new(
            std::fs::File::create(p).map_err(|e| format!("creating {}: {e}", p.display()))?,
        )),
        None => Box::new(BufWriter::new(std::io::stdout())),
    };

    // The machine clock after seeking `from_frame` whole frames. Each
    // `debug_tick()` advances it by one; frames derive from it via /cpf.
    let start_cycle = from_frame as u64 * cycles_per_frame;
    let total_ticks = (frames - from_frame) as u64 * cycles_per_frame;
    let mut emitted = 0usize;

    for tick in 0..total_ticks {
        let cycle = start_cycle + tick;
        let boundaries = harness.machine_mut().debug_tick();
        let frame = cycle / cycles_per_frame;

        // Lines produced this cycle, in a stable sub-cycle order: instruction
        // boundaries first, then watchpoint hits, then events.
        let mut lines: Vec<String> = Vec::new();
        let mut hit_break = false;

        // Instruction trace + break-pc need the (immutable) debug bus; scope
        // that borrow so the mutable `take_watchpoint_hit()` below is free.
        if boundaries != 0
            && let Some(bus) = harness.machine_mut().debug_bus()
        {
            let cpus = bus.cpus();
            for t in traced {
                if t.index < cpus.len() && (boundaries >> t.index) & 1 != 0 {
                    let cpu = cpus[t.index].1;
                    let pc = cpu.debug_pc();
                    let mut bytes = [0u8; MAX_INSN];
                    for (i, b) in bytes.iter_mut().enumerate() {
                        *b = bus.read(t.index, pc.wrapping_add(i as u32)).unwrap_or(0);
                    }
                    let insn = cpu.debug_disassemble(pc, &bytes);
                    let regs = t.regs.then(|| cpu.debug_registers());
                    lines.push(render_instr(
                        format,
                        frame,
                        cycle,
                        t.index,
                        pc,
                        &insn,
                        regs.as_deref(),
                    ));
                }
            }
            for &(bi, baddr) in breaks {
                if bi < cpus.len() && (boundaries >> bi) & 1 != 0 && cpus[bi].1.debug_pc() == baddr
                {
                    hit_break = true;
                }
            }
        }

        // Watchpoint hits queued during this tick.
        let mut watch_fired = false;
        while let Some(hit) = harness.machine_mut().take_watchpoint_hit() {
            watch_fired = true;
            let f = hit.cycle / cycles_per_frame;
            lines.push(render_record(&Record::Watch(hit), f, format));
        }

        // Events recorded during this tick (drain + clear the ring).
        if let Some(filter) = event_filter {
            let machine = harness.machine_mut();
            let mut evlines: Vec<String> = Vec::new();
            let mut ev_bodies: Vec<String> = Vec::new();
            for &e in machine.trace_events() {
                if filter.accepts(e.kind) {
                    let f = e.cycle / cycles_per_frame;
                    if hang.is_some() {
                        ev_bodies.push(event_body(&e));
                    }
                    evlines.push(render_record(&Record::Event(e), f, format));
                }
            }
            machine.clear_trace_events();
            lines.extend(evlines);
            if let Some(h) = hang.as_mut() {
                for body in ev_bodies {
                    h.note_event(body);
                }
            }
        }

        for line in &lines {
            writeln!(sink, "{line}").map_err(|e| format!("writing trace: {e}"))?;
        }
        emitted += lines.len();

        // Hang detection samples per-CPU PC at frame boundaries (last cycle of
        // a frame). The completed frame index is `cycle / cpf`.
        let mut hang_fired = false;
        if (cycle + 1).is_multiple_of(cycles_per_frame)
            && let Some(h) = hang.as_mut()
        {
            let frame_done = cycle / cycles_per_frame;
            for line in h.observe_frame(harness.machine_mut(), frame_done) {
                writeln!(sink, "{line}").map_err(|e| format!("writing trace: {e}"))?;
                emitted += 1;
                hang_fired = true;
            }
        }

        if hit_break || (stop_on_watch && watch_fired) || (stop_on_hang && hang_fired) {
            break;
        }
    }

    sink.flush().map_err(|e| format!("flushing trace: {e}"))?;

    let dest = out
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "stdout".to_string());
    Ok(format!(
        "trace: {emitted} line(s) over frames {from_frame}..{frames} -> {dest}\n"
    ))
}

// ---------------------------------------------------------------------------
// Observer-spec parsing
// ---------------------------------------------------------------------------

/// Which event kinds to keep (`All`, or an explicit set).
enum EventFilter {
    All,
    Some(Vec<DebugEventKind>),
}

impl EventFilter {
    fn accepts(&self, kind: DebugEventKind) -> bool {
        match self {
            EventFilter::All => true,
            EventFilter::Some(kinds) => kinds.contains(&kind),
        }
    }
}

/// Parse a `--events` value: `all`, or a comma-separated list of kind tokens.
fn parse_event_kinds(spec: &str) -> Result<EventFilter, String> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("all") {
        return Ok(EventFilter::All);
    }
    let mut kinds = Vec::new();
    for tok in spec.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        kinds.push(parse_event_kind(tok).ok_or_else(|| {
            format!(
                "unknown event kind '{tok}'; valid: {}, or 'all'",
                EVENT_TOKENS
            )
        })?);
    }
    if kinds.is_empty() {
        return Err("--events was empty; give kind tokens or 'all'".to_string());
    }
    Ok(EventFilter::Some(kinds))
}

/// Space-free CLI tokens for each [`DebugEventKind`] (the enum's `label()` has
/// spaces, so it isn't usable as a CLI token).
const EVENT_TOKENS: &str = "memread, memwrite, ioread, iowrite, devread, devwrite, \
irqassert, irqclear, irqack, dmaread, dmawrite, bank, watchdog, scanline, halt, resume, message";

fn parse_event_kind(tok: &str) -> Option<DebugEventKind> {
    use DebugEventKind::*;
    Some(match tok.to_ascii_lowercase().as_str() {
        "memread" => MemoryRead,
        "memwrite" => MemoryWrite,
        "ioread" => IoRead,
        "iowrite" => IoWrite,
        "devread" => DeviceRead,
        "devwrite" => DeviceWrite,
        "irqassert" => InterruptAssert,
        "irqclear" => InterruptClear,
        "irqack" => InterruptAck,
        "dmaread" => DmaRead,
        "dmawrite" => DmaWrite,
        "bank" => BankSwitch,
        "watchdog" => Watchdog,
        "scanline" => Scanline,
        "halt" => CpuHalt,
        "resume" => CpuResume,
        "message" => Message,
        _ => return None,
    })
}

/// Parse a `--watch` value: comma-separated `cpu:addr:kind` specs.
fn parse_watch_specs(spec: &str) -> Result<Vec<WatchSpec>, String> {
    let mut specs = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        specs.push(parse_watch_spec(part)?);
    }
    if specs.is_empty() {
        return Err("--watch was empty; give one or more cpu:addr:kind specs".to_string());
    }
    Ok(specs)
}

/// Parse one `cpu:addr:kind[:cond]` spec (kind = `r`, `w`, or `rw`).
///
/// The optional 4th field is a value condition: `=HEX` (equals), `&MASK=HEX`
/// (bit test), or `chg` (changed); absent means fire on any access.
fn parse_watch_spec(part: &str) -> Result<WatchSpec, String> {
    let fields: Vec<&str> = part.split(':').collect();
    if fields.len() < 3 || fields.len() > 4 {
        return Err(format!(
            "bad watch spec '{part}'; expected cpu:addr:kind[:cond] (e.g. 0:0x87cf:w or 0:0x4000:w:=4E5F)"
        ));
    }
    let cpu = fields[0]
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("bad cpu index '{}' in watch spec '{part}'", fields[0]))?;
    let addr = crate::parse_u32_auto(fields[1])
        .map_err(|e| format!("bad address in watch spec '{part}': {e}"))?;
    let (read, write) = match fields[2].trim().to_ascii_lowercase().as_str() {
        "r" => (true, false),
        "w" => (false, true),
        "rw" | "wr" => (true, true),
        other => {
            return Err(format!(
                "bad kind '{other}' in watch spec '{part}'; expected r, w, or rw"
            ));
        }
    };
    let condition = match fields.get(3) {
        Some(cond) => parse_condition(cond)
            .map_err(|e| format!("bad condition in watch spec '{part}': {e}"))?,
        None => WatchpointCondition::Always,
    };
    Ok(WatchSpec {
        cpu,
        addr,
        read,
        write,
        condition,
    })
}

/// Hex-only parse (optional `0x`) for condition operands, which are always hex.
fn parse_cond_hex(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let digits = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u32::from_str_radix(digits, 16).map_err(|_| format!("invalid hex '{s}'"))
}

/// Parse a watchpoint condition: `=HEX` equals, `&MASK=HEX` bit test, `chg`
/// changed. Operands are hex (with or without a `0x` prefix).
fn parse_condition(spec: &str) -> Result<WatchpointCondition, String> {
    let s = spec.trim();
    if s.eq_ignore_ascii_case("chg") || s.eq_ignore_ascii_case("changed") {
        return Ok(WatchpointCondition::Changed);
    }
    if let Some(rest) = s.strip_prefix('=') {
        return Ok(WatchpointCondition::Equals(parse_cond_hex(rest)?));
    }
    if let Some(rest) = s.strip_prefix('&') {
        let (mask, expected) = rest
            .split_once('=')
            .ok_or_else(|| format!("bad bits condition '{s}'; expected &MASK=HEX"))?;
        return Ok(WatchpointCondition::Bits {
            mask: parse_cond_hex(mask)?,
            expected: parse_cond_hex(expected)?,
        });
    }
    Err(format!(
        "bad condition '{s}'; expected =HEX, &MASK=HEX, or chg"
    ))
}

/// A `--cpu` entry before name resolution: a CPU token plus a `:regs` flag.
struct CpuSpec {
    token: String,
    regs: bool,
}

/// Parse a `--cpu` value: comma-separated `<name|idx>[:regs]` entries.
fn parse_cpu_specs(spec: &str) -> Result<Vec<CpuSpec>, String> {
    let mut specs = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (token, regs) = match part.rsplit_once(':') {
            Some((head, tail)) if tail.eq_ignore_ascii_case("regs") => (head.trim(), true),
            _ => (part, false),
        };
        if token.is_empty() {
            return Err(format!(
                "bad --cpu entry '{part}'; expected <name|idx>[:regs]"
            ));
        }
        specs.push(CpuSpec {
            token: token.to_string(),
            regs,
        });
    }
    if specs.is_empty() {
        return Err("--cpu was empty; give one or more <name|idx>[:regs] entries".to_string());
    }
    Ok(specs)
}

/// Parse a `--press` value: comma-separated `<control>@<frame>[:<hold>]`
/// entries (e.g. `fire1@120`, `coin@60:8`). `control` is a machine's stable
/// input name; `hold` frames defaults to the harness default.
fn parse_press_specs(spec: &str) -> Result<Vec<PressSpec>, String> {
    let mut specs = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (control, rest) = part
            .split_once('@')
            .ok_or_else(|| format!("bad --press entry '{part}'; expected control@frame[:hold]"))?;
        let (at_str, hold) = match rest.split_once(':') {
            Some((a, h)) => (
                a,
                h.trim()
                    .parse::<usize>()
                    .map_err(|_| format!("bad hold '{h}' in --press entry '{part}'"))?,
            ),
            None => (rest, 8),
        };
        let at = at_str
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("bad frame '{at_str}' in --press entry '{part}'"))?;
        let control = control.trim();
        if control.is_empty() {
            return Err(format!("bad --press entry '{part}'; empty control name"));
        }
        specs.push(PressSpec {
            control: control.to_string(),
            at,
            hold,
        });
    }
    if specs.is_empty() {
        return Err("--press was empty; give one or more control@frame[:hold] entries".to_string());
    }
    Ok(specs)
}

/// Parse a `--break-pc` value: comma-separated `<cpu>:<addr>` entries.
fn parse_break_specs(spec: &str) -> Result<Vec<(String, u32)>, String> {
    let mut specs = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (cpu, addr) = part
            .split_once(':')
            .ok_or_else(|| format!("bad --break-pc entry '{part}'; expected cpu:addr"))?;
        let addr = crate::parse_u32_auto(addr)
            .map_err(|e| format!("bad address in --break-pc entry '{part}': {e}"))?;
        specs.push((cpu.trim().to_string(), addr));
    }
    if specs.is_empty() {
        return Err("--break-pc was empty; give one or more cpu:addr entries".to_string());
    }
    Ok(specs)
}

/// Snapshot the booted machine's CPU names (owned, so the bus borrow ends).
fn cpu_names(harness: &mut Harness) -> Result<Vec<String>, String> {
    let bus = harness
        .machine_mut()
        .debug_bus()
        .ok_or("machine exposes no debug bus (cannot resolve --cpu/--break-pc)")?;
    Ok(bus.cpus().iter().map(|(n, _)| n.to_string()).collect())
}

/// Resolve a CPU token (decimal index or case-insensitive name) to an index.
fn resolve_cpu_token(token: &str, names: &[String]) -> Result<usize, String> {
    if let Ok(idx) = token.parse::<usize>() {
        return if idx < names.len() {
            Ok(idx)
        } else {
            Err(format!(
                "cpu index {idx} out of range (machine has {} CPU(s))",
                names.len()
            ))
        };
    }
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(token))
        .ok_or_else(|| format!("unknown cpu '{token}'; available: {}", names.join(", ")))
}

fn resolve_cpu_specs(harness: &mut Harness, specs: &[CpuSpec]) -> Result<Vec<TracedCpu>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let names = cpu_names(harness)?;
    specs
        .iter()
        .map(|s| {
            resolve_cpu_token(&s.token, &names).map(|index| TracedCpu {
                index,
                regs: s.regs,
            })
        })
        .collect()
}

fn resolve_break_specs(
    harness: &mut Harness,
    specs: &[(String, u32)],
) -> Result<Vec<(usize, u32)>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let names = cpu_names(harness)?;
    specs
        .iter()
        .map(|(tok, addr)| resolve_cpu_token(tok, &names).map(|idx| (idx, *addr)))
        .collect()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(records: &[Record], cycles_per_frame: u64, format: TraceFormat) -> String {
    let mut out = String::new();
    for r in records {
        let frame = frame_of(r.cycle(), cycles_per_frame);
        match format {
            TraceFormat::Text => out.push_str(&render_text(r, frame)),
            TraceFormat::Jsonl => out.push_str(&render_jsonl(r, frame)),
        }
        out.push('\n');
    }
    out
}

/// Frame index containing `cycle` (0 when the machine reports no frame length).
fn frame_of(cycle: u64, cycles_per_frame: u64) -> u64 {
    cycle.checked_div(cycles_per_frame).unwrap_or(0)
}

/// Render one event/watch record in the requested format (no trailing newline).
fn render_record(record: &Record, frame: u64, format: TraceFormat) -> String {
    match format {
        TraceFormat::Text => render_text(record, frame),
        TraceFormat::Jsonl => render_jsonl(record, frame),
    }
}

/// Register columns for an instruction line, e.g. `A=42 X=1BCC`.
fn fmt_regs_text(regs: &[DebugRegister]) -> String {
    regs.iter()
        .map(|r| {
            let digits = (r.width.max(4) as usize).div_ceil(4);
            format!("{}={:0digits$X}", r.name, r.value)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Registers as a JSON object, e.g. `{"A":"42","X":"1BCC"}`.
fn fmt_regs_json(regs: &[DebugRegister]) -> String {
    let fields: Vec<String> = regs
        .iter()
        .map(|r| {
            let digits = (r.width.max(4) as usize).div_ceil(4);
            format!("{}:\"{:0digits$X}\"", json_str(r.name), r.value)
        })
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// An instruction-trace line for a CPU at an instruction boundary.
fn render_instr(
    format: TraceFormat,
    frame: u64,
    cycle: u64,
    cpu_index: usize,
    pc: u32,
    insn: &DisassembledInstruction,
    regs: Option<&[DebugRegister]>,
) -> String {
    let raw = &insn.bytes[..insn.byte_len.min(insn.bytes.len() as u8) as usize];
    let hex = raw
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let disasm = format!("{insn}");
    match format {
        TraceFormat::Text => {
            let mut line = format!(
                "frame {frame:<5} cyc {cycle:<10} cpu{cpu_index} pc={pc:04X}  {hex:<14} {disasm}"
            );
            if let Some(regs) = regs {
                line.push_str(&format!("  {}", fmt_regs_text(regs)));
            }
            line.push_str("  ; instr");
            line
        }
        TraceFormat::Jsonl => {
            let regs_json = match regs {
                Some(regs) => fmt_regs_json(regs),
                None => "null".to_string(),
            };
            format!(
                "{{\"frame\":{frame},\"cycle\":{cycle},\"cpu\":{cpu_index},\"pc\":\"{pc:X}\",\
                 \"stream\":\"instr\",\"bytes\":{},\"text\":{},\"regs\":{regs_json}}}",
                json_str(&hex.replace(' ', "")),
                json_str(&disasm),
            )
        }
    }
}

/// `cpuN` label for a source/cpu-index pair.
fn cpu_label(cpu_index: Option<usize>) -> String {
    match cpu_index {
        Some(i) => format!("cpu{i}"),
        None => "cpu-".to_string(),
    }
}

fn pc_col(pc: Option<u32>) -> String {
    match pc {
        Some(p) => format!("pc={p:04X}"),
        None => "pc=----".to_string(),
    }
}

/// Hex-format a value using its access width (2 hex digits per byte).
fn fmt_value(value: u32, width: u8) -> String {
    let digits = (width.max(1) as usize) * 2;
    format!("{value:0digits$X}")
}

/// The semantic body of an event, without the frame/cycle/cpu/pc columns
/// (e.g. `bank $C900=$01 [ROM Bank] banked ROM mapped at $0000-$8FFF`).
fn event_body(e: &DebugEvent) -> String {
    let mut body = e.kind.label().to_string();
    if let Some(addr) = e.addr {
        body.push_str(&format!(" ${addr:04X}"));
        if e.width > 0
            && let Some(v) = e.value
        {
            body.push_str(&format!("=${}", fmt_value(v, e.width)));
        }
    }
    if let Some(region) = e.region {
        body.push_str(&format!(" [{region}]"));
    }
    if let Some(device) = e.device {
        body.push_str(&format!(" ({device})"));
    }
    if let Some(detail) = e.detail {
        body.push_str(&format!(" {detail}"));
    }
    body
}

fn watch_kind_label(kind: WatchpointKind) -> &'static str {
    match kind {
        WatchpointKind::Read => "mem rd",
        WatchpointKind::Write => "mem wr",
    }
}

/// The semantic body of a watchpoint hit (e.g. `mem wr $87CF=$32 [sharedram]`).
fn watch_body(h: &WatchpointHit) -> String {
    let mut body = format!(
        "{} ${:04X}=${}",
        watch_kind_label(h.kind),
        h.addr,
        fmt_value(h.value, h.width)
    );
    if let Some(region) = h.region {
        body.push_str(&format!(" [{region}]"));
    }
    body
}

fn render_text(record: &Record, frame: u64) -> String {
    match record {
        Record::Event(e) => format!(
            "frame {frame:<5} cyc {:<10} {} {}  {}  ; event",
            e.cycle,
            cpu_label(e.cpu_index),
            pc_col(e.pc),
            event_body(e),
        ),
        Record::Watch(h) => format!(
            "frame {frame:<5} cyc {:<10} {} {}  {}  ; watch",
            h.cycle,
            cpu_label(Some(h.cpu_index)),
            pc_col(h.pc),
            watch_body(h),
        ),
    }
}

/// JSON-escape a string for embedding in a jsonl line.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `Some(hex)` → `"XXXX"`, `None` → `null`.
fn json_hex(v: Option<u32>) -> String {
    match v {
        Some(v) => format!("\"{v:X}\""),
        None => "null".to_string(),
    }
}

fn json_opt_str(s: Option<&str>) -> String {
    match s {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

fn render_jsonl(record: &Record, frame: u64) -> String {
    match record {
        Record::Event(e) => format!(
            "{{\"frame\":{frame},\"cycle\":{},\"cpu\":{},\"pc\":{},\"stream\":\"event\",\
             \"kind\":{},\"addr\":{},\"value\":{},\"width\":{},\"region\":{},\"device\":{},\"text\":{}}}",
            e.cycle,
            json_cpu(e.cpu_index, e.source),
            json_hex(e.pc),
            json_str(e.kind.label()),
            json_hex(e.addr),
            json_hex(e.value),
            e.width,
            json_opt_str(e.region),
            json_opt_str(e.device),
            json_str(&event_body(e)),
        ),
        Record::Watch(h) => format!(
            "{{\"frame\":{frame},\"cycle\":{},\"cpu\":{},\"pc\":{},\"stream\":\"watch\",\
             \"kind\":{},\"addr\":{},\"value\":{},\"width\":{},\"region\":{},\"device\":null,\"text\":{}}}",
            h.cycle,
            h.cpu_index,
            json_hex(h.pc),
            json_str(watch_kind_label(h.kind)),
            json_hex(Some(h.addr)),
            json_hex(Some(h.value)),
            h.width,
            json_opt_str(h.region),
            json_str(&watch_body(h)),
        ),
    }
}

/// CPU index for jsonl: prefer the event's explicit `cpu_index`, else the CPU
/// carried by the access source, else `null`.
fn json_cpu(cpu_index: Option<usize>, source: DebugAccessSource) -> String {
    let idx = cpu_index.or(match source {
        DebugAccessSource::Cpu(i) => Some(i),
        _ => None,
    });
    match idx {
        Some(i) => i.to_string(),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::watchpoint::WatchpointPhase;

    #[test]
    fn watch_spec_kinds() {
        let r = parse_watch_spec("0:0x87cf:w").unwrap();
        assert_eq!(r.cpu, 0);
        assert_eq!(r.addr, 0x87CF);
        assert!(!r.read && r.write);

        let rw = parse_watch_spec("1:0x4000:rw").unwrap();
        assert_eq!(rw.cpu, 1);
        assert_eq!(rw.addr, 0x4000);
        assert!(rw.read && rw.write);

        // Bare numbers parse as decimal (matching the rest of the CLI).
        assert_eq!(parse_watch_spec("0:4000:r").unwrap().addr, 4000);

        // No condition field defaults to Always.
        assert_eq!(r.condition, WatchpointCondition::Always);

        assert!(parse_watch_spec("0:0x10").is_err()); // too few fields
        assert!(parse_watch_spec("x:0x10:r").is_err()); // bad cpu
        assert!(parse_watch_spec("0:zz:r").is_err()); // bad addr
        assert!(parse_watch_spec("0:0x10:x").is_err()); // bad kind
        assert!(parse_watch_spec("0:0x10:w:junk").is_err()); // bad condition
        assert!(parse_watch_spec("0:0x10:w:=1:extra").is_err()); // too many fields
    }

    #[test]
    fn watch_spec_conditions() {
        // Condition operands are hex (no 0x needed).
        assert_eq!(
            parse_watch_spec("0:0x4000:w:=4E5F").unwrap().condition,
            WatchpointCondition::Equals(0x4E5F)
        );
        assert_eq!(
            parse_watch_spec("0:0x20:w:&F0=50").unwrap().condition,
            WatchpointCondition::Bits {
                mask: 0xF0,
                expected: 0x50
            }
        );
        assert_eq!(
            parse_watch_spec("1:0x20:w:chg").unwrap().condition,
            WatchpointCondition::Changed
        );
        // Direct parse_condition coverage.
        assert_eq!(
            parse_condition("=0x1f").unwrap(),
            WatchpointCondition::Equals(0x1F)
        );
        assert!(parse_condition("&F0").is_err()); // missing =EXPECTED
        assert!(parse_condition("=zz").is_err()); // bad hex
        assert!(parse_condition("nope").is_err());
    }

    #[test]
    fn event_kinds_all_and_list() {
        assert!(matches!(
            parse_event_kinds("all").unwrap(),
            EventFilter::All
        ));
        let f = parse_event_kinds("devwrite, bank ,watchdog").unwrap();
        assert!(f.accepts(DebugEventKind::DeviceWrite));
        assert!(f.accepts(DebugEventKind::BankSwitch));
        assert!(!f.accepts(DebugEventKind::MemoryRead));
        assert!(parse_event_kinds("nope").is_err());
        assert!(parse_event_kinds("").is_err());
    }

    #[test]
    fn text_render_event_and_watch() {
        let e = DebugEvent {
            addr: Some(0xC900),
            value: Some(0x01),
            width: 1,
            cpu_index: Some(0),
            pc: Some(0x1BCC),
            region: Some("sharedram"),
            ..DebugEvent::new(
                12_694_000,
                DebugAccessSource::Cpu(0),
                DebugEventKind::BankSwitch,
            )
        };
        let line = render_text(&Record::Event(e), 3100);
        assert!(line.contains("frame 3100"), "{line}");
        assert!(line.contains("cpu0"), "{line}");
        assert!(line.contains("pc=1BCC"), "{line}");
        assert!(line.contains("$C900=$01"), "{line}");
        assert!(line.contains("[sharedram]"), "{line}");
        assert!(line.trim_end().ends_with("; event"), "{line}");

        let h = WatchpointHit {
            cpu_index: 0,
            source: DebugAccessSource::Cpu(0),
            cycle: 12_694_104,
            pc: Some(0x0066),
            addr: 0x87CF,
            kind: WatchpointKind::Write,
            phase: WatchpointPhase::Before,
            value: 0x32,
            width: 1,
            region: Some("sharedram"),
            device: None,
        };
        let line = render_text(&Record::Watch(h), 3100);
        assert!(line.contains("mem wr $87CF=$32 [sharedram]"), "{line}");
        assert!(line.trim_end().ends_with("; watch"), "{line}");
    }

    #[test]
    fn jsonl_render_has_structured_fields() {
        let h = WatchpointHit {
            cpu_index: 1,
            source: DebugAccessSource::Cpu(1),
            cycle: 500,
            pc: None,
            addr: 0x4E5F,
            kind: WatchpointKind::Write,
            phase: WatchpointPhase::Before,
            value: 0xAB,
            width: 1,
            region: None,
            device: None,
        };
        let line = render_jsonl(&Record::Watch(h), 2);
        assert!(line.contains("\"frame\":2"), "{line}");
        assert!(line.contains("\"cycle\":500"), "{line}");
        assert!(line.contains("\"cpu\":1"), "{line}");
        assert!(line.contains("\"pc\":null"), "{line}");
        assert!(line.contains("\"addr\":\"4E5F\""), "{line}");
        assert!(line.contains("\"value\":\"AB\""), "{line}");
        assert!(line.contains("\"stream\":\"watch\""), "{line}");
        assert!(line.contains("\"region\":null"), "{line}");
        // Well-formed enough to not contain a dangling brace/quote.
        assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
    }

    #[test]
    fn width_widens_value_hex() {
        assert_eq!(fmt_value(0x1234_5678, 4), "12345678");
        assert_eq!(fmt_value(0x05, 1), "05");
        assert_eq!(fmt_value(0x05, 0), "05"); // width 0 treated as 1
    }

    #[test]
    fn bad_observer_and_range_args_rejected() {
        // No observer requested.
        assert!(
            run_trace(
                "joust",
                1,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                false,
                false,
                false,
                TraceFormat::Text,
                None,
                "."
            )
            .is_err()
        );
        // from-frame past frames (fails before any boot / ROM load).
        assert!(
            run_trace(
                "joust",
                10,
                20,
                None,
                None,
                None,
                Some("all"),
                None,
                None,
                None,
                false,
                false,
                false,
                TraceFormat::Text,
                None,
                "."
            )
            .is_err()
        );
    }

    #[test]
    fn cpu_specs_parse_tokens_and_regs() {
        let s = parse_cpu_specs("0, main:regs ,1").unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].token, "0");
        assert!(!s[0].regs);
        assert_eq!(s[1].token, "main");
        assert!(s[1].regs);
        assert_eq!(s[2].token, "1");
        assert!(!s[2].regs);

        assert!(parse_cpu_specs("").is_err());
        assert!(parse_cpu_specs(":regs").is_err()); // empty token
    }

    #[test]
    fn break_specs_parse_cpu_and_addr() {
        let s = parse_break_specs("0:0xF000, main:4096").unwrap();
        assert_eq!(s[0], ("0".to_string(), 0xF000));
        assert_eq!(s[1], ("main".to_string(), 4096)); // bare = decimal

        assert!(parse_break_specs("0xF000").is_err()); // no colon
        assert!(parse_break_specs("0:zz").is_err()); // bad addr
        assert!(parse_break_specs("").is_err());
    }

    #[test]
    fn press_specs_parse_control_frame_hold() {
        let s = parse_press_specs("fire1@120, coin@60:16").unwrap();
        assert_eq!(s[0].control, "fire1");
        assert_eq!(s[0].at, 120);
        assert_eq!(s[0].hold, 8); // default hold
        assert_eq!(s[1].control, "coin");
        assert_eq!(s[1].at, 60);
        assert_eq!(s[1].hold, 16);

        assert!(parse_press_specs("fire1").is_err()); // no @frame
        assert!(parse_press_specs("fire1@zz").is_err()); // bad frame
        assert!(parse_press_specs("fire1@10:zz").is_err()); // bad hold
        assert!(parse_press_specs("@10").is_err()); // empty control
        assert!(parse_press_specs("").is_err());
    }

    #[test]
    fn resolve_cpu_token_by_index_and_name() {
        let names = vec!["M6809 Main".to_string(), "M6800 Sound".to_string()];
        assert_eq!(resolve_cpu_token("0", &names).unwrap(), 0);
        assert_eq!(resolve_cpu_token("1", &names).unwrap(), 1);
        // Case-insensitive name match.
        assert_eq!(resolve_cpu_token("m6800 sound", &names).unwrap(), 1);
        assert!(resolve_cpu_token("2", &names).is_err()); // out of range
        assert!(resolve_cpu_token("nope", &names).is_err()); // unknown name
    }

    #[test]
    fn instr_render_text_and_jsonl() {
        let mut bytes = [0u8; 10];
        bytes[0] = 0x86;
        bytes[1] = 0x42;
        let insn = DisassembledInstruction {
            mnemonic: "LDA",
            operands: "#$42".to_string(),
            byte_len: 2,
            bytes,
            target_addr: None,
        };
        let regs = [
            DebugRegister {
                name: "A",
                value: 0x42,
                width: 8,
            },
            DebugRegister {
                name: "X",
                value: 0x1BCC,
                width: 16,
            },
        ];

        let text = render_instr(TraceFormat::Text, 5, 100, 0, 0x1000, &insn, Some(&regs));
        assert!(text.contains("frame 5"), "{text}");
        assert!(text.contains("cpu0 pc=1000"), "{text}");
        assert!(text.contains("86 42"), "{text}");
        assert!(text.contains("LDA"), "{text}");
        assert!(text.contains("#$42"), "{text}");
        assert!(text.contains("A=42"), "{text}");
        assert!(text.contains("X=1BCC"), "{text}");
        assert!(text.trim_end().ends_with("; instr"), "{text}");

        let json = render_instr(TraceFormat::Jsonl, 5, 100, 0, 0x1000, &insn, None);
        assert!(json.contains("\"stream\":\"instr\""), "{json}");
        assert!(json.contains("\"pc\":\"1000\""), "{json}");
        assert!(json.contains("\"bytes\":\"8642\""), "{json}");
        assert!(json.contains("\"regs\":null"), "{json}");
        assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
    }

    #[test]
    fn hang_render_text_and_jsonl() {
        // Mirrors the Dig Dug report: cpu0 stuck at 0x1BCC (B=08), $1BC8..$1BD0.
        let report = HangReport {
            cpu_index: 0,
            pc: 0x1BCC,
            window_lo: 0x1BC8,
            window_hi: 0x1BD0,
            frames_stuck: 120,
        };
        let regs = [DebugRegister {
            name: "B",
            value: 0x08,
            width: 8,
        }];
        let ctx = ["wdog", "bank $C900=$01 [ROM Bank]"];

        let text = render_hang(&report, &regs, &ctx, 3100, TraceFormat::Text);
        assert!(text.contains("HANG: cpu0"), "{text}");
        assert!(text.contains("$1BC8..$1BD0"), "{text}");
        assert!(text.contains("for 120 frames"), "{text}");
        assert!(text.contains("pc=1BCC"), "{text}");
        assert!(text.contains("B=08"), "{text}");
        assert!(text.contains("context: wdog | bank $C900=$01"), "{text}");

        let json = render_hang(&report, &regs, &ctx, 3100, TraceFormat::Jsonl);
        assert!(json.contains("\"stream\":\"hang\""), "{json}");
        assert!(json.contains("\"cpu\":0"), "{json}");
        assert!(json.contains("\"pc\":\"1BCC\""), "{json}");
        assert!(json.contains("\"window_lo\":\"1BC8\""), "{json}");
        assert!(json.contains("\"window_hi\":\"1BD0\""), "{json}");
        assert!(json.contains("\"frames_stuck\":120"), "{json}");
        assert!(json.contains("\"context\":["), "{json}");
        assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
    }

    // ---- ROM-gated end-to-end boot test (skips when ROMs are absent) --------

    // The ROM-dir convention lives in phosphor-harness so this and the
    // phosphor-script end-to-end test share one copy.
    use phosphor_harness::roms_dir;

    /// Extract the `"cycle":N` from a jsonl line (test helper).
    fn cycle_of(line: &str) -> u64 {
        let after = line.split("\"cycle\":").nth(1).expect("cycle field");
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().expect("cycle number")
    }

    #[test]
    fn joust_headless_trace_emits_bank_and_device_events_in_cycle_order() {
        let Some(roms) = roms_dir() else {
            eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
            return;
        };
        let path = roms.to_str().unwrap();

        let out = run_trace(
            "joust",
            120,
            0,
            None,
            None,
            None,
            Some("bank,devwrite"),
            None,
            None,
            None,
            false,
            false,
            false,
            TraceFormat::Jsonl,
            None,
            path,
        )
        .expect("trace run");

        let lines: Vec<&str> = out.lines().collect();
        assert!(!lines.is_empty(), "expected some events");

        // Joust maps its banked ROM via a bank-switch on the 0xC900 latch and
        // programs its PIAs via device writes — both must appear.
        assert!(
            lines.iter().any(|l| l.contains("\"kind\":\"bank\"")),
            "expected a bank-switch event:\n{out}"
        );
        assert!(
            lines.iter().any(|l| l.contains("\"kind\":\"dev wr\"")),
            "expected a device-write event:\n{out}"
        );

        // Records are emitted cycle-sorted.
        let cycles: Vec<u64> = lines.iter().map(|l| cycle_of(l)).collect();
        assert!(
            cycles.windows(2).all(|w| w[0] <= w[1]),
            "events must be cycle-sorted: {cycles:?}"
        );
    }

    #[test]
    fn joust_headless_watch_hits_bank_latch() {
        let Some(roms) = roms_dir() else {
            eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
            return;
        };
        let path = roms.to_str().unwrap();

        // The 0xC900 bank latch is written during boot; a write watchpoint on
        // it must fire at least once.
        let out = run_trace(
            "joust",
            30,
            0,
            None,
            None,
            None,
            None,
            Some("0:0xC900:w"),
            None,
            None,
            false,
            false,
            false,
            TraceFormat::Text,
            None,
            path,
        )
        .expect("trace run");

        assert!(
            out.lines()
                .any(|l| l.contains("$C900") && l.trim_end().ends_with("; watch")),
            "expected a watchpoint hit on $C900:\n{out}"
        );
    }

    #[test]
    fn joust_conditional_watch_fires_only_on_matching_value() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let path = roms.to_str().unwrap();

        // During boot the C900 bank latch takes 0x01 then 0x00. An Equals(0x01)
        // condition must fire on the 0x01 write and NOT on the 0x00 write.
        let out = run_trace(
            "joust",
            30,
            0,
            None,
            None,
            None,
            None,
            Some("0:0xC900:w:=01"),
            None,
            None,
            false,
            false,
            false,
            TraceFormat::Text,
            None,
            path,
        )
        .expect("trace run");

        let hits: Vec<&str> = out.lines().filter(|l| l.contains("$C900")).collect();
        assert!(!hits.is_empty(), "the =01 write should fire:\n{out}");
        assert!(
            hits.iter().all(|l| l.contains("$C900=$01")),
            "only the =01 write may fire, not =00:\n{out}"
        );
    }

    #[test]
    fn joust_hang_detection_flags_idle_sound_cpu() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let path = roms.to_str().unwrap();

        // End-to-end: joust's M6800 sound CPU (cpu1) sits in a tight wait loop
        // at $F7F8 from boot, so the 120-frame detector reports it — a real,
        // deterministic detection (and a demonstration of the PC-sampling
        // limitation: an idle wait loop reads the same as a hang). --hang
        // alone is a valid observer, and --stop-on-hang halts at the report.
        let out = run_trace(
            "joust",
            200,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            true, // --hang
            true, // --stop-on-hang
            TraceFormat::Text,
            None,
            path,
        )
        .expect("trace run");

        let hang: Vec<&str> = out.lines().filter(|l| l.contains("=== HANG")).collect();
        assert_eq!(hang.len(), 1, "expected exactly one hang report:\n{out}");
        let line = hang[0];
        assert!(line.contains("cpu1"), "sound CPU should be flagged: {line}");
        assert!(line.contains("pc=F7F8"), "idle loop PC: {line}");
        assert!(line.contains("for 120 frames"), "threshold: {line}");
        assert!(line.contains("SP="), "register columns present: {line}");
    }

    #[test]
    fn from_frame_suppresses_pre_seek_output() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let path = roms.to_str().unwrap();

        // Watching the bank latch, restricting to frames [110, 111): the boot
        // writes at frame 0 must be suppressed, so no record has frame < 110.
        let out = run_trace(
            "joust",
            111,
            110,
            None,
            None,
            None,
            Some("all"),
            None,
            None,
            None,
            false,
            false,
            false,
            TraceFormat::Jsonl,
            None,
            path,
        )
        .expect("trace run");

        for line in out.lines() {
            let after = line.split("\"frame\":").nth(1).expect("frame field");
            let f: u64 = after
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap();
            assert!(f >= 110, "frame {f} leaked before the seek point:\n{line}");
        }
    }

    // ---- ROM-gated cycle-loop (instruction trace) tests ---------------------

    /// Run a cycle-loop trace to a temp file and return its contents (the cycle
    /// loop streams to a writer, so tests read the file it wrote).
    #[allow(clippy::too_many_arguments)]
    fn cycle_trace_to_string(
        tag: &str,
        frames: usize,
        from_frame: usize,
        cpu: Option<&str>,
        break_pc: Option<&str>,
        format: TraceFormat,
        roms: &str,
    ) -> String {
        let p = std::env::temp_dir().join(format!("phosphor_trace_{tag}.txt"));
        run_trace(
            "joust",
            frames,
            from_frame,
            None,
            None,
            None,
            None,
            None,
            cpu,
            break_pc,
            false,
            false,
            false,
            format,
            Some(&p),
            roms,
        )
        .expect("cycle trace run");
        let s = std::fs::read_to_string(&p).expect("read trace file");
        let _ = std::fs::remove_file(&p);
        s
    }

    /// Parse `pc=XXXX` out of a text instruction line.
    fn instr_pc(line: &str) -> u32 {
        let after = line.split("pc=").nth(1).expect("pc field");
        let hex: String = after
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        u32::from_str_radix(&hex, 16).expect("pc hex")
    }

    #[test]
    fn joust_instruction_trace_pc_monotonic_over_reset_block() {
        let Some(roms) = roms_dir() else {
            eprintln!("skipping: no ROM dir");
            return;
        };
        let out = cycle_trace_to_string(
            "mono",
            1,
            0,
            Some("0"),
            None,
            TraceFormat::Text,
            roms.to_str().unwrap(),
        );

        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 6, "expected an instruction stream:\n{out}");
        for l in &lines {
            assert!(l.trim_end().ends_with("; instr"), "not an instr line: {l}");
        }
        // The 6809 reset sequence is straight-line: PC strictly increases over
        // the first handful of instructions (before the first branch/loop).
        let pcs: Vec<u32> = lines.iter().take(6).map(|l| instr_pc(l)).collect();
        assert!(
            pcs.windows(2).all(|w| w[0] < w[1]),
            "reset block PCs must strictly increase: {pcs:04X?}"
        );
    }

    #[test]
    fn joust_break_pc_stops_on_target() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let out = cycle_trace_to_string(
            "brk",
            1,
            0,
            Some("0"),
            Some("0:0xF02B"),
            TraceFormat::Text,
            roms.to_str().unwrap(),
        );
        let last = out.lines().last().expect("at least one line");
        assert_eq!(
            instr_pc(last),
            0xF02B,
            "run must stop on the break PC:\n{out}"
        );
        // The target appears exactly once (we stop the first time we reach it).
        assert_eq!(
            out.lines().filter(|l| instr_pc(l) == 0xF02B).count(),
            1,
            "break target should appear once:\n{out}"
        );
    }

    #[test]
    fn joust_regs_columns_present_with_regs_suffix() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let out = cycle_trace_to_string(
            "regs",
            1,
            0,
            Some("0:regs"),
            Some("0:0xF031"),
            TraceFormat::Text,
            roms.to_str().unwrap(),
        );
        assert!(
            out.lines().all(|l| l.contains("A=") && l.contains("CC=")),
            "every :regs line carries register columns:\n{out}"
        );
    }

    #[test]
    fn joust_multi_cpu_trace_labels_both_cpus() {
        let Some(roms) = roms_dir() else {
            return;
        };
        let out = cycle_trace_to_string(
            "multi",
            1,
            0,
            Some("0,1"),
            None,
            TraceFormat::Text,
            roms.to_str().unwrap(),
        );
        assert!(
            out.lines().any(|l| l.contains("cpu0")),
            "expected cpu0 lines"
        );
        assert!(
            out.lines().any(|l| l.contains("cpu1")),
            "expected cpu1 lines"
        );
    }

    #[test]
    fn joust_cycle_loop_from_frame_suppresses_pre_seek() {
        let Some(roms) = roms_dir() else {
            return;
        };
        // Instruction trace over frames [2, 3): every line's frame must be >= 2.
        let out = cycle_trace_to_string(
            "seek",
            3,
            2,
            Some("0"),
            None,
            TraceFormat::Jsonl,
            roms.to_str().unwrap(),
        );
        assert!(!out.is_empty(), "expected instructions in the window");
        for line in out.lines() {
            let after = line.split("\"frame\":").nth(1).expect("frame field");
            let f: u64 = after
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap();
            assert!(f >= 2, "frame {f} leaked before the seek point:\n{line}");
        }
    }

    // ---- Render-once machines: the debugger's frozen-picture guard ---------

    /// Render the machine's current picture as RGB24 (what the frontend shows).
    fn snapshot(machine: &mut dyn FrontendMachine) -> Vec<u8> {
        let (w, h) = machine.display_size();
        let mut buf = vec![0u8; w as usize * h as usize * 3];
        machine.render_frame(&mut buf);
        buf
    }

    /// Machines that render once per frame used to repaint only inside
    /// `run_frame`. The debugger advances a machine with `debug_tick()` and
    /// never calls `run_frame`, so it showed a frozen picture. Driving one
    /// frame purely with `debug_tick` must reach the same image `run_frame`
    /// produces.
    #[test]
    fn debug_tick_advances_picture_like_run_frame() {
        let Some(roms) = roms_dir() else {
            eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
            return;
        };
        let path = roms.to_str().unwrap();
        // Compare across a span rather than a single frame: attract screens hold
        // a still image for long stretches, and a static comparison would pass
        // even with the bug present. The per-machine warmup is the earliest
        // frame each game's attract sequence is actually animating (measured
        // with `frameshot` + `imgdiff`), keeping the run short.
        const ADVANCE: usize = 30;
        let cases = [
            ("galaga", 60usize),
            ("xevious", 60),
            ("digdug", 60),
            ("burgertime", 800),
            ("qbert", 200),
            ("shollow", 200),
        ];

        for (machine, warmup) in cases {
            let mut reference = Harness::build(machine, path, None, None, &[]).expect("boot");
            for _ in 0..warmup {
                reference.run_frame();
            }
            let before = snapshot(reference.machine_mut());
            for _ in 0..ADVANCE {
                reference.run_frame();
            }
            let after = snapshot(reference.machine_mut());

            // Guard the guard: if the picture never changes over the span, the
            // comparison below would pass even with the bug present.
            assert_ne!(
                before,
                after,
                "{machine}: picture is static over frames {warmup}..{}; pick an \
                 animated span so this test can detect a frozen image",
                warmup + ADVANCE
            );

            let mut subject = Harness::build(machine, path, None, None, &[]).expect("boot");
            for _ in 0..warmup {
                subject.run_frame();
            }
            // Advance the same span using only the debugger's path.
            let cycles = subject.machine_mut().cycles_per_frame();
            for _ in 0..ADVANCE {
                for _ in 0..cycles {
                    subject.machine_mut().debug_tick();
                }
            }

            assert_eq!(
                snapshot(subject.machine_mut()),
                after,
                "{machine}: debug_tick must refresh the displayed picture like run_frame"
            );
        }
    }
}
