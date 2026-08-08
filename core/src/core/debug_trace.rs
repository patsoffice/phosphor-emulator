//! Debug event tracing: a bounded ring of recent hardware events.
//!
//! The debugger can show current state; this module records how state
//! changed. Boards embed a [`DebugTraceBuffer`] and record integration
//! events (bus writes to devices, bank switches, DMA traffic, interrupt
//! edges) at bus/board boundaries. The [`DebugTrace`] capability exposes
//! the ring to the frontend.
//!
//! Tracing is disabled by default and costs one branch per potential
//! record site when off. Event strings are `&'static str` so recording
//! never allocates; dynamic data belongs in the structured fields
//! (`addr`, `value`, `region`, `device`).
//!
//! Design: `docs/designs/debug-observability.md` (Phase 2). Events are
//! observer state, not emulated hardware state — they are never included
//! in machine save states.

use std::collections::VecDeque;

use crate::core::watchpoint::DebugAccessSource;

/// What kind of hardware event was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DebugEventKind {
    MemoryRead,
    MemoryWrite,
    IoRead,
    IoWrite,
    DeviceRead,
    DeviceWrite,
    InterruptAssert,
    InterruptClear,
    InterruptAck,
    DmaRead,
    DmaWrite,
    BankSwitch,
    Watchdog,
    Scanline,
    CpuHalt,
    CpuResume,
    Message,
}

impl DebugEventKind {
    /// Short display label (for trace panels and logs).
    pub fn label(&self) -> &'static str {
        match self {
            DebugEventKind::MemoryRead => "mem rd",
            DebugEventKind::MemoryWrite => "mem wr",
            DebugEventKind::IoRead => "io rd",
            DebugEventKind::IoWrite => "io wr",
            DebugEventKind::DeviceRead => "dev rd",
            DebugEventKind::DeviceWrite => "dev wr",
            DebugEventKind::InterruptAssert => "irq +",
            DebugEventKind::InterruptClear => "irq -",
            DebugEventKind::InterruptAck => "irq ack",
            DebugEventKind::DmaRead => "dma rd",
            DebugEventKind::DmaWrite => "dma wr",
            DebugEventKind::BankSwitch => "bank",
            DebugEventKind::Watchdog => "wdog",
            DebugEventKind::Scanline => "scanline",
            DebugEventKind::CpuHalt => "halt",
            DebugEventKind::CpuResume => "resume",
            DebugEventKind::Message => "msg",
        }
    }

    /// Every kind, in declaration order. Drives the CLI's token list and the
    /// debugger panel's filter checkboxes, so a new kind appears in both
    /// without either having to enumerate the enum again.
    pub const ALL: [DebugEventKind; 17] = [
        DebugEventKind::MemoryRead,
        DebugEventKind::MemoryWrite,
        DebugEventKind::IoRead,
        DebugEventKind::IoWrite,
        DebugEventKind::DeviceRead,
        DebugEventKind::DeviceWrite,
        DebugEventKind::InterruptAssert,
        DebugEventKind::InterruptClear,
        DebugEventKind::InterruptAck,
        DebugEventKind::DmaRead,
        DebugEventKind::DmaWrite,
        DebugEventKind::BankSwitch,
        DebugEventKind::Watchdog,
        DebugEventKind::Scanline,
        DebugEventKind::CpuHalt,
        DebugEventKind::CpuResume,
        DebugEventKind::Message,
    ];

    /// Space-free token naming this kind where a label with spaces won't do:
    /// a CLI `--events` list, a config file, a filter chip.
    /// [`label`](Self::label) stays the human-readable column text.
    pub fn token(&self) -> &'static str {
        match self {
            DebugEventKind::MemoryRead => "memread",
            DebugEventKind::MemoryWrite => "memwrite",
            DebugEventKind::IoRead => "ioread",
            DebugEventKind::IoWrite => "iowrite",
            DebugEventKind::DeviceRead => "devread",
            DebugEventKind::DeviceWrite => "devwrite",
            DebugEventKind::InterruptAssert => "irqassert",
            DebugEventKind::InterruptClear => "irqclear",
            DebugEventKind::InterruptAck => "irqack",
            DebugEventKind::DmaRead => "dmaread",
            DebugEventKind::DmaWrite => "dmawrite",
            DebugEventKind::BankSwitch => "bank",
            DebugEventKind::Watchdog => "watchdog",
            DebugEventKind::Scanline => "scanline",
            DebugEventKind::CpuHalt => "halt",
            DebugEventKind::CpuResume => "resume",
            DebugEventKind::Message => "message",
        }
    }

    /// Resolve a [`token`](Self::token), case-insensitively.
    pub fn from_token(token: &str) -> Option<Self> {
        let token = token.trim();
        Self::ALL
            .into_iter()
            .find(|k| k.token().eq_ignore_ascii_case(token))
    }

    /// Every token, comma-separated — for `--events` help and parse errors.
    pub fn token_list() -> String {
        Self::ALL
            .iter()
            .map(|k| k.token())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// One recorded hardware event.
///
/// Only `cycle`, `source`, and `kind` are always meaningful; the rest are
/// populated when the recording site knows them. `detail` is a static
/// string by design — model dynamic values structurally instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugEvent {
    /// Machine cycle when the event was recorded.
    pub cycle: u64,
    /// Who caused the event.
    pub source: DebugAccessSource,
    /// CPU whose address space the event belongs to, when applicable.
    pub cpu_index: Option<usize>,
    /// Address of the instruction executing at the event, when known.
    pub pc: Option<u32>,
    /// What happened.
    pub kind: DebugEventKind,
    /// Address involved, when applicable.
    pub addr: Option<u32>,
    /// Value read/written, when applicable (low `width * 8` bits).
    pub value: Option<u32>,
    /// Access width in bytes (0 when no value is involved).
    pub width: u8,
    /// Name of the mapped region containing `addr`, when known.
    pub region: Option<&'static str>,
    /// Name of the device involved, when known.
    pub device: Option<&'static str>,
    /// Static annotation (e.g. "sound command").
    pub detail: Option<&'static str>,
}

