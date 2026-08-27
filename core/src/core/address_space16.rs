//! Composed address-space container for 16-bit machines.
//!
//! [`AddressSpace16`] owns address decode, memory backing, and watchpoint
//! state for a 64 KB address space. It provides three capabilities that
//! hand-rolled match arms cannot:
//!
//! 1. **Watchpoints** — per-page read/write watch flags with zero cost when
//!    no watchpoints are active (a single `active_watch_count == 0` check).
//! 2. **Introspection** — named region descriptors that a debugger or memory
//!    viewer can enumerate without parsing source code.
//! 3. **Declarative mirroring** — `mirror()` copies page entries at build time
//!    instead of hand-coding `addr & mask` per machine.
//!
//! Decode divides the 64 KB address space into 256 pages of 256 bytes each.
//! Each page entry carries a machine-defined `region_id` (a plain `u8`) that
//! the machine's `Bus::read`/`write` dispatches on with a small match.
//!
//! The split into decode / backing / watchpoint services and the 32-bit
//! sibling (`AddressSpace32`) are described in
//! `docs/designs/address-space-refactor.md`.

use crate::core::address_space::{AccessKind, DebugRead, MemoryBacking, RegionId, UNMAPPED};
use crate::core::bus::BusMaster;
use crate::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::core::watchpoint::{
    DebugAccessSource, WatchpointCondition, WatchpointHit, WatchpointKind, WatchpointPhase,
    Watchpoints,
};

/// How one bus write should appear in the event trace.
///
/// Regions are page-granular — a region starts on a 256-byte boundary and is a
/// whole number of pages long — so the region map alone cannot label hardware
/// that decodes finer than that: an 8-byte latch, a single watchdog address, a
/// 32-byte sound-chip window. This is what a board passes to
/// [`watch_write_annotated`](AddressSpace16::watch_write_annotated) to name
/// such a write without inventing a fake region for it.
///
/// [`MEMORY`](Self::MEMORY) is the plain-RAM default; [`device`](Self::device)
/// and [`detail`](Self::detail) build the common device-write shapes:
///
/// ```
/// # use phosphor_core::core::address_space16::WriteAnnotation;
/// # use phosphor_core::core::debug_trace::DebugEventKind;
/// let wsg = WriteAnnotation::device("Namco WSG");
/// let latch = WriteAnnotation::detail("Misc latch", "main IRQ enable");
/// let wdog = WriteAnnotation {
///     kind: DebugEventKind::Watchdog,
///     detail: Some("watchdog cleared"),
///     ..WriteAnnotation::MEMORY
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WriteAnnotation {
    /// Event kind to record. Defaults to [`DebugEventKind::MemoryWrite`].
    pub kind: DebugEventKind,
    /// Device that owns the address, when the board can attribute it.
    pub device: Option<&'static str>,
    /// Short static annotation, e.g. which latch bit the address selects.
    pub detail: Option<&'static str>,
    /// Check watchpoints but record no trace event. For addresses whose
    /// events a dedicated device path already records, so the write is not
    /// counted twice.
    pub suppress_trace: bool,
}

impl WriteAnnotation {
    /// A plain, unattributed memory write — what an untagged RAM store records.
    pub const MEMORY: Self = Self {
        kind: DebugEventKind::MemoryWrite,
        device: None,
        detail: None,
        suppress_trace: false,
    };

    /// A device write attributed to `device`.
    pub const fn device(device: &'static str) -> Self {
        Self {
            kind: DebugEventKind::DeviceWrite,
            device: Some(device),
            ..Self::MEMORY
        }
    }

    /// A device write attributed to `device` and annotated with `detail`.
    pub const fn detail(device: &'static str, detail: &'static str) -> Self {
        Self {
            detail: Some(detail),
            ..Self::device(device)
        }
    }

    /// Watchpoints still fire, but nothing is written to the trace.
    pub const SUPPRESSED: Self = Self {
        suppress_trace: true,
        ..Self::MEMORY
    };
}

impl Default for WriteAnnotation {
    fn default() -> Self {
        Self::MEMORY
    }
}

/// A single entry in the 256-page table.
#[derive(Clone, Copy, Debug)]
pub struct PageEntry {
    /// Machine-defined region identifier. The machine's `Bus::read`/`write`
    /// matches on this to reach the right backing store.
    pub region_id: RegionId,

    /// Byte offset into the region for the start of this page.
    /// For a region starting at address 0x4000 mapped to pages 0x40..0x43,
    /// page 0x40 has `base_offset = 0`, page 0x41 has `base_offset = 0x100`, etc.
    ///
    /// `u32` (not `u16`) so banked regions and shared backing stores are not
    /// limited to 64 KB of region-local offsets.
    pub base_offset: u32,

    /// True if read watchpoint is active on this page.
    pub watch_read: bool,

    /// True if write watchpoint is active on this page.
    pub watch_write: bool,
}

impl Default for PageEntry {
    fn default() -> Self {
        Self {
            region_id: UNMAPPED,
            base_offset: 0,
            watch_read: false,
            watch_write: false,
        }
    }
}

/// A named region descriptor for debugger introspection.
#[derive(Clone, Debug)]
pub struct RegionDescriptor {
    /// Machine-defined region ID (matches `PageEntry::region_id`).
    pub id: RegionId,
    /// Human-readable name (e.g., "Video RAM", "Widget PIA").
    pub name: &'static str,
    /// First address in this region.
    pub start_addr: u16,
    /// Last address in this region (inclusive).
    pub end_addr: u16,
    /// Access characteristics.
    pub access: AccessKind,
}

/// Page-table decode metadata for a 16-bit (64 KB) address space.
///
/// Owns only the 256-entry page table and the region descriptors — it maps
/// addresses to region IDs and answers region lookup questions. It holds no
/// bytes and no watchpoint hits; [`AddressSpace16`] composes it with those
/// services.
pub struct AddressMap16 {
    pages: [PageEntry; 256],
    regions: Vec<RegionDescriptor>,
}

impl AddressMap16 {
    /// Create a new map with all pages unmapped.
    pub fn new() -> Self {
        Self {
            pages: [PageEntry::default(); 256],
            regions: Vec::new(),
        }
    }

    /// Map a contiguous address range to a region.
    ///
    /// `start` must be page-aligned (low 8 bits == 0) and `length` must be
    /// a multiple of 256. Sets page entries and adds a region descriptor.
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u16,
        length: u32,
        access: AccessKind,
    ) -> &mut Self {
        let id = id.into();
        debug_assert_eq!(
            start & 0xFF,
            0,
            "region start {start:#06X} must be page-aligned"
        );
        debug_assert!(
            length >= 256 || length == 0,
            "region length must be >= 256 (one full page)"
        );

        let start_page = (start >> 8) as usize;
        let page_count = length.div_ceil(256) as usize;

        for i in 0..page_count {
            let idx = start_page + i;
            if idx < 256 {
                self.pages[idx] = PageEntry {
                    region_id: id,
                    base_offset: (i as u32) * 256,
                    watch_read: false,
                    watch_write: false,
                };
            }
        }

        let end_addr = if length == 0 {
            start
        } else {
            start.wrapping_add((length - 1) as u16)
        };

        self.regions.push(RegionDescriptor {
            id,
            name,
            start_addr: start,
            end_addr,
            access,
        });

        self
    }

    /// Copy page entries from a source range to a mirror range.
    ///
    /// All three parameters must be page-aligned and `length` must be a
    /// multiple of 256. Watch flags are not copied (mirrors start clean).
    pub fn mirror(&mut self, mirror_start: u16, source_start: u16, length: u32) -> &mut Self {
        let mirror_page = (mirror_start >> 8) as usize;
        let source_page = (source_start >> 8) as usize;
        let page_count = (length / 256) as usize;

        for i in 0..page_count {
            let src = source_page + i;
            let dst = mirror_page + i;
            if src < 256 && dst < 256 {
                self.pages[dst] = PageEntry {
                    region_id: self.pages[src].region_id,
                    base_offset: self.pages[src].base_offset,
                    watch_read: false,
                    watch_write: false,
                };
            }
        }
        self
    }

    /// Remap a range of pages to a different region (for bank switching).
    ///
    /// Called at runtime when a bank register is written.
    pub fn remap_pages(
        &mut self,
        start_page: u8,
        page_count: u8,
        new_region_id: impl Into<RegionId>,
        new_base_offset: u32,
    ) {
        let new_region_id = new_region_id.into();
        for i in 0..page_count as usize {
            let idx = start_page as usize + i;
            if idx < 256 {
                self.pages[idx].region_id = new_region_id;
                self.pages[idx].base_offset = new_base_offset + (i as u32) * 256;
            }
        }
    }

    /// Look up the page entry for an address.
    #[inline(always)]
    pub fn page(&self, addr: u16) -> &PageEntry {
        // SAFETY: (addr >> 8) is always in 0..256, matching the array bounds.
        unsafe { self.pages.get_unchecked((addr >> 8) as usize) }
    }

    /// Compute the byte offset into the region for an address.
    ///
    /// This is `page.base_offset + (addr & 0xFF)` — the region-local index.
    #[inline(always)]
    pub fn region_offset(&self, addr: u16) -> usize {
        let page = self.page(addr);
        page.base_offset as usize + (addr & 0xFF) as usize
    }

    /// Get all named region descriptors.
    pub fn regions(&self) -> &[RegionDescriptor] {
        &self.regions
    }

    /// Get the region descriptor for the page containing `addr`, if mapped.
    pub fn region_at(&self, addr: u16) -> Option<&RegionDescriptor> {
        let id = self.page(addr).region_id;
        if id == UNMAPPED {
            return None;
        }
        self.regions.iter().find(|r| r.id == id)
    }
}

