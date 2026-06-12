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
#[derive(Clone, Copy, Debug, PartialEq)]
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