impl DebugEvent {
    /// An event with the always-meaningful fields set and everything else
    /// empty. Use struct-update syntax to fill in what the site knows:
    ///
    /// ```
    /// # use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind};
    /// # use phosphor_core::core::watchpoint::DebugAccessSource;
    /// let event = DebugEvent {
    ///     addr: Some(0xC900),
    ///     value: Some(0x01),
    ///     width: 1,
    ///     ..DebugEvent::new(1234, DebugAccessSource::Cpu(0), DebugEventKind::BankSwitch)
    /// };
    /// ```
    pub fn new(cycle: u64, source: DebugAccessSource, kind: DebugEventKind) -> Self {
        Self {
            cycle,
            source,
            cpu_index: None,
            pc: None,
            kind,
            addr: None,
            value: None,
            width: 0,
            region: None,
            device: None,
            detail: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

/// Which access sources an [`EventFilter`] keeps.
///
/// Coarser than [`DebugAccessSource`]: `Device` matches any device by name and
/// `Cpu(i)` matches one CPU, because "which CPU wrote this" is the question a
/// trace viewer actually asks. Board-level events with no CPU attribution
/// (`Unknown`) pass only under [`Any`](Self::Any).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceFilter {
    #[default]
    Any,
    Cpu(usize),
    Dma,
    Device,
    Frontend,
}

impl SourceFilter {
    pub fn accepts(self, source: DebugAccessSource) -> bool {
        match (self, source) {
            (SourceFilter::Any, _) => true,
            (SourceFilter::Cpu(want), DebugAccessSource::Cpu(got)) => want == got,
            (SourceFilter::Dma, DebugAccessSource::Dma) => true,
            (SourceFilter::Device, DebugAccessSource::Device(_)) => true,
            (SourceFilter::Frontend, DebugAccessSource::Frontend) => true,
            _ => false,
        }
    }

    /// Menu/label text ("any", "CPU0", "DMA", …).
    pub fn label(self) -> String {
        match self {
            SourceFilter::Any => "any".to_string(),
            SourceFilter::Cpu(i) => format!("CPU{i}"),
            SourceFilter::Dma => "DMA".to_string(),
            SourceFilter::Device => "device".to_string(),
            SourceFilter::Frontend => "frontend".to_string(),
        }
    }
}

/// Which recorded events a viewer keeps: by kind, by source, by address range.
///
/// One model shared by the two consumers of the event ring — `disasm trace
/// --events` and the debugger's Event Trace panel — so a kind token means the
/// same thing on the command line as in the panel, and neither grows its own
/// parallel predicate. The CLI only sets `kinds` today; `source` and `addr`
/// default to "accept everything", so adding flags for them later needs no
/// change here.
///
/// Filtering is a *display* concern: the ring records whatever the board
/// reports, and a filter only decides what is shown. Widening one therefore
/// reveals events already captured rather than requiring a re-run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventFilter {
    /// Kinds to keep; `None` means every kind. `Some(empty)` keeps none.
    kinds: Option<Vec<DebugEventKind>>,
    /// Which source to keep.
    pub source: SourceFilter,
    /// Inclusive address range to keep. An event with no address is kept only
    /// when this is `None` — a range is a question about addressed accesses.
    pub addr: Option<(u32, u32)>,
}

impl EventFilter {
    /// A filter that accepts every event.
    pub fn all() -> Self {
        Self::default()
    }