impl Default for AddressMap16 {
    fn default() -> Self {
        Self::new()
    }
}

/// Composed address-space container for a 16-bit (64 KB) address space.
///
/// Composes page-table decode ([`AddressMap16`]), backing storage
/// ([`MemoryBacking`]), and watchpoint state. Machines build this at init
/// time and use it in `Bus::read`/`write` to look up the `region_id` for
/// dispatch.
///
/// Non-I/O regions (RAM, ROM) have backing memory stored in `MemoryBacking`.
/// This enables side-effect-free `debug_read`/`debug_write` for the debugger
/// without requiring machines to write manual `memory_read` methods.
///
/// The debugger uses it for watchpoints (per-page flags checked only on
/// flagged pages) and region introspection (list of named regions).
pub struct AddressSpace16 {
    map: AddressMap16,
    backing: MemoryBacking,

    /// Number of pages with at least one watch flag set; the hot-path
    /// zero-cost check.
    active_watch_count: u16,
    /// Exact-address watchpoint set and hit queue. Page flags serve as a
    /// fast filter; this provides address-level precision so only the
    /// exact address fires.
    watchpoints: Watchpoints,

    /// Machine cycle latched by the board before CPU execution, used for
    /// hit attribution. Bus dispatch cannot read board/CPU state while a
    /// CPU is mid-tick (borrow splitting), so boards latch it up front via
    /// [`latch_access_context`](Self::latch_access_context).
    debug_cycle: u64,
    /// Address of the instruction currently executing on the owning CPU
    /// (latched at instruction boundaries), when known.
    debug_pc: Option<u32>,

    /// Optional write-event ring. When enabled, every bus write through this
    /// space is recorded as a region-tagged [`DebugEventKind::MemoryWrite`],
    /// giving the headless `trace` tool CPU-agnostic, mirror-resolved capture
    /// without the board hand-rolling a `trace_bus_write` path.
    trace: DebugTraceBuffer,
}

impl AddressSpace16 {
    /// Create a new address space with all pages unmapped.
    pub fn new() -> Self {
        Self {
            map: AddressMap16::new(),
            backing: MemoryBacking::new(),
            active_watch_count: 0,
            watchpoints: Watchpoints::new(),
            debug_cycle: 0,
            debug_pc: None,
            // Large ring: the trace tool drains per frame, but a busy frame can
            // write many thousands of times; a small ring would drop the early
            // (often vblank-handler) video-register writes we care about.
            trace: DebugTraceBuffer::with_capacity(1 << 16),
        }
    }

    // -----------------------------------------------------------------------
    // Builder methods (called at machine init time)
    // -----------------------------------------------------------------------

