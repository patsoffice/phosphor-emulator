//! Headless CPU/bus trace — the frame-loop half of `disasm trace`.
//!
//! Boots a registered machine (via [`Harness`]), runs it for a frame window,
//! and observes two correlated-by-cycle streams the interactive debugger
//! already produces but that were previously reachable only from the SDL
//! frontend:
//!
//! - `--events <kinds|all>` enables the board's [`DebugTrace`] ring and
//!   collects the recorded hardware events (device writes, bank switches,
//!   interrupt edges, watchdog kicks, …).
//! - `--watch <cpu:addr:kind>` sets memory watchpoints and logs every hit.
//!
//! Both streams carry a machine `cycle`, so the collected records are merged
//! and emitted cycle-sorted in either human `text` or machine-diffable
//! `jsonl` form. `--from-frame N` seeks cheaply (runs fast to N with the
//! observers off, then turns them on) so long runs need not emit from frame 0.
//!
//! This module is the frame loop only. The cycle-granular instruction trace
//! and `--break-pc`/stop conditions (driven by `debug_tick`) are a separate
//! feature and will share the same [`Harness`] and record/output model.

use std::path::Path;

use clap::ValueEnum;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind};
use phosphor_core::core::watchpoint::{DebugAccessSource, WatchpointHit, WatchpointKind};

use crate::harness::Harness;

/// Output serialization for a trace run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum TraceFormat {
    /// One human-readable, columnar line per record.
    Text,
    /// One JSON object per line (greppable / diffable).
    Jsonl,
}

/// A parsed `--watch cpu:addr:kind` request (may set two watchpoints for `rw`).
struct WatchSpec {
    cpu: usize,
    addr: u32,
    read: bool,
    write: bool,
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

/// Boot `machine`, run `frames` frames, and emit the requested trace.
///
/// At least one observer (`--events` or `--watch`) must be requested — the
/// cycle-loop observers (`--cpu`/`--break-pc`) are a separate feature, so a
/// bare `trace` run would have nothing to report.
#[allow(clippy::too_many_arguments)]
pub fn run_trace(
    machine: &str,
    frames: usize,
    from_frame: usize,
    coin_at: Option<usize>,
    nvram: Option<&Path>,
    events: Option<&str>,
    watch: Option<&str>,
    format: TraceFormat,
    out: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    if events.is_none() && watch.is_none() {
        return Err(
            "trace needs at least one observer: --events <kinds|all> and/or --watch <cpu:addr:kind>"
                .to_string(),
        );
    }
    if from_frame > frames {
        return Err(format!(
            "--from-frame {from_frame} is past --frames {frames}"
        ));
    }

    // Parse observer specs up front so bad grammar fails before booting.
    let event_filter = events.map(parse_event_kinds).transpose()?;
    let watch_specs = match watch {
        Some(spec) => parse_watch_specs(spec)?,
        None => Vec::new(),
    };

    let mut harness = Harness::build(machine, path, nvram, coin_at)?;
    let cycles_per_frame = harness.machine_mut().cycles_per_frame();

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
            harness
                .machine_mut()
                .set_watchpoint(w.cpu, w.addr, WatchpointKind::Read);
        }
        if w.write {
            harness
                .machine_mut()
                .set_watchpoint(w.cpu, w.addr, WatchpointKind::Write);
        }
    }

    // Observe the window. Watchpoint hits are drained every frame (the pending
    // queue is shallow); trace events are drained and cleared every frame so a
    // run longer than the ring capacity does not silently lose early events.
    let mut records: Vec<Record> = Vec::new();
    for _ in from_frame..frames {
        harness.run_frame();

        while let Some(hit) = harness.machine_mut().take_watchpoint_hit() {
            records.push(Record::Watch(hit));
        }

        if let Some(filter) = &event_filter {
            let machine = harness.machine_mut();
            for &e in machine.trace_events() {
                if filter.accepts(e.kind) {
                    records.push(Record::Event(e));
                }
            }
            machine.clear_trace_events();
        }
    }

    // Stable merge by cycle; equal cycles keep collection order (events and
    // hits recorded in the same cycle stay in the order the board produced).
    records.sort_by_key(|r| r.cycle());

    let body = render(&records, cycles_per_frame, format);
    match out {
        Some(p) => {
            std::fs::write(p, &body).map_err(|e| format!("writing {}: {e}", p.display()))?;
            Ok(format!(
                "trace: {} record(s) over frames {from_frame}..{frames} -> {}\n",
                records.len(),
                p.display()
            ))
        }
        None => Ok(body),
    }
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

/// Parse one `cpu:addr:kind` spec (kind = `r`, `w`, or `rw`).
fn parse_watch_spec(part: &str) -> Result<WatchSpec, String> {
    let fields: Vec<&str> = part.split(':').collect();
    if fields.len() != 3 {
        return Err(format!(
            "bad watch spec '{part}'; expected cpu:addr:kind (e.g. 0:0x87cf:w)"
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
    Ok(WatchSpec {
        cpu,
        addr,
        read,
        write,
    })
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

        assert!(parse_watch_spec("0:0x10").is_err()); // too few fields
        assert!(parse_watch_spec("x:0x10:r").is_err()); // bad cpu
        assert!(parse_watch_spec("0:zz:r").is_err()); // bad addr
        assert!(parse_watch_spec("0:0x10:x").is_err()); // bad kind
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
                Some("all"),
                None,
                TraceFormat::Text,
                None,
                "."
            )
            .is_err()
        );
    }

    // ---- ROM-gated end-to-end boot test (skips when ROMs are absent) --------

    /// Locate a ROM directory for the gated integration test: `PHOSPHOR_ROMS`
    /// if set, else the conventional `~/ws/mame-runtime/roms`. Returns `None`
    /// (test skips) when neither is present, so CI without ROMs stays green.
    fn roms_dir() -> Option<std::path::PathBuf> {
        if let Ok(dir) = std::env::var("PHOSPHOR_ROMS") {
            let p = std::path::PathBuf::from(dir);
            return p.is_dir().then_some(p);
        }
        let home = std::env::var("HOME").ok()?;
        let p = std::path::PathBuf::from(home).join("ws/mame-runtime/roms");
        p.is_dir().then_some(p)
    }

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
            Some("bank,devwrite"),
            None,
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
            Some("0:0xC900:w"),
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
            Some("all"),
            None,
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
}