    /// Accept only these kinds (any source, any address).
    pub fn with_kinds(kinds: impl IntoIterator<Item = DebugEventKind>) -> Self {
        Self {
            kinds: Some(kinds.into_iter().collect()),
            ..Self::default()
        }
    }

    /// Parse a `--events`-style value: `all`, or a comma-separated list of
    /// [`DebugEventKind::token`]s. Empty entries are skipped; an entirely empty
    /// list is an error rather than a silent "match nothing".
    pub fn parse_kinds(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("all") {
            return Ok(Self::all());
        }
        let mut kinds = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let kind = DebugEventKind::from_token(token).ok_or_else(|| {
                format!(
                    "unknown event kind '{token}'; valid: {}, or 'all'",
                    DebugEventKind::token_list()
                )
            })?;
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        if kinds.is_empty() {
            return Err("no event kinds given; pass kind tokens or 'all'".to_string());
        }
        Ok(Self::with_kinds(kinds))
    }

    /// True if every predicate is wide open (nothing is being hidden).
    pub fn is_unfiltered(&self) -> bool {
        self.kinds.is_none() && self.source == SourceFilter::Any && self.addr.is_none()
    }

    /// The selected kinds, or `None` when every kind is accepted.
    pub fn kinds(&self) -> Option<&[DebugEventKind]> {
        self.kinds.as_deref()
    }

    /// True if `kind` passes the kind predicate alone.
    pub fn accepts_kind(&self, kind: DebugEventKind) -> bool {
        match &self.kinds {
            None => true,
            Some(kinds) => kinds.contains(&kind),
        }
    }

    /// Select or deselect one kind. Deselecting from "all kinds" first
    /// materializes the full list, so a single unchecked box does not silently
    /// become "only that kind".
    pub fn set_kind(&mut self, kind: DebugEventKind, on: bool) {
        let kinds = self
            .kinds
            .get_or_insert_with(|| DebugEventKind::ALL.to_vec());
        match (on, kinds.iter().position(|k| *k == kind)) {
            (true, None) => kinds.push(kind),
            (false, Some(i)) => {
                kinds.remove(i);
            }
            _ => {}
        }
        // Back to every kind selected: drop to `None` so `is_unfiltered` is true
        // and the panel reports the filter as off.
        if kinds.len() == DebugEventKind::ALL.len() {
            self.kinds = None;
        }
    }

    /// Accept every kind.
    pub fn select_all_kinds(&mut self) {
        self.kinds = None;
    }

    /// Accept no kind (the filter then hides everything).
    pub fn select_no_kinds(&mut self) {
        self.kinds = Some(Vec::new());
    }