    /// Map a contiguous address range to a region.
    ///
    /// `start` must be page-aligned (low 8 bits == 0) and `length` must be
    /// a multiple of 256. Sets page entries, adds a region descriptor, and
    /// allocates backing memory for non-I/O regions.
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u16,
        length: u32,
        access: AccessKind,
    ) -> &mut Self {
        let id = id.into();
        self.map.region(id, name, start, length, access);

        // Allocate backing memory for non-I/O regions
        if matches!(
            access,
            AccessKind::ReadWrite | AccessKind::ReadOnly | AccessKind::WriteOnly
        ) {
            self.backing.allocate(id, length);
        }

        self
    }

    /// Register a region with backing memory but no page mapping.
    ///
    /// Used for bank-switched overlays (e.g., banked ROM) that share an
    /// address range with another region. Use `remap_pages()` to switch
    /// pages to this region at runtime.
    pub fn backing_region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        length: u32,
    ) -> &mut Self {
        let id = id.into();
        self.backing.allocate(id, length);

        self.map.regions.push(RegionDescriptor {
            id,
            name,
            start_addr: 0,
            end_addr: 0,
            access: AccessKind::ReadOnly,
        });

        self
    }

    /// Copy page entries from a source range to a mirror range.
    ///
    /// All three parameters must be page-aligned and `length` must be a
    /// multiple of 256. Watch flags are not copied (mirrors start clean).
    pub fn mirror(&mut self, mirror_start: u16, source_start: u16, length: u32) -> &mut Self {
        self.map.mirror(mirror_start, source_start, length);
        self
    }

    /// Remap a range of pages to a different region (for bank switching).
    ///
    /// Called at runtime when a bank register is written.
    pub fn remap_pages(
        &mut self,
        start_page: u8,
        page_count: u8,
        new_region_id: impl Into<RegionId>,
        new_base_offset: u32,
    ) {
        self.map
            .remap_pages(start_page, page_count, new_region_id, new_base_offset);
    }

    // -----------------------------------------------------------------------
    // Dispatch helpers (called on the hot path)
    // -----------------------------------------------------------------------

    /// Look up the page entry for an address.
    #[inline(always)]
    pub fn page(&self, addr: u16) -> &PageEntry {
        self.map.page(addr)
    }

    /// Compute the byte offset into the region for an address.
    ///
    /// This is `page.base_offset + (addr & 0xFF)` — the region-local index.
    #[inline(always)]
    pub fn region_offset(&self, addr: u16) -> usize {
        self.map.region_offset(addr)
    }

    // -----------------------------------------------------------------------
    // Backing memory access
    // -----------------------------------------------------------------------

    /// Side-effect-free read from backing memory. Returns `None` for I/O
    /// and unmapped regions (which have no backing store).
    ///
    /// Convenience over [`debug_peek`](Self::debug_peek) for callers that
    /// don't need to distinguish I/O from unmapped.
    #[inline]
    pub fn debug_read(&self, addr: u16) -> Option<u8> {
        match self.debug_peek(addr) {
            DebugRead::Backed { value, .. } => Some(value as u8),
            DebugRead::Io | DebugRead::Unmapped => None,
        }
    }

    /// Side-effect-free read with full address semantics.
    ///
    /// Distinguishes backed memory (RAM/ROM, value returned) from mapped
    /// I/O (reading the live device would have side effects) and unmapped
    /// addresses — so a memory viewer can label cells instead of showing
    /// a fake bus value.
    #[inline]
    pub fn debug_peek(&self, addr: u16) -> DebugRead {
        let page = self.page(addr);
        if page.region_id == UNMAPPED {
            return DebugRead::Unmapped;
        }
        match self
            .backing
            .read_region_offset(page.region_id, self.map.region_offset(addr))
        {
            Some(value) => DebugRead::Backed {
                value: value as u32,
                width: 1,
                region_id: page.region_id,
            },
            None => DebugRead::Io,
        }
    }

    /// Side-effect-free write to backing memory. No-op for I/O and unmapped regions.
    #[inline]
    pub fn debug_write(&mut self, addr: u16, data: u8) {
        let page = self.map.page(addr);
        self.backing
            .write_region_offset(page.region_id, self.map.region_offset(addr), data);
    }

    /// A *debugger* poke: like [`debug_write`](Self::debug_write), but records a
    /// [`DebugAccessSource::Frontend`] event in the trace ring (when tracing is
    /// on) so a script/console poke surfaces in the event trace instead of
    /// masquerading as a hardware store. Watchpoints are intentionally not
    /// checked — a poke is a debugger action, not machine execution.
    pub fn poke(&mut self, addr: u16, data: u8) {
        if self.trace.enabled() {
            let region = self.region_at(addr).map(|r| r.name);
            self.trace.record(DebugEvent {
                addr: Some(addr as u32),
                value: Some(data as u32),
                width: 1,
                region,
                detail: Some("frontend poke"),
                ..DebugEvent::new(
                    self.debug_cycle,
                    DebugAccessSource::Frontend,
                    DebugEventKind::MemoryWrite,
                )
            });
        }
        self.debug_write(addr, data);
    }

    /// Read a byte from backing memory (hot-path version).
    ///
    /// Only call on addresses mapped to regions with backing (RAM/ROM).
    /// Panics in debug builds if the region has no backing.
    #[inline(always)]
    pub fn read_backing(&self, addr: u16) -> u8 {
        let page = self.page(addr);
        self.backing
            .read(page.region_id, self.map.region_offset(addr))
    }

    /// Write a byte to backing memory (hot-path version).
    ///
    /// Only call on addresses mapped to regions with backing (RAM/ROM).
    /// Panics in debug builds if the region has no backing.
    #[inline(always)]
    pub fn write_backing(&mut self, addr: u16, data: u8) {
        let region_id = self.map.page(addr).region_id;
        self.backing
            .write(region_id, self.map.region_offset(addr), data);
    }

    /// Get a read-only slice of a region's backing store.
    ///
    /// Panics if the region has no backing (I/O or unregistered).
    pub fn region_data(&self, region_id: impl Into<RegionId>) -> &[u8] {
        self.backing.region_data(region_id)
    }

    /// Get a mutable slice of a region's backing store.
    ///
    /// Panics if the region has no backing (I/O or unregistered).
    pub fn region_data_mut(&mut self, region_id: impl Into<RegionId>) -> &mut [u8] {
        self.backing.region_data_mut(region_id)
    }

    /// Whether a region has bytes behind it, as opposed to being I/O or
    /// unregistered. The question [`Self::region_data`] panics rather than
    /// answers.
    pub fn has_backing(&self, region_id: impl Into<RegionId>) -> bool {
        self.backing.has_region(region_id)
    }

    /// Bulk-copy data into a region's backing store (e.g., ROM loading).
    ///
    /// `data` must exactly match the region's length.
    pub fn load_region(&mut self, region_id: impl Into<RegionId>, data: &[u8]) {
        self.backing.load_region(region_id, data);
    }

    /// Copy data into a region's backing store at the given byte offset.
    pub fn load_region_at(&mut self, region_id: impl Into<RegionId>, offset: usize, data: &[u8]) {
        self.backing.load_region_at(region_id, offset, data);
    }

    // -----------------------------------------------------------------------
    // Watchpoint methods
    // -----------------------------------------------------------------------

    /// Latch the owning CPU's execution context for watchpoint-hit
    /// attribution.
    ///
    /// Boards call this from `tick()` before executing the owning CPU
    /// (gated on [`has_any_watchpoints`](Self::has_any_watchpoints)),
    /// because bus dispatch cannot read CPU state while the CPU is
    /// mid-tick. `pc` should be `Some(cpu.pc)` when the CPU is at an
    /// instruction boundary — the address of the instruction about to
    /// execute — and `None` mid-instruction (the previously latched
    /// instruction address is retained).
    #[inline]
    pub fn latch_access_context(&mut self, cycle: u64, pc: Option<u32>) {
        self.debug_cycle = cycle;
        if pc.is_some() {
            self.debug_pc = pc;
        }
    }

    /// The latched instruction PC of the owning CPU, when known.
    ///
    /// Valid only while the board latches context each tick (see
    /// [`latch_access_context`](Self::latch_access_context)); used by
    /// boards to attribute debug events as well as watchpoint hits.
    #[inline]
    pub fn latched_pc(&self) -> Option<u32> {
        self.debug_pc
    }

    /// The cycle latched by the last
    /// [`latch_access_context`](Self::latch_access_context), for boards
    /// stamping their own events onto this space's timeline.
    #[inline]
    pub fn debug_cycle(&self) -> u64 {
        self.debug_cycle
    }

    /// Record a board-built event into this space's trace ring.
    ///
    /// Some events cannot be synthesized from a bus access alone — an
    /// interrupt assertion has no address, and a read routed through an I/O
    /// coprocessor is attributed to the chip rather than to the address it
    /// arrived on. Boards build those events themselves and hand them over
    /// here, so a board needs no trace ring of its own. Stamp them with
    /// [`debug_cycle`](Self::debug_cycle) and [`latched_pc`](Self::latched_pc)
    /// to keep them on the same timeline as the space's own events.
    ///
    /// No-op unless tracing is enabled.
    #[inline]
    pub fn trace_record(&mut self, event: DebugEvent) {
        if self.trace.enabled() {
            self.trace.record(event);
        }
    }

    /// Check a read access against this space's watchpoints. Returns true
    /// if the exact address is read-watched, queueing a hit.
    ///
    /// `cpu_index` is the CPU that owns this address space (matching
    /// `#[debug_map(cpu = N)]`); `master` is who performed the access.
    /// Call after the value is read; the hit records
    /// [`WatchpointPhase::After`].
    ///
    /// When no watchpoints are set anywhere (`active_watch_count == 0`),
    /// this compiles to a single branch-not-taken — effectively zero cost.
    #[inline(always)]
    pub fn watch_read(&mut self, cpu_index: usize, master: BusMaster, addr: u16, data: u8) -> bool {
        self.watch_read_with(cpu_index, master, addr, data, None)
    }

    /// [`watch_read`](Self::watch_read) with the name of the device that
    /// owns `addr` (for boards that can attribute I/O addresses).
    #[inline(always)]
    pub fn watch_read_with(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        data: u8,
        device: Option<&'static str>,
    ) -> bool {
        if self.active_watch_count == 0 {
            return false;
        }
        let page = &self.map.pages[(addr >> 8) as usize];
        if page.watch_read
            && self
                .watchpoints
                .fires(cpu_index, addr as u32, WatchpointKind::Read, data as u32, 1)
        {
            let hit = self.make_hit(cpu_index, master, addr, WatchpointKind::Read, data, device);
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Check a write access against this space's watchpoints. Returns true
    /// if the exact address is write-watched, queueing a hit.
    ///
    /// `cpu_index` is the CPU that owns this address space (matching
    /// `#[debug_map(cpu = N)]`); `master` is who performed the access.
    /// Call before applying the write's side effect; the hit records
    /// [`WatchpointPhase::Before`], so the metadata snapshot (including
    /// the region lookup) reflects pre-write state.
    #[inline(always)]
    pub fn watch_write(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        data: u8,
    ) -> bool {
        self.watch_write_with(cpu_index, master, addr, data, None)
    }

    /// [`watch_write`](Self::watch_write) with the name of the device that
    /// owns `addr` (for boards that can attribute I/O addresses).
    #[inline(always)]
    pub fn watch_write_with(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        data: u8,
        device: Option<&'static str>,
    ) -> bool {
        self.watch_write_annotated(
            cpu_index,
            master,
            addr,
            data,
            WriteAnnotation {
                device,
                ..WriteAnnotation::MEMORY
            },
        )
    }

    /// [`watch_write`](Self::watch_write) carrying a full [`WriteAnnotation`],
    /// for boards whose address decode is finer than the page-granular region
    /// map — an 8-byte latch or a single watchdog address cannot be a region,
    /// but it can still be labelled per write.
    #[inline(always)]
    pub fn watch_write_annotated(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        data: u8,
        annotation: WriteAnnotation,
    ) -> bool {
        let device = annotation.device;
        if self.trace.enabled() && !annotation.suppress_trace {
            self.record_write_event(cpu_index, master, addr, data, annotation);
        }
        if self.active_watch_count == 0 {
            return false;
        }
        let page = &self.map.pages[(addr >> 8) as usize];
        if page.watch_write
            && self.watchpoints.fires(
                cpu_index,
                addr as u32,
                WatchpointKind::Write,
                data as u32,
                1,
            )
        {
            let hit = self.make_hit(cpu_index, master, addr, WatchpointKind::Write, data, device);
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Build a canonical hit for a byte access through this map.
    ///
    /// `cycle`/`pc` come from the context latched by the board (see
    /// [`latch_access_context`](Self::latch_access_context)); the latched
    /// PC belongs to the owning CPU, so it is attributed only when that
    /// CPU performed the access (DMA and cross-CPU masters get `None`).
    fn make_hit(
        &self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        kind: WatchpointKind,
        value: u8,
        device: Option<&'static str>,
    ) -> WatchpointHit {
        WatchpointHit {
            cpu_index,
            source: DebugAccessSource::from(master),
            cycle: self.debug_cycle,
            pc: match master {
                BusMaster::Cpu(i) if i == cpu_index => self.debug_pc,
                _ => None,
            },
            addr: addr as u32,
            kind,
            phase: match kind {
                WatchpointKind::Read => WatchpointPhase::After,
                WatchpointKind::Write => WatchpointPhase::Before,
            },
            value: value as u32,
            width: 1,
            region: self.region_at(addr).map(|r| r.name),
            device,
        }
    }

    /// Consume the oldest queued watchpoint hit (polled by debugger after
    /// each tick). Hits queue in FIFO order; none are lost between polls.
    #[inline]
    pub fn take_hit(&mut self) -> Option<WatchpointHit> {
        self.watchpoints.take_hit()
    }

    /// True if any watchpoint is set on any page.
    #[inline]
    pub fn has_any_watchpoints(&self) -> bool {
        self.active_watch_count > 0
    }

    /// True if any debug observer (watchpoints or the write-event trace) is
    /// active. Boards gate `latch_access_context` on this so the cycle/PC
    /// context is available to both watchpoint hits and trace events.
    #[inline]
    pub fn debug_active(&self) -> bool {
        self.active_watch_count > 0 || self.trace.enabled()
    }

    // -- Write-event trace (headless `trace --events`) ----------------------

    /// Enable/disable the write-event ring. When enabled, every bus write
    /// through this space is recorded (see [`watch_write`](Self::watch_write)).
    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace.set_enabled(enabled);
    }

    /// Whether the write-event ring is currently recording.
    pub fn trace_enabled(&self) -> bool {
        self.trace.enabled()
    }

    /// Borrow the recorded write events (drained per frame by the trace tool).
    pub fn trace_events(&mut self) -> &[DebugEvent] {
        self.trace.events()
    }

    /// Clear the recorded write events.
    pub fn clear_trace_events(&mut self) {
        self.trace.clear();
    }

    /// Record one I/O-port write as a device-tagged [`DebugEventKind::IoWrite`].
    /// Boards call this explicitly from `Bus::io_write`, because the separate
    /// I/O space never flows through the memory map / [`watch_write`]. No-op
    /// unless tracing is enabled.
    ///
    /// [`watch_write`]: Self::watch_write
    pub fn trace_bus_io_write(
        &mut self,
        master: BusMaster,
        port: u16,
        data: u8,
        device: Option<&'static str>,
    ) {
        if !self.trace.enabled() {
            return;
        }
        let cpu = match master {
            BusMaster::Cpu(i) => Some(i),
            _ => None,
        };
        self.trace.record(DebugEvent {
            cpu_index: cpu,
            pc: self.debug_pc,
            addr: Some(port as u32),
            value: Some(data as u32),
            width: 1,
            region: None,
            device,
            ..DebugEvent::new(self.debug_cycle, master.into(), DebugEventKind::IoWrite)
        });
    }

    /// Record one bus write as a region-tagged event — [`DebugEventKind::MemoryWrite`]
    /// unless the annotation names another kind. Attribution mirrors
    /// [`make_hit`](Self::make_hit): the latched PC belongs to the owning CPU,
    /// so it is attached only when that CPU wrote.
    #[cold]
    fn record_write_event(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u16,
        data: u8,
        annotation: WriteAnnotation,
    ) {
        let region = self.region_at(addr).map(|r| r.name);
        let pc = match master {
            BusMaster::Cpu(i) if i == cpu_index => self.debug_pc,
            _ => None,
        };
        let cpu = match master {
            BusMaster::Cpu(i) => Some(i),
            _ => None,
        };
        self.trace.record(DebugEvent {
            cpu_index: cpu,
            pc,
            addr: Some(addr as u32),
            value: Some(data as u32),
            width: 1,
            region,
            device: annotation.device,
            detail: annotation.detail,
            ..DebugEvent::new(self.debug_cycle, master.into(), annotation.kind)
        });
    }

    /// Set an unconditional watchpoint on the exact address `addr` for the CPU
    /// that owns this address space.
    ///
    /// The page-level flag is set as a fast filter; the exact address is
    /// recorded in the watchpoint set so only that address fires.
    pub fn set_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {
        self.set_watchpoint_cond(cpu_index, addr, kind, WatchpointCondition::Always);
    }

    /// Set a watchpoint gated by `condition` on the exact address `addr`.
    /// The page-level fast-filter flag is set the same as for an
    /// unconditional watchpoint (conditions are evaluated when a hit fires).
    pub fn set_watchpoint_cond(
        &mut self,
        cpu_index: usize,
        addr: u16,
        kind: WatchpointKind,
        condition: WatchpointCondition,
    ) {
        self.watchpoints
            .set_cond(cpu_index, addr as u32, kind, condition);
        let page = &mut self.map.pages[(addr >> 8) as usize];
        let was_active = page.watch_read || page.watch_write;
        match kind {
            WatchpointKind::Read => page.watch_read = true,
            WatchpointKind::Write => page.watch_write = true,
        }
        if !was_active {
            self.active_watch_count += 1;
        }
    }

    /// Clear a watchpoint on the exact address `addr`.
    ///
    /// The page-level flag is only cleared if no other watched addresses
    /// on the same page still need it.
    pub fn clear_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {
        self.watchpoints.clear(cpu_index, addr as u32, kind);

        // Check if any remaining entries share this page and kind
        let page_idx = (addr >> 8) as usize;
        let still_has_kind = self
            .watchpoints
            .watched()
            .iter()
            .any(|w| (w.addr >> 8) as usize == page_idx && w.kind == kind);

        let page = &mut self.map.pages[page_idx];
        let was_active = page.watch_read || page.watch_write;
        if !still_has_kind {
            match kind {
                WatchpointKind::Read => page.watch_read = false,
                WatchpointKind::Write => page.watch_write = false,
            }
        }
        let is_active = page.watch_read || page.watch_write;
        if was_active && !is_active {
            self.active_watch_count = self.active_watch_count.saturating_sub(1);
        }
    }

    /// Clear all watchpoints on all pages.
    pub fn clear_all_watchpoints(&mut self) {
        for page in &mut self.map.pages {
            page.watch_read = false;
            page.watch_write = false;
        }
        self.active_watch_count = 0;
        self.watchpoints.clear_all();
    }

    // -----------------------------------------------------------------------
    // Introspection (for debugger / memory viewer)
    // -----------------------------------------------------------------------

    /// Get all named region descriptors.
    pub fn regions(&self) -> &[RegionDescriptor] {
        self.map.regions()
    }

    /// Get the region descriptor for the page containing `addr`, if mapped.
    pub fn region_at(&self, addr: u16) -> Option<&RegionDescriptor> {
        self.map.region_at(addr)
    }

    /// Backing regions this space saves, ascending, deduplicated.
    ///
    /// A region is saved when the CPU can change it (see
    /// [`AccessKind::is_cpu_writable`]) and it has backing. Keyed on the backing
    /// region id rather than on descriptors, because a mirrored or banked window
    /// is a second descriptor over bytes that are already accounted for.
    ///
    /// Public so the rule can be inspected rather than inferred from a save
    /// file: a board that expected a region to be persisted can check here.
    pub fn saved_region_ids(&self) -> Vec<RegionId> {
        let mut ids: Vec<RegionId> = Vec::new();
        for region in self.map.regions() {
            if !region.access.is_cpu_writable() || !self.backing.has_region(region.id) {
                continue;
            }
            if !ids.contains(&region.id) {
                ids.push(region.id);
            }
        }
        ids.sort_unstable();
        ids
    }

    fn region_name(&self, id: RegionId) -> &'static str {
        self.map
            .regions()
            .iter()
            .find(|r| r.id == id)
            .map_or("unnamed region", |r| r.name)
    }
}

/// Version of the address-space body, independent of any board's, and shared
/// with [`AddressSpace32`](crate::core::address_space32::AddressSpace32).
///
/// Version 2 added the page table.
pub(crate) const ADDRESS_SPACE_SAVE_VERSION: u8 = 2;

/// Chunk tag for the page table.
///
/// Every other tag in this body is a [`RegionId`], which is a `u8`, so anything
/// above `0xFF` cannot collide with one.
const PAGE_TABLE_TAG: u16 = 0x100;

/// An address space saves the bytes the CPU can change, and where they are
/// currently mapped.
///
/// ```text
/// body       := version:u8 | count:u16 | entry{count}
/// entry      := tag:u16 | len:u32 | payload
/// region     := tag < 0x100, payload = the region's bytes
/// page table := tag = 0x100, payload = 256 x (region_id:u8 | base_offset:u32)
/// ```
///
/// Which regions are saved is derived from the map rather than listed by each
/// board: a region is saved when its [`AccessKind`] is CPU-writable and it has
/// backing. ROM drops out because it is `ReadOnly`, and I/O because it has no
/// bytes. Boards used to enumerate this by hand, two lines a region, which made
/// "forgot to save a region" a silent bug with nothing to catch it.
///
/// # Why the page table is here
///
/// It is banking state. [`remap_pages`](Self::remap_pages) is how a board points
/// a window at a different part of a region, and the page table is the only
/// record of where it currently points.
///
/// It was left out at first, on the reasoning that a board rebuilds banking from
/// its own saved bank register and that persisting it here too would give one
/// fact two sources of truth. That was wrong, and in exactly the way this impl
/// exists to fix. The rebuild is not free: it is a call the board has to remember
/// to make from its `load_state`, it runs nowhere else on the normal path, and a
/// board that forgets it comes back with the CPU reading through pages that point
/// where they pointed before the load. That is the same silent bug as a forgotten
/// region, one level up.
///
/// So the map saves its own mapping, the board's bank register stays saved as the
/// value the game wrote, and no board needs a post-load hook to reconcile them.
///
/// Watchpoint flags are deliberately not restored: they belong to whoever is
/// debugging, not to the machine, so a load leaves them where they were set.
///
/// The count makes the body self-delimiting, so it is correct even inside a
/// parent that frames nothing.
impl Saveable for AddressSpace16 {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(ADDRESS_SPACE_SAVE_VERSION);
        let ids = self.saved_region_ids();
        w.write_u16_le(ids.len() as u16 + 1);
        for id in ids {
            w.write_tlv(id as u16, |w| w.write_raw(self.backing.region_data(id)));
        }
        w.write_tlv(PAGE_TABLE_TAG, |w| {
            for page in &self.map.pages {
                w.write_u8(page.region_id);
                w.write_u32_le(page.base_offset);
            }
        });
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(ADDRESS_SPACE_SAVE_VERSION)?;
        let expected = self.saved_region_ids();
        let count = r.read_u16_le()?;
        let mut seen: Vec<RegionId> = Vec::new();
        let mut saw_page_table = false;

        for i in 0..count {
            let at = r.offset();
            let Some((tag, len)) = r.read_tag_len()? else {
                return Err(SaveError::InvalidFormat(format!(
                    "address space declares {count} entries but ran out after {i}"
                )));
            };

            if tag == PAGE_TABLE_TAG {
                saw_page_table = true;
                r.read_payload(tag, len, at, "page table", |r| self.load_pages(r))?;
                continue;
            }

            let id = tag as RegionId;
            // A region this build's map does not have: another configuration of
            // the same board, or one since removed. Skipping by length is what
            // makes either harmless.
            if u16::from(id) != tag || !expected.contains(&id) {
                r.skip_unknown(tag, len, at)?;
                continue;
            }
            if seen.contains(&id) {
                return Err(SaveError::InvalidFormat(format!(
                    "region {id} appears twice"
                )));
            }
            seen.push(id);
            let name = self.region_name(id);
            r.read_payload(tag, len, at, name, |r| {
                r.read_raw_into(self.backing.region_data_mut(id))
            })?;
        }

        if let Some(missing) = expected.iter().find(|id| !seen.contains(id)) {
            return Err(SaveError::InvalidFormat(format!(
                "region {} ({}) is absent from the save",
                missing,
                self.region_name(*missing)
            )));
        }
        if !saw_page_table {
            return Err(SaveError::InvalidFormat(
                "the page table is absent from the save".into(),
            ));
        }
        Ok(())
    }
}