    /// True if `event` passes every predicate.
    pub fn accepts(&self, event: &DebugEvent) -> bool {
        if !self.accepts_kind(event.kind) {
            return false;
        }
        if !self.source.accepts(event.source) {
            return false;
        }
        if let Some((lo, hi)) = self.addr {
            match event.addr {
                Some(addr) => return (lo..=hi).contains(&addr),
                None => return false,
            }
        }
        true
    }
}

/// Parse an address-range filter: `$1234`, `1234`, `0x1234` (a single address),
/// or `$1000-$1FFF` (inclusive range). **Hex always**, with or without a `$`/
/// `0x` prefix — this reads debugger-panel fields, where every address is hex,
/// unlike the CLI's decimal-by-default `parse_u32_auto`.
///
/// A reversed range (`$1FFF-$1000`) is normalized rather than rejected.
pub fn parse_addr_range(spec: &str) -> Result<(u32, u32), String> {
    fn hex(s: &str) -> Result<u32, String> {
        let t = s.trim();
        let t = t.strip_prefix('$').unwrap_or(t);
        let t = t
            .strip_prefix("0x")
            .or_else(|| t.strip_prefix("0X"))
            .unwrap_or(t);
        if t.is_empty() {
            return Err("empty address".to_string());
        }
        u32::from_str_radix(t, 16).map_err(|_| format!("invalid hex address '{s}'"))
    }

    let spec = spec.trim();
    // Split on the *last* `-`: a leading `$` never contains one, so this only
    // ever separates two operands.
    match spec.rsplit_once('-') {
        Some((lo, hi)) => {
            let (lo, hi) = (hex(lo)?, hex(hi)?);
            Ok((lo.min(hi), lo.max(hi)))
        }
        None => {
            let addr = hex(spec)?;
            Ok((addr, addr))
        }
    }
}

/// Default ring capacity (events retained before the oldest are dropped).
pub const DEFAULT_TRACE_CAPACITY: usize = 4096;

/// A bounded FIFO ring of [`DebugEvent`]s, embedded per board.
///
/// Disabled by default; when disabled, [`record`](Self::record) is a no-op
/// and hot paths should gate on [`enabled`](Self::enabled) so event
/// construction itself is skipped:
///
/// ```ignore
/// if self.debug_trace.enabled() {
///     self.debug_trace.record(DebugEvent { ... });
/// }
/// ```
#[derive(Debug)]
pub struct DebugTraceBuffer {
    events: VecDeque<DebugEvent>,
    capacity: usize,
    enabled: bool,
}

impl DebugTraceBuffer {
    /// Create a disabled buffer with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TRACE_CAPACITY)
    }