impl AddressSpace16 {
    /// Restore page mappings, leaving each page's watch flags alone.
    fn load_pages(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        for index in 0..self.map.pages.len() {
            let region_id = r.read_u8()?;
            let base_offset = r.read_u32_le()?;
            // A page pointing at a region this map does not have would send
            // every read through it somewhere arbitrary, so refuse rather than
            // map it. UNMAPPED is always legitimate.
            if region_id != UNMAPPED && !self.map.regions().iter().any(|d| d.id == region_id) {
                return Err(SaveError::InvalidFormat(format!(
                    "page {index:#04X} maps region {region_id}, which this map does not have"
                )));
            }
            let page = &mut self.map.pages[index];
            page.region_id = region_id;
            page.base_offset = base_offset;
        }
        Ok(())
    }
}

impl Default for AddressSpace16 {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Decode-only tests for the bare [`AddressMap16`] (no backing, no
/// watchpoints).
#[cfg(test)]
mod address_map16_tests {
    use super::*;

    const RAM: RegionId = 1;
    const ROM: RegionId = 2;
    const IO: RegionId = 3;

    #[test]
    fn new_map_is_all_unmapped() {
        let map = AddressMap16::new();
        for page_idx in 0..=255u8 {
            let addr = (page_idx as u16) << 8;
            assert_eq!(map.page(addr).region_id, UNMAPPED);
        }
        assert!(map.regions().is_empty());
    }

    #[test]
    fn region_sets_page_entries_and_descriptor() {
        let mut map = AddressMap16::new();
        map.region(RAM, "RAM", 0x4000, 0x1000, AccessKind::ReadWrite);

        for page_idx in 0x40..0x50u8 {
            let addr = (page_idx as u16) << 8;
            let entry = map.page(addr);
            assert_eq!(entry.region_id, RAM);
            assert_eq!(entry.base_offset, (page_idx as u32 - 0x40) * 256);
        }
        assert_eq!(map.page(0x3F00).region_id, UNMAPPED);
        assert_eq!(map.page(0x5000).region_id, UNMAPPED);

        let desc = &map.regions()[0];
        assert_eq!(desc.name, "RAM");
        assert_eq!(desc.start_addr, 0x4000);
        assert_eq!(desc.end_addr, 0x4FFF);
        assert_eq!(desc.access, AccessKind::ReadWrite);
    }

    #[test]
    fn region_offset_is_region_local() {
        let mut map = AddressMap16::new();
        map.region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        assert_eq!(map.region_offset(0xD000), 0);
        assert_eq!(map.region_offset(0xD042), 0x42);
        assert_eq!(map.region_offset(0xFFFF), 0x2FFF);
    }

    #[test]
    fn mirror_copies_page_entries() {
        let mut map = AddressMap16::new();
        map.region(RAM, "Work RAM", 0x4C00, 0x0400, AccessKind::ReadWrite)
            .mirror(0xCC00, 0x4C00, 0x0400);

        let src = map.page(0x4C00);
        let dst = map.page(0xCC00);
        assert_eq!(src.region_id, dst.region_id);
        assert_eq!(src.base_offset, dst.base_offset);
    }

    #[test]
    fn remap_pages_switches_region() {
        let mut map = AddressMap16::new();
        map.region(RAM, "RAM", 0x0000, 0x9000, AccessKind::ReadWrite);

        map.remap_pages(0x00, 0x90, ROM, 0);
        assert_eq!(map.page(0x0000).region_id, ROM);
        assert_eq!(map.page(0x8F00).region_id, ROM);

        map.remap_pages(0x00, 0x90, RAM, 0);
        assert_eq!(map.page(0x0000).region_id, RAM);
    }

    #[test]
    fn remap_pages_supports_offsets_beyond_64k() {
        let mut map = AddressMap16::new();
        map.region(RAM, "RAM", 0x0000, 0x4000, AccessKind::ReadWrite);

        // Bank window pointing 128 KB into a large backing region —
        // exercises the u32 widening of PageEntry::base_offset.
        map.remap_pages(0x00, 0x40, ROM, 0x2_0000);
        assert_eq!(map.page(0x0000).base_offset, 0x2_0000);
        assert_eq!(map.page(0x0100).base_offset, 0x2_0100);
        assert_eq!(map.region_offset(0x3FFF), 0x2_3FFF);
    }

    #[test]
    fn region_at_finds_descriptor() {
        let mut map = AddressMap16::new();
        map.region(RAM, "Work RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x1000, AccessKind::Io);

        assert_eq!(map.region_at(0x1234).unwrap().name, "Work RAM");
        assert_eq!(map.region_at(0xC042).unwrap().name, "I/O");
        assert!(map.region_at(0xA000).is_none());
    }
}

/// Composed [`AddressSpace16`] tests (decode + backing + watchpoints).
#[cfg(test)]
mod tests {
    use super::*;

    const RAM: RegionId = 1;
    const ROM: RegionId = 2;
    const IO: RegionId = 3;

    #[test]
    fn new_map_is_all_unmapped() {
        let map = AddressSpace16::new();
        for page_idx in 0..=255u8 {
            let addr = (page_idx as u16) << 8;
            assert_eq!(map.page(addr).region_id, UNMAPPED);
        }
        assert!(map.regions().is_empty());
    }

    #[test]
    fn region_populates_pages_and_descriptor() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);

        // Pages 0x00..0x7F should be RAM
        for page_idx in 0x00..0x80u8 {
            let addr = (page_idx as u16) << 8;
            let entry = map.page(addr);
            assert_eq!(entry.region_id, RAM);
            assert_eq!(entry.base_offset, (page_idx as u32) * 256);
        }

        // Pages 0x80..0xFF should still be unmapped
        for page_idx in 0x80..=0xFFu8 {
            let addr = (page_idx as u16) << 8;
            assert_eq!(map.page(addr).region_id, UNMAPPED);
        }

        assert_eq!(map.regions().len(), 1);
        assert_eq!(map.regions()[0].name, "RAM");
        assert_eq!(map.regions()[0].start_addr, 0x0000);
        assert_eq!(map.regions()[0].end_addr, 0x7FFF);
    }

    #[test]
    fn region_offset_calculation() {
        let mut map = AddressSpace16::new();
        map.region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        // Address 0xD000 → page 0xD0, base_offset=0, low byte=0x00 → offset 0
        assert_eq!(map.region_offset(0xD000), 0);
        // Address 0xD042 → page 0xD0, base_offset=0, low byte=0x42 → offset 0x42
        assert_eq!(map.region_offset(0xD042), 0x42);
        // Address 0xD100 → page 0xD1, base_offset=0x100, low byte=0x00 → offset 0x100
        assert_eq!(map.region_offset(0xD100), 0x100);
        // Address 0xFFFF → page 0xFF, base_offset=0x2F00, low byte=0xFF → offset 0x2FFF
        assert_eq!(map.region_offset(0xFFFF), 0x2FFF);
    }

    #[test]
    fn multiple_regions() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0x8000, 0x8000, AccessKind::ReadOnly);