    /// Create a disabled buffer holding at most `capacity` events.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            capacity: capacity.max(1),
            enabled: false,
        }
    }

    /// True if recording is enabled.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable recording. Disabling keeps already-recorded
    /// events (so a capture can be inspected after stopping the trace).
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Record an event, dropping the oldest when the ring is full.
    /// No-op while disabled.
    #[inline]
    pub fn record(&mut self, event: DebugEvent) {
        if !self.enabled {
            return;
        }
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// All recorded events, oldest first.
    ///
    /// Takes `&mut self` because the ring is made contiguous in place to
    /// return a single slice.
    pub fn events(&mut self) -> &[DebugEvent] {
        self.events.make_contiguous();
        self.events.as_slices().0
    }

    /// Drop all recorded events (recording state is unchanged).
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Number of events currently held.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// True if no events are held.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Maximum number of events retained.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Default for DebugTraceBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Event-tracing capability, part of the frontend machine bundle.
///
/// Machines without tracing use the no-op defaults. Boards that embed a
/// [`DebugTraceBuffer`] implement this via `#[derive(DebugTrace)]` with a
/// `#[debug_events]` field attribute (or by hand), and game wrappers
/// delegate to the board.
pub trait DebugTrace {
    /// Enable or disable event recording.
    fn set_trace_enabled(&mut self, _enabled: bool) {}

    /// True if event recording is enabled.
    fn trace_enabled(&self) -> bool {
        false
    }

    /// All recorded events, oldest first. `&mut self` because ring
    /// buffers are made contiguous in place.
    fn trace_events(&mut self) -> &[DebugEvent] {
        &[]
    }

    /// Drop all recorded events.
    fn clear_trace_events(&mut self) {}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn event(cycle: u64) -> DebugEvent {
        DebugEvent::new(cycle, DebugAccessSource::Cpu(0), DebugEventKind::Message)
    }

    // ---- Kind tokens ------------------------------------------------------

    #[test]
    fn every_kind_has_a_unique_space_free_token() {
        let mut tokens: Vec<&str> = DebugEventKind::ALL.iter().map(|k| k.token()).collect();
        assert_eq!(tokens.len(), DebugEventKind::ALL.len());
        for token in &tokens {
            assert!(!token.contains(' '), "token '{token}' has a space");
            assert!(!token.is_empty());
        }
        tokens.sort_unstable();
        let before = tokens.len();
        tokens.dedup();
        assert_eq!(before, tokens.len(), "duplicate event-kind token");
    }

    #[test]
    fn tokens_round_trip_case_insensitively() {
        for kind in DebugEventKind::ALL {
            assert_eq!(DebugEventKind::from_token(kind.token()), Some(kind));
            assert_eq!(
                DebugEventKind::from_token(&kind.token().to_uppercase()),
                Some(kind)
            );
            // Surrounding whitespace is a list-splitting artifact, not an error.
            assert_eq!(
                DebugEventKind::from_token(&format!(" {} ", kind.token())),
                Some(kind)
            );
        }
        assert_eq!(DebugEventKind::from_token("nope"), None);
        // `label()` has spaces and is NOT a token.
        assert_eq!(DebugEventKind::from_token("mem wr"), None);
    }

    // ---- Filter -----------------------------------------------------------

    fn ev(kind: DebugEventKind, source: DebugAccessSource, addr: Option<u32>) -> DebugEvent {
        DebugEvent {
            addr,
            ..DebugEvent::new(0, source, kind)
        }
    }

    #[test]
    fn default_filter_accepts_everything() {
        let f = EventFilter::all();
        assert!(f.is_unfiltered());
        assert!(f.accepts(&ev(
            DebugEventKind::MemoryRead,
            DebugAccessSource::Unknown,
            None
        )));
        assert!(f.accepts(&ev(
            DebugEventKind::BankSwitch,
            DebugAccessSource::Cpu(3),
            Some(0xC900)
        )));
    }

    #[test]
    fn kind_filter_keeps_only_listed_kinds() {
        let f = EventFilter::with_kinds([DebugEventKind::BankSwitch, DebugEventKind::Watchdog]);
        assert!(!f.is_unfiltered());
        assert!(f.accepts_kind(DebugEventKind::BankSwitch));
        assert!(!f.accepts_kind(DebugEventKind::MemoryRead));
        assert!(f.accepts(&ev(
            DebugEventKind::Watchdog,
            DebugAccessSource::Cpu(0),
            None
        )));
        assert!(!f.accepts(&ev(
            DebugEventKind::MemoryWrite,
            DebugAccessSource::Cpu(0),
            None
        )));
    }

    #[test]
    fn parse_kinds_matches_the_cli_grammar() {
        assert!(EventFilter::parse_kinds("all").unwrap().is_unfiltered());
        assert!(EventFilter::parse_kinds("ALL").unwrap().is_unfiltered());

        let f = EventFilter::parse_kinds("devwrite, bank ,watchdog").unwrap();
        assert!(f.accepts_kind(DebugEventKind::DeviceWrite));
        assert!(f.accepts_kind(DebugEventKind::BankSwitch));
        assert!(f.accepts_kind(DebugEventKind::Watchdog));
        assert!(!f.accepts_kind(DebugEventKind::MemoryRead));

        // A repeated token is idempotent, not a duplicate entry.
        assert_eq!(
            EventFilter::parse_kinds("bank,bank").unwrap().kinds(),
            Some(&[DebugEventKind::BankSwitch][..])
        );

        assert!(EventFilter::parse_kinds("nope").is_err());
        assert!(EventFilter::parse_kinds("").is_err());
        assert!(EventFilter::parse_kinds(" , ").is_err());
    }

    #[test]
    fn unchecking_one_kind_from_all_keeps_the_rest() {
        // The panel's checkbox path: "all kinds" must materialize before one is
        // removed, or unchecking a box would collapse to selecting only it.
        let mut f = EventFilter::all();
        f.set_kind(DebugEventKind::MemoryRead, false);
        assert!(!f.accepts_kind(DebugEventKind::MemoryRead));
        assert!(f.accepts_kind(DebugEventKind::MemoryWrite));
        assert!(f.accepts_kind(DebugEventKind::BankSwitch));
        assert_eq!(f.kinds().unwrap().len(), DebugEventKind::ALL.len() - 1);

        // Re-checking the last missing kind returns to "unfiltered", so the
        // panel stops claiming a filter is active.
        f.set_kind(DebugEventKind::MemoryRead, true);
        assert!(f.is_unfiltered());

        // Setting an already-set kind is a no-op (no duplicate entries).
        let mut f = EventFilter::with_kinds([DebugEventKind::BankSwitch]);
        f.set_kind(DebugEventKind::BankSwitch, true);
        assert_eq!(f.kinds().unwrap(), &[DebugEventKind::BankSwitch]);
        f.set_kind(DebugEventKind::Watchdog, false);
        assert_eq!(f.kinds().unwrap(), &[DebugEventKind::BankSwitch]);
    }

    #[test]
    fn select_none_hides_everything() {
        let mut f = EventFilter::all();
        f.select_no_kinds();
        assert!(!f.is_unfiltered());
        for kind in DebugEventKind::ALL {
            assert!(!f.accepts_kind(kind));
        }
        f.select_all_kinds();
        assert!(f.is_unfiltered());
    }

    #[test]
    fn source_filter_narrows_to_one_cpu_or_class() {
        let mut f = EventFilter::all();
        f.source = SourceFilter::Cpu(1);
        assert!(f.accepts(&ev(
            DebugEventKind::MemoryWrite,
            DebugAccessSource::Cpu(1),
            None
        )));
        assert!(!f.accepts(&ev(
            DebugEventKind::MemoryWrite,
            DebugAccessSource::Cpu(0),
            None
        )));
        // A board-level event with no CPU attribution is not "CPU1".
        assert!(!f.accepts(&ev(
            DebugEventKind::Watchdog,
            DebugAccessSource::Unknown,
            None
        )));

        f.source = SourceFilter::Device;
        assert!(f.accepts(&ev(
            DebugEventKind::DeviceWrite,
            DebugAccessSource::Device("psg"),
            None
        )));
        assert!(!f.accepts(&ev(
            DebugEventKind::DeviceWrite,
            DebugAccessSource::Cpu(0),
            None
        )));

        f.source = SourceFilter::Frontend;
        assert!(f.accepts(&ev(
            DebugEventKind::MemoryWrite,
            DebugAccessSource::Frontend,
            None
        )));
    }

    #[test]
    fn addr_filter_is_an_inclusive_range_and_drops_addressless_events() {
        let mut f = EventFilter::all();
        f.addr = Some((0x1000, 0x1FFF));
        let at = |a| {
            ev(
                DebugEventKind::MemoryWrite,
                DebugAccessSource::Cpu(0),
                Some(a),
            )
        };
        assert!(f.accepts(&at(0x1000))); // inclusive low
        assert!(f.accepts(&at(0x1FFF))); // inclusive high
        assert!(!f.accepts(&at(0x0FFF)));
        assert!(!f.accepts(&at(0x2000)));
        // A range asks about addressed accesses, so an event with no address
        // (a watchdog kick, a CPU halt) is excluded rather than always kept.
        assert!(!f.accepts(&ev(
            DebugEventKind::Watchdog,
            DebugAccessSource::Cpu(0),
            None
        )));
    }

    #[test]
    fn predicates_compose() {
        let mut f = EventFilter::with_kinds([DebugEventKind::MemoryWrite]);
        f.source = SourceFilter::Cpu(0);
        f.addr = Some((0x9000, 0x9FFF));
        let hit = ev(
            DebugEventKind::MemoryWrite,
            DebugAccessSource::Cpu(0),
            Some(0x9100),
        );
        assert!(f.accepts(&hit));
        // Each predicate alone is enough to reject.
        assert!(!f.accepts(&DebugEvent {
            kind: DebugEventKind::MemoryRead,
            ..hit
        }));
        assert!(!f.accepts(&DebugEvent {
            source: DebugAccessSource::Cpu(1),
            ..hit
        }));
        assert!(!f.accepts(&DebugEvent {
            addr: Some(0x8FFF),
            ..hit
        }));
    }

    #[test]
    fn addr_range_parses_hex_with_optional_prefixes() {
        assert_eq!(parse_addr_range("9100").unwrap(), (0x9100, 0x9100));
        assert_eq!(parse_addr_range("$9100").unwrap(), (0x9100, 0x9100));
        assert_eq!(parse_addr_range("0x9100").unwrap(), (0x9100, 0x9100));
        assert_eq!(parse_addr_range(" $1000-$1fff ").unwrap(), (0x1000, 0x1FFF));
        assert_eq!(parse_addr_range("1000-1FFF").unwrap(), (0x1000, 0x1FFF));
        // Bare digits are HEX here (panel convention), not decimal.
        assert_eq!(parse_addr_range("20").unwrap(), (0x20, 0x20));
        // A reversed range is the same range, not an error.
        assert_eq!(parse_addr_range("$1fff-$1000").unwrap(), (0x1000, 0x1FFF));

        assert!(parse_addr_range("").is_err());
        assert!(parse_addr_range("zz").is_err());
        assert!(parse_addr_range("$1000-").is_err());
        assert!(parse_addr_range("-$1000").is_err());
    }

    #[test]
    fn disabled_buffer_records_nothing() {
        let mut buf = DebugTraceBuffer::new();
        assert!(!buf.enabled());
        buf.record(event(1));
        assert!(buf.is_empty());
        assert!(buf.events().is_empty());
    }

    #[test]
    fn enabled_buffer_preserves_order() {
        let mut buf = DebugTraceBuffer::new();
        buf.set_enabled(true);
        for cycle in 0..10 {
            buf.record(event(cycle));
        }
        assert_eq!(buf.len(), 10);
        let cycles: Vec<u64> = buf.events().iter().map(|e| e.cycle).collect();
        assert_eq!(cycles, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn ring_drops_oldest_at_capacity() {
        let mut buf = DebugTraceBuffer::with_capacity(4);
        buf.set_enabled(true);
        for cycle in 0..10 {
            buf.record(event(cycle));
        }
        assert_eq!(buf.len(), 4);
        // Oldest events (0..6) were dropped; the ring holds 6..10 in order.
        let cycles: Vec<u64> = buf.events().iter().map(|e| e.cycle).collect();
        assert_eq!(cycles, vec![6, 7, 8, 9]);
    }

    #[test]
    fn events_returns_single_contiguous_slice_after_wrap() {
        let mut buf = DebugTraceBuffer::with_capacity(3);
        buf.set_enabled(true);
        for cycle in 0..7 {
            buf.record(event(cycle));
        }
        // After wrapping, the slice must still contain everything in order.
        assert_eq!(buf.events().len(), 3);
        assert_eq!(buf.events()[0].cycle, 4);
        assert_eq!(buf.events()[2].cycle, 6);
    }

    #[test]
    fn disabling_keeps_captured_events() {
        let mut buf = DebugTraceBuffer::new();
        buf.set_enabled(true);
        buf.record(event(1));
        buf.set_enabled(false);
        buf.record(event(2)); // dropped: disabled
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.events()[0].cycle, 1);
    }

    #[test]
    fn clear_drops_events_but_keeps_enabled_state() {
        let mut buf = DebugTraceBuffer::new();
        buf.set_enabled(true);
        buf.record(event(1));
        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.enabled());
    }

    #[test]
    fn new_constructor_fills_optional_fields_empty() {
        let e = DebugEvent::new(42, DebugAccessSource::Dma, DebugEventKind::DmaWrite);
        assert_eq!(e.cycle, 42);
        assert_eq!(e.source, DebugAccessSource::Dma);
        assert_eq!(e.kind, DebugEventKind::DmaWrite);
        assert_eq!(e.cpu_index, None);
        assert_eq!(e.pc, None);
        assert_eq!(e.addr, None);
        assert_eq!(e.value, None);
        assert_eq!(e.width, 0);
        assert_eq!(e.region, None);
        assert_eq!(e.device, None);
        assert_eq!(e.detail, None);
    }

    #[test]
    fn default_trait_impl_is_noop() {
        struct NoTrace;
        impl DebugTrace for NoTrace {}

        let mut m = NoTrace;
        assert!(!m.trace_enabled());
        m.set_trace_enabled(true);
        assert!(!m.trace_enabled());
        assert!(m.trace_events().is_empty());
        m.clear_trace_events();
    }
}