        assert_eq!(map.page(0x0000).region_id, RAM);
        assert_eq!(map.page(0x7F00).region_id, RAM);
        assert_eq!(map.page(0x8000).region_id, ROM);
        assert_eq!(map.page(0xFF00).region_id, ROM);
        assert_eq!(map.regions().len(), 2);
    }

    #[test]
    fn mirror_copies_entries() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0x8000, 0x8000, AccessKind::ReadOnly);

        // Pac-Man style: mirror lower 32K to upper 32K
        // (In practice you'd mirror before setting ROM, but this tests the mechanic)
        let mut map2 = AddressSpace16::new();
        map2.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .mirror(0x8000, 0x0000, 0x8000);

        // Upper half should mirror lower half
        assert_eq!(map2.page(0x8000).region_id, RAM);
        assert_eq!(map2.page(0x8000).base_offset, 0); // same as page 0x00
        assert_eq!(map2.page(0xFF00).region_id, RAM);
        assert_eq!(map2.page(0xFF00).base_offset, map2.page(0x7F00).base_offset);
    }

    #[test]
    fn mirror_preserves_region_offsets() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Work RAM", 0x4C00, 0x0400, AccessKind::ReadWrite)
            .mirror(0xCC00, 0x4C00, 0x0400);

        let src = map.page(0x4C00);
        let dst = map.page(0xCC00);
        assert_eq!(src.region_id, dst.region_id);
        assert_eq!(src.base_offset, dst.base_offset);
    }

    #[test]
    fn remap_pages_for_bank_switching() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Video RAM", 0x0000, 0x9000, AccessKind::ReadWrite);

        // Bank in ROM over pages 0x00..0x90
        map.remap_pages(0x00, 0x90, ROM, 0);

        assert_eq!(map.page(0x0000).region_id, ROM);
        assert_eq!(map.page(0x0000).base_offset, 0);
        assert_eq!(map.page(0x8F00).region_id, ROM);

        // Bank back to RAM
        map.remap_pages(0x00, 0x90, RAM, 0);
        assert_eq!(map.page(0x0000).region_id, RAM);
    }

    // -----------------------------------------------------------------------
    // Watchpoint tests
    // -----------------------------------------------------------------------

    #[test]
    fn no_watchpoints_by_default() {
        let map = AddressSpace16::new();
        assert!(!map.has_any_watchpoints());
        assert_eq!(map.active_watch_count, 0);
    }

    #[test]
    fn check_watch_is_noop_when_no_watchpoints() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x1234, 0x42));
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x1234, 0x42));
        assert!(map.take_hit().is_none());
    }

    #[test]
    fn read_watchpoint_fires() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x4000, WatchpointKind::Read);
        assert!(map.has_any_watchpoints());
        assert_eq!(map.active_watch_count, 1);

        // Read at the exact watched address → hit
        assert!(map.watch_read(0, BusMaster::Cpu(0), 0x4000, 0xAB));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x4000);
        assert_eq!(hit.kind, WatchpointKind::Read);
        assert_eq!(hit.value, 0xAB);

        // Read at a different address on the same page → no hit (exact match only)
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x4042, 0xCD));

        // Write at the watched address → no hit (only read watchpoint set)
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x4000, 0xCD));
        assert!(map.take_hit().is_none());

        // Read in a different page → no hit
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x5000, 0x00));
    }

    #[test]
    fn write_watchpoint_fires() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x1000, WatchpointKind::Write);

        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x1000, 0x99));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x1000);
        assert_eq!(hit.kind, WatchpointKind::Write);
        assert_eq!(hit.value, 0x99);

        // Read on same page → no hit
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x1000, 0x00));
    }

    #[test]
    fn both_read_and_write_watchpoint() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x2000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x2000, WatchpointKind::Write);
        // Same page, so active_watch_count should still be 1
        assert_eq!(map.active_watch_count, 1);

        assert!(map.watch_read(0, BusMaster::Cpu(0), 0x2000, 0x11));
        map.take_hit();
        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x2000, 0x22));
    }

    #[test]
    fn clear_watchpoint() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x3000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x3000, WatchpointKind::Write);
        assert_eq!(map.active_watch_count, 1);

        // Clear read but keep write
        map.clear_watchpoint(0, 0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1); // still active (write remains)
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x3000, 0x00));
        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x3000, 0x00));

        // Clear write too
        map.take_hit();
        map.clear_watchpoint(0, 0x3000, WatchpointKind::Write);
        assert_eq!(map.active_watch_count, 0);
        assert!(!map.has_any_watchpoints());
    }

    #[test]
    fn clear_all_watchpoints() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x1000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x2000, WatchpointKind::Write);
        map.set_watchpoint(0, 0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 3);

        map.clear_all_watchpoints();
        assert_eq!(map.active_watch_count, 0);
        assert!(!map.has_any_watchpoints());
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x1000, 0x00));
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x2000, 0x00));
    }

    #[test]
    fn multiple_watchpoints_on_different_pages() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x1000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x2000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 2);

        // Only exact addresses fire
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x0000, 0x00));
        assert!(map.watch_read(0, BusMaster::Cpu(0), 0x1000, 0x42));
        map.take_hit();
        // Different address on same page → no hit
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x1050, 0x00));
    }

    #[test]
    fn watchpoint_exact_address_only() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x2042, WatchpointKind::Write);

        // Same page, different address → no hit
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x2000, 0x11));
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x2041, 0x22));
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x2043, 0x33));
        assert!(!map.watch_write(0, BusMaster::Cpu(0), 0x20FF, 0x44));

        // Exact address → hit
        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x2042, 0x55));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x2042);
        assert_eq!(hit.value, 0x55);
    }

    #[test]
    fn clear_one_of_two_on_same_page() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);

        map.set_watchpoint(0, 0x3000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x3010, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1); // same page

        // Clear the first; page flag should stay (0x3010 still active)
        map.clear_watchpoint(0, 0x3000, WatchpointKind::Read);
        assert_eq!(map.active_watch_count, 1);
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x3000, 0x00)); // cleared
        assert!(map.watch_read(0, BusMaster::Cpu(0), 0x3010, 0xAA)); // still active
    }

    // -----------------------------------------------------------------------
    // Introspection tests
    // -----------------------------------------------------------------------

    #[test]
    fn region_at_returns_descriptor() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Work RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "Program ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        let r = map.region_at(0x1234).unwrap();
        assert_eq!(r.name, "Work RAM");
        assert_eq!(r.id, RAM);

        let r = map.region_at(0xE000).unwrap();
        assert_eq!(r.name, "Program ROM");

        // Unmapped address
        assert!(map.region_at(0xC000).is_none());
    }

    #[test]
    fn region_descriptors_have_correct_bounds() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Video RAM", 0x0000, 0xC000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x1000, AccessKind::Io)
            .region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        assert_eq!(map.regions().len(), 3);

        assert_eq!(map.regions()[0].start_addr, 0x0000);
        assert_eq!(map.regions()[0].end_addr, 0xBFFF);

        assert_eq!(map.regions()[1].start_addr, 0xC000);
        assert_eq!(map.regions()[1].end_addr, 0xCFFF);

        assert_eq!(map.regions()[2].start_addr, 0xD000);
        assert_eq!(map.regions()[2].end_addr, 0xFFFF);
    }

    #[test]
    fn multiple_hits_queue_in_order() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x10000, AccessKind::ReadWrite);
        map.set_watchpoint(0, 0x1000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x1001, WatchpointKind::Read);

        map.watch_read(0, BusMaster::Cpu(0), 0x1000, 0xAA);
        map.watch_read(0, BusMaster::Cpu(0), 0x1001, 0xBB); // queued behind the first

        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x1000);
        assert_eq!(hit.value, 0xAA);
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.addr, 0x1001);
        assert_eq!(hit.value, 0xBB);
        assert!(map.take_hit().is_none());
    }

    #[test]
    fn hit_records_region_name() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Work RAM", 0x0000, 0x8000, AccessKind::ReadWrite);
        map.set_watchpoint(0, 0x1234, WatchpointKind::Write);

        map.watch_write(0, BusMaster::Cpu(0), 0x1234, 0x42);
        assert_eq!(map.take_hit().unwrap().region, Some("Work RAM"));
    }

    #[test]
    fn hit_records_cpu_index_and_source() {
        // A sound-CPU map (cpu 1), accessed by CPU 1 and by DMA.
        let mut map = AddressSpace16::new();
        map.region(RAM, "Sound RAM", 0x0000, 0x1000, AccessKind::ReadWrite);
        map.set_watchpoint(1, 0x0200, WatchpointKind::Read);

        // Wrong owning CPU index → no match
        assert!(!map.watch_read(0, BusMaster::Cpu(0), 0x0200, 0x11));

        assert!(map.watch_read(1, BusMaster::Cpu(1), 0x0200, 0x22));
        assert!(map.watch_read(1, BusMaster::Dma, 0x0200, 0x33));

        let hit = map.take_hit().unwrap();
        assert_eq!(hit.cpu_index, 1);
        assert_eq!(hit.source, DebugAccessSource::Cpu(1));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.source, DebugAccessSource::Dma);
    }

    #[test]
    fn hit_records_latched_cycle_and_pc() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);
        map.set_watchpoint(0, 0x4000, WatchpointKind::Write);

        // Board latches context at an instruction boundary, then the CPU
        // performs the watched access mid-instruction.
        map.latch_access_context(123_456, Some(0x1BCC));
        map.latch_access_context(123_457, None); // mid-instruction: PC retained

        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x4000, 0x12));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.cycle, 123_457);
        assert_eq!(hit.pc, Some(0x1BCC));
    }

    #[test]
    fn pc_attributed_only_to_owning_cpu() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);
        map.set_watchpoint(0, 0x4000, WatchpointKind::Write);
        map.latch_access_context(100, Some(0x2000));

        // DMA access: latched PC belongs to the owning CPU, not the master.
        assert!(map.watch_write(0, BusMaster::Dma, 0x4000, 0x12));
        assert_eq!(map.take_hit().unwrap().pc, None);

        // Owning CPU access: PC attributed.
        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x4000, 0x12));
        assert_eq!(map.take_hit().unwrap().pc, Some(0x2000));
    }

    #[test]
    fn hit_records_phase_by_kind() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);
        map.set_watchpoint(0, 0x4000, WatchpointKind::Read);
        map.set_watchpoint(0, 0x4000, WatchpointKind::Write);

        assert!(map.watch_read(0, BusMaster::Cpu(0), 0x4000, 0xAA));
        assert_eq!(map.take_hit().unwrap().phase, WatchpointPhase::After);

        assert!(map.watch_write(0, BusMaster::Cpu(0), 0x4000, 0xBB));
        assert_eq!(map.take_hit().unwrap().phase, WatchpointPhase::Before);
    }

    #[test]
    fn hit_records_device_name() {
        let mut map = AddressSpace16::new();
        map.region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);
        map.set_watchpoint(0, 0xC004, WatchpointKind::Write);

        assert!(map.watch_write_with(0, BusMaster::Cpu(0), 0xC004, 0x01, Some("Widget PIA")));
        let hit = map.take_hit().unwrap();
        assert_eq!(hit.device, Some("Widget PIA"));
        assert_eq!(hit.region, Some("I/O"));
    }

    // -----------------------------------------------------------------------
    // Backing memory tests
    // -----------------------------------------------------------------------

    #[test]
    fn region_allocates_backing_for_rw() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite);

        assert_eq!(map.region_data(RAM).len(), 0x0400);
        assert!(map.region_data(RAM).iter().all(|&b| b == 0));
    }

    #[test]
    fn region_allocates_backing_for_rom() {
        let mut map = AddressSpace16::new();
        map.region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        assert_eq!(map.region_data(ROM).len(), 0x3000);
    }

    #[test]
    fn io_region_has_no_backing() {
        let mut map = AddressSpace16::new();
        map.region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);

        assert!(map.debug_read(0xC042).is_none());
    }

    #[test]
    fn debug_read_returns_backing_data() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x100, AccessKind::Io)
            .region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        // Write via region_data_mut
        map.region_data_mut(RAM)[0x1234] = 0xAB;
        map.region_data_mut(ROM)[0x0042] = 0xCD;

        // Read back via debug_read
        assert_eq!(map.debug_read(0x1234), Some(0xAB));
        assert_eq!(map.debug_read(0xD042), Some(0xCD));

        // I/O returns None
        assert_eq!(map.debug_read(0xC042), None);

        // Unmapped returns None
        assert_eq!(map.debug_read(0xA000), None);
    }

    #[test]
    fn debug_peek_distinguishes_backed_io_unmapped() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);

        map.region_data_mut(RAM)[0x1234] = 0xAB;

        assert_eq!(
            map.debug_peek(0x1234),
            DebugRead::Backed {
                value: 0xAB,
                width: 1,
                region_id: RAM
            }
        );
        assert_eq!(map.debug_peek(0xC042), DebugRead::Io);
        assert_eq!(map.debug_peek(0xA000), DebugRead::Unmapped);
    }

    #[test]
    fn debug_peek_follows_mirrors_and_banking() {
        const BANK_ROM: RegionId = 4;

        let mut map = AddressSpace16::new();
        map.region(ROM, "ROM", 0xF000, 0x1000, AccessKind::ReadOnly)
            .mirror(0xB000, 0xF000, 0x1000)
            .backing_region(BANK_ROM, "Banked ROM", 0x1000);

        map.region_data_mut(ROM)[0x42] = 0xEE;
        map.region_data_mut(BANK_ROM)[0x42] = 0x99;

        // Mirror resolves to the same backing byte
        assert_eq!(
            map.debug_peek(0xB042),
            DebugRead::Backed {
                value: 0xEE,
                width: 1,
                region_id: ROM
            }
        );

        // After banking, the same address peeks the banked region
        map.remap_pages(0xF0, 0x10, BANK_ROM, 0);
        assert_eq!(
            map.debug_peek(0xF042),
            DebugRead::Backed {
                value: 0x99,
                width: 1,
                region_id: BANK_ROM
            }
        );
    }

    #[test]
    fn debug_write_updates_backing() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(IO, "I/O", 0xC000, 0x100, AccessKind::Io);

        map.debug_write(0x1234, 0x42);
        assert_eq!(map.debug_read(0x1234), Some(0x42));

        // I/O write is a no-op (doesn't panic)
        map.debug_write(0xC042, 0xFF);
    }

    #[test]
    fn read_write_backing_hot_path() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite);

        map.write_backing(0x4000, 0xBE);
        assert_eq!(map.read_backing(0x4000), 0xBE);
    }

    #[test]
    fn load_region_copies_data() {
        let mut map = AddressSpace16::new();
        map.region(ROM, "ROM", 0xD000, 0x0400, AccessKind::ReadOnly);

        let rom_data: Vec<u8> = (0..0x0400).map(|i| (i & 0xFF) as u8).collect();
        map.load_region(ROM, &rom_data);

        assert_eq!(map.debug_read(0xD000), Some(0x00));
        assert_eq!(map.debug_read(0xD0FF), Some(0xFF));
        assert_eq!(map.debug_read(0xD100), Some(0x00));
    }

    #[test]
    fn load_region_at_partial() {
        let mut map = AddressSpace16::new();
        map.region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        let chunk = [0xAA, 0xBB, 0xCC];
        map.load_region_at(ROM, 0x100, &chunk);

        assert_eq!(map.debug_read(0xD100), Some(0xAA));
        assert_eq!(map.debug_read(0xD101), Some(0xBB));
        assert_eq!(map.debug_read(0xD102), Some(0xCC));
        assert_eq!(map.debug_read(0xD103), Some(0x00)); // not written
    }

    #[test]
    fn remap_pages_switches_backing() {
        const BANK_ROM: RegionId = 4;

        let mut map = AddressSpace16::new();
        map.region(RAM, "Video RAM", 0x0000, 0x9000, AccessKind::ReadWrite)
            .backing_region(BANK_ROM, "Banked ROM", 0x9000);

        // Write different values to each region's backing
        map.region_data_mut(RAM)[0x1000] = 0xAA; // Video RAM at offset 0x1000
        map.region_data_mut(BANK_ROM)[0x1000] = 0xBB; // Banked ROM at offset 0x1000

        // Initially reads Video RAM
        assert_eq!(map.read_backing(0x1000), 0xAA);

        // Remap pages 0x00..0x90 to Banked ROM
        map.remap_pages(0x00, 0x90, BANK_ROM, 0);
        assert_eq!(map.read_backing(0x1000), 0xBB);

        // Remap back to Video RAM
        map.remap_pages(0x00, 0x90, RAM, 0);
        assert_eq!(map.read_backing(0x1000), 0xAA);
    }

    #[test]
    fn mirror_reads_same_backing() {
        let mut map = AddressSpace16::new();
        map.region(ROM, "Sound ROM", 0xF000, 0x1000, AccessKind::ReadOnly)
            .mirror(0xB000, 0xF000, 0x1000);

        map.region_data_mut(ROM)[0x42] = 0xEE;

        // Both canonical and mirror addresses read the same byte
        assert_eq!(map.debug_read(0xF042), Some(0xEE));
        assert_eq!(map.debug_read(0xB042), Some(0xEE));
    }

    #[test]
    fn backing_region_has_no_page_mapping() {
        const OVERLAY: RegionId = 5;

        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .backing_region(OVERLAY, "Overlay", 0x8000);

        // Overlay has backing
        assert_eq!(map.region_data(OVERLAY).len(), 0x8000);

        // But pages 0x00..0x7F still point to RAM, not OVERLAY
        assert_eq!(map.page(0x0000).region_id, RAM);
        assert_eq!(map.page(0x7F00).region_id, RAM);
    }

    #[test]
    fn multiple_regions_share_backing_vec() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "RAM", 0x0000, 0x8000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0xD000, 0x3000, AccessKind::ReadOnly);

        // Both get independent backing
        map.region_data_mut(RAM)[0] = 0x11;
        map.region_data_mut(ROM)[0] = 0x22;

        assert_eq!(map.debug_read(0x0000), Some(0x11));
        assert_eq!(map.debug_read(0xD000), Some(0x22));
    }

    #[test]
    fn trace_records_region_tagged_writes_only_when_enabled() {
        let mut map = AddressSpace16::new();
        map.region(RAM, "Video RAM", 0x8000, 0x0800, AccessKind::ReadWrite);

        // Disabled by default: watch_write records nothing, and debug_active is
        // false (so boards skip latching the access context).
        assert!(!map.trace_enabled());
        assert!(!map.debug_active());
        map.watch_write(0, BusMaster::Cpu(0), 0x8000, 0x11);
        assert!(map.trace_events().is_empty());

        // Enable: writes are recorded with the region name resolved.
        map.set_trace_enabled(true);
        assert!(map.debug_active());
        map.latch_access_context(12_345, Some(0x0100));
        map.watch_write(0, BusMaster::Cpu(0), 0x8010, 0xAB); // mapped → region set
        map.watch_write(0, BusMaster::Cpu(0), 0x0000, 0xCD); // unmapped → region None

        let events = map.trace_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, DebugEventKind::MemoryWrite);
        assert_eq!(events[0].addr, Some(0x8010));
        assert_eq!(events[0].value, Some(0xAB));
        assert_eq!(events[0].cycle, 12_345);
        assert_eq!(events[0].pc, Some(0x0100));
        assert_eq!(events[0].region, Some("Video RAM"));
        assert_eq!(events[1].region, None); // unmapped address still recorded

        // I/O writes are captured via the explicit io hook, device-tagged.
        map.trace_bus_io_write(BusMaster::Cpu(0), 0x00E0, 0x01, Some("watchdog"));
        assert_eq!(map.trace_events().len(), 3);
        let io = &map.trace_events()[2];
        assert_eq!(io.kind, DebugEventKind::IoWrite);
        assert_eq!(io.device, Some("watchdog"));

        // Clearing empties the ring; disabling stops recording.
        map.clear_trace_events();
        assert!(map.trace_events().is_empty());
        map.set_trace_enabled(false);
        map.watch_write(0, BusMaster::Cpu(0), 0x8000, 0x22);
        assert!(map.trace_events().is_empty());
    }

    #[test]
    fn write_annotation_labels_decode_finer_than_a_region() {
        // Regions are page-granular, so hardware that decodes finer than a page
        // — an 8-byte latch, a single watchdog address — is labelled per write.
        let mut map = AddressSpace16::new();
        map.region(RAM, "I/O", 0x6800, 0x0100, AccessKind::ReadWrite);
        map.set_trace_enabled(true);
        map.latch_access_context(7, Some(0x0200));

        map.watch_write_annotated(
            0,
            BusMaster::Cpu(0),
            0x6805,
            0x0F,
            WriteAnnotation::device("Namco WSG"),
        );
        map.watch_write_annotated(
            0,
            BusMaster::Cpu(0),
            0x6823,
            0x01,
            WriteAnnotation::detail("Misc latch", "sub/sound reset"),
        );
        map.watch_write_annotated(
            0,
            BusMaster::Cpu(0),
            0x6830,
            0x00,
            WriteAnnotation {
                kind: DebugEventKind::Watchdog,
                detail: Some("watchdog cleared"),
                ..WriteAnnotation::MEMORY
            },
        );

        let events = map.trace_events();
        assert_eq!(events.len(), 3);

        // Device name and the region the address falls in are independent: the
        // region still resolves, so the debugger can show both.
        assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
        assert_eq!(events[0].device, Some("Namco WSG"));
        assert_eq!(events[0].detail, None);
        assert_eq!(events[0].region, Some("I/O"));

        assert_eq!(events[1].device, Some("Misc latch"));
        assert_eq!(events[1].detail, Some("sub/sound reset"));

        // The kind is the board's to choose — a watchdog strobe is not a store.
        assert_eq!(events[2].kind, DebugEventKind::Watchdog);
        assert_eq!(events[2].detail, Some("watchdog cleared"));
        assert_eq!(events[2].device, None);
    }

    #[test]
    fn trace_record_puts_board_events_on_the_space_timeline() {
        // Interrupts have no address and I/O-coprocessor reads are attributed
        // to the chip, so boards build those events and hand them over.
        let mut map = AddressSpace16::new();
        map.region(RAM, "Work RAM", 0x8000, 0x0100, AccessKind::ReadWrite);

        // Gated like every other trace path.
        map.trace_record(DebugEvent::new(
            1,
            DebugAccessSource::Cpu(0),
            DebugEventKind::InterruptAssert,
        ));
        assert!(map.trace_events().is_empty());

        map.set_trace_enabled(true);
        map.latch_access_context(4_242, Some(0x0300));
        map.watch_write(0, BusMaster::Cpu(0), 0x8000, 0x01);
        map.trace_record(DebugEvent {
            cpu_index: Some(2),
            detail: Some("sound NMI (scanline timer)"),
            ..DebugEvent::new(
                map.debug_cycle(),
                DebugAccessSource::Cpu(2),
                DebugEventKind::InterruptAssert,
            )
        });

        let events = map.trace_events();
        assert_eq!(events.len(), 2, "board events share the space's ring");
        assert_eq!(events[0].kind, DebugEventKind::MemoryWrite);
        assert_eq!(events[1].kind, DebugEventKind::InterruptAssert);
        assert_eq!(events[1].detail, Some("sound NMI (scanline timer)"));
        // Same timeline: the board stamped its event from debug_cycle().
        assert_eq!(events[0].cycle, events[1].cycle);
    }

    #[test]
    fn suppressed_annotation_still_fires_watchpoints() {
        // Addresses whose events a dedicated device path already records must
        // not be double-counted, but must still trip a watchpoint.
        let mut map = AddressSpace16::new();
        map.region(RAM, "I/O", 0x7000, 0x0100, AccessKind::ReadWrite);
        map.set_trace_enabled(true);
        map.set_watchpoint(0, 0x7000, WatchpointKind::Write);

        let fired = map.watch_write_annotated(
            0,
            BusMaster::Cpu(0),
            0x7000,
            0x42,
            WriteAnnotation::SUPPRESSED,
        );

        assert!(fired, "watchpoint fires even with tracing suppressed");
        assert!(
            map.trace_events().is_empty(),
            "suppressed write records no event"
        );
        assert_eq!(map.take_hit().map(|h| h.addr), Some(0x7000));
    }

    #[test]
    fn watch_write_defaults_match_the_plain_memory_annotation() {
        // watch_write/watch_write_with are the annotated call with MEMORY, so
        // the pre-existing callers keep recording exactly what they did before.
        let mut map = AddressSpace16::new();
        map.region(RAM, "Work RAM", 0x8000, 0x0100, AccessKind::ReadWrite);
        map.set_trace_enabled(true);

        map.watch_write(0, BusMaster::Cpu(0), 0x8000, 0x01);
        map.watch_write_with(0, BusMaster::Cpu(0), 0x8001, 0x02, Some("PIA"));

        let events = map.trace_events();
        assert_eq!(events[0].kind, DebugEventKind::MemoryWrite);
        assert_eq!(events[0].device, None);
        assert_eq!(events[0].detail, None);
        // watch_write_with keeps recording a MemoryWrite, only device-tagged.
        assert_eq!(events[1].kind, DebugEventKind::MemoryWrite);
        assert_eq!(events[1].device, Some("PIA"));
        assert_eq!(WriteAnnotation::default(), WriteAnnotation::MEMORY);
    }
}
