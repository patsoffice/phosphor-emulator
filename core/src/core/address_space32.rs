//! Composed address-space container for 32-bit machines.
//!
//! [`AddressMap32`] is the sparse decode sibling of
//! [`AddressMap16`](crate::core::memory_map::AddressMap16). A flat page
//! table is the wrong shape for a 32-bit address space (a 256-byte page
//! table would need 16,777,216 entries), and arcade/computer maps of this
//! class are sparse: a handful of ROM/RAM/I-O windows in a sea of unmapped
//! space. Decode is therefore a binary search over sorted, non-overlapping
//! address ranges.
//!
//! The 16-bit and 32-bit paths are parallel first-class implementations —
//! shared services ([`MemoryBacking`], [`Watchpoints`], `DebugRead`/
//! `DebugWrite`) are reused, the hot-path decode is not abstracted behind a
//! trait. See `docs/designs/address-space-refactor.md` (Phase 5).
//!
//! Addresses are taken exactly as given: the Motorola 68000's 24-bit
//! effective-address mask is CPU-variant behavior and is **not** applied
//! here, so 68020+ class machines inherit correct semantics.

use crate::core::bus::BusMaster;
use crate::core::memory_map::{AccessKind, DebugRead, DebugWrite, MemoryBacking, RegionId};
use crate::core::watchpoint::{
    DebugAccessSource, WatchpointHit, WatchpointKind, WatchpointPhase, Watchpoints,
};

/// Where accesses to an [`AddressRegion32`] land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionTarget {
    /// Accesses resolve to backing storage at
    /// `base_offset + (addr - region.start)`.
    Backing {
        /// Backing region the bytes live in (often the mapped region's own
        /// id; differs when a bank window points into another region).
        region_id: RegionId,
        /// Byte offset into the backing region for `region.start`.
        base_offset: u32,
    },
    /// Accesses are device I/O; the map has no bytes for them.
    Io,
    /// Accesses resolve as if made at `source_start + (addr - start)`
    /// (mirroring). The source range is re-resolved, so it follows
    /// subsequent remaps.
    Alias {
        /// Address the start of this region mirrors.
        source_start: u32,
    },
    /// Accesses hit nothing (decode hole declared explicitly).
    Unmapped,
}

/// A named, sorted address range in a 32-bit address space.
#[derive(Clone, Copy, Debug)]
pub struct AddressRegion32 {
    /// Machine-defined region ID (dispatch key, like `PageEntry::region_id`).
    pub id: RegionId,
    /// Human-readable name (e.g., "Program ROM", "Mirror").
    pub name: &'static str,
    /// First address in this range.
    pub start: u32,
    /// Last address in this range (inclusive).
    pub end: u32,
    /// Access characteristics (for introspection and display).
    pub access: AccessKind,
    /// Where accesses land.
    pub target: RegionTarget,
}

impl AddressRegion32 {
    /// True if `addr` falls inside this range.
    #[inline]
    pub fn contains(&self, addr: u32) -> bool {
        self.start <= addr && addr <= self.end
    }
}

/// Alias chains longer than this are treated as cycles and resolve to
/// unmapped. Real hardware mirrors are one level; two covers a mirror of a
/// banked window.
const MAX_ALIAS_DEPTH: usize = 4;

/// Sparse range-map decode for a 32-bit address space.
///
/// Owns decode metadata only: sorted non-overlapping ranges and their
/// targets. It holds no bytes and no watchpoint state; `AddressSpace32`
/// composes it with those services.
///
/// Lookup is a binary search over the sorted ranges — simple and fast
/// enough for the expected handful of regions. If profiling ever shows
/// decode is hot for 68000-class machines, a last-hit cache or two-level
/// page table can replace the internals without changing this API.
pub struct AddressMap32 {
    /// Sorted by `start`; ranges never overlap.
    regions: Vec<AddressRegion32>,
}

impl AddressMap32 {
    /// Create a new map with the whole address space unmapped.
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    /// Map a contiguous address range to a region.
    ///
    /// Non-I/O access kinds target backing region `id` at offset 0; `Io`
    /// and `Unmapped` kinds get the matching no-storage target. `length`
    /// must be non-zero and the range must not overlap an existing one
    /// (panics otherwise — maps are built once at machine init).
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u32,
        length: u32,
        access: AccessKind,
    ) -> &mut Self {
        let id = id.into();
        let target = match access {
            AccessKind::ReadWrite | AccessKind::ReadOnly | AccessKind::WriteOnly => {
                RegionTarget::Backing {
                    region_id: id,
                    base_offset: 0,
                }
            }
            AccessKind::Io => RegionTarget::Io,
            AccessKind::Unmapped => RegionTarget::Unmapped,
        };
        self.insert(AddressRegion32 {
            id,
            name,
            start,
            end: range_end(start, length),
            access,
            target,
        });
        self
    }

    /// Mirror an address range: accesses to
    /// `mirror_start..mirror_start+length` resolve as if made at
    /// `source_start..source_start+length`.
    ///
    /// The source range must lie entirely within one existing region
    /// (panics otherwise); the mirror inherits its id and access kind for
    /// introspection. Resolution re-decodes the source address, so the
    /// mirror follows later `remap_range` calls on the source.
    pub fn alias(
        &mut self,
        name: &'static str,
        mirror_start: u32,
        source_start: u32,
        length: u32,
    ) -> &mut Self {
        let source_end = range_end(source_start, length);
        let source = self
            .lookup(source_start)
            .filter(|r| r.contains(source_end))
            .unwrap_or_else(|| {
                panic!(
                    "alias source {source_start:#010X}..={source_end:#010X} \
                     must lie within one mapped region"
                )
            });
        let (id, access) = (source.id, source.access);
        self.insert(AddressRegion32 {
            id,
            name,
            start: mirror_start,
            end: range_end(mirror_start, length),
            access,
            target: RegionTarget::Alias { source_start },
        });
        self
    }

    /// Retarget an existing range (for bank switching). Called at runtime
    /// when a bank register is written.
    ///
    /// `start`/`length` must exactly match a mapped range (panics
    /// otherwise) — banking swaps where a fixed window points, it does not
    /// resize windows.
    pub fn remap_range(&mut self, start: u32, length: u32, target: RegionTarget) {
        let end = range_end(start, length);
        let region = self
            .regions
            .iter_mut()
            .find(|r| r.start == start && r.end == end)
            .unwrap_or_else(|| {
                panic!("remap_range {start:#010X}..={end:#010X} must exactly match a mapped range")
            });
        region.target = target;
    }

    /// Get the region containing `addr`, if mapped. Alias regions are
    /// returned as themselves (not resolved) so the debugger can label
    /// mirrors.
    pub fn region_at(&self, addr: u32) -> Option<&AddressRegion32> {
        self.lookup(addr)
    }

    /// Resolve `addr` through aliases to its canonical region.
    ///
    /// Returns the first non-alias region reached and the resolved address
    /// within it. Returns `None` for unmapped addresses and alias chains
    /// deeper than [`MAX_ALIAS_DEPTH`] (a cycle).
    pub fn resolve(&self, addr: u32) -> Option<(&AddressRegion32, u32)> {
        let mut addr = addr;
        for _ in 0..MAX_ALIAS_DEPTH {
            let region = self.lookup(addr)?;
            match region.target {
                RegionTarget::Alias { source_start } => {
                    addr = source_start + (addr - region.start);
                }
                _ => return Some((region, addr)),
            }
        }
        debug_assert!(false, "alias chain at {addr:#010X} exceeds MAX_ALIAS_DEPTH");
        None
    }

    /// Resolve `addr` to a backing location: the backing region's id and
    /// the byte offset within it. Follows aliases. Returns `None` for I/O,
    /// unmapped, and explicitly-unmapped targets.
    #[inline]
    pub fn resolved_offset(&self, addr: u32) -> Option<(RegionId, u32)> {
        let (region, resolved) = self.resolve(addr)?;
        match region.target {
            RegionTarget::Backing {
                region_id,
                base_offset,
            } => Some((region_id, base_offset + (resolved - region.start))),
            _ => None,
        }
    }

    /// Get all mapped ranges, sorted by start address.
    pub fn regions(&self) -> &[AddressRegion32] {
        &self.regions
    }

    /// Binary search for the range containing `addr`.
    #[inline]
    fn lookup(&self, addr: u32) -> Option<&AddressRegion32> {
        let idx = self.regions.partition_point(|r| r.start <= addr);
        let region = self.regions[..idx].last()?;
        region.contains(addr).then_some(region)
    }

    /// Insert keeping `regions` sorted; panics on overlap.
    fn insert(&mut self, region: AddressRegion32) {
        let idx = self.regions.partition_point(|r| r.start < region.start);
        let overlaps_prev = idx > 0 && self.regions[idx - 1].end >= region.start;
        let overlaps_next = self
            .regions
            .get(idx)
            .is_some_and(|next| region.end >= next.start);
        assert!(
            !overlaps_prev && !overlaps_next,
            "region \"{}\" {:#010X}..={:#010X} overlaps an existing range",
            region.name,
            region.start,
            region.end
        );
        self.regions.insert(idx, region);
    }
}

impl Default for AddressMap32 {
    fn default() -> Self {
        Self::new()
    }
}

/// Inclusive end of a range; panics on zero length or address-space
/// overflow (build-time machine configuration errors).
fn range_end(start: u32, length: u32) -> u32 {
    assert!(
        length > 0,
        "range at {start:#010X} must have non-zero length"
    );
    start
        .checked_add(length - 1)
        .unwrap_or_else(|| panic!("range at {start:#010X} (length {length:#X}) overflows u32"))
}

/// Composed address-space container for a 32-bit address space.
///
/// Composes sparse range decode ([`AddressMap32`]), backing storage
/// ([`MemoryBacking`]), and watchpoint state ([`Watchpoints`]) — the
/// 32-bit sibling of [`AddressSpace16`](crate::core::memory_map::AddressSpace16),
/// built for 68000-class machines whose `Bus` implementations dispatch on
/// `u32` addresses.
///
/// The CPU core never sees this type: `M68000` talks only to the `Bus`
/// trait. Machines and test harnesses implement that bus on top of this
/// container.
pub struct AddressSpace32 {
    map: AddressMap32,
    backing: MemoryBacking,

    /// Exact-address watchpoint set and hit queue. There is no page table
    /// to carry per-page flags, so the hot-path zero-cost check is
    /// `watchpoints.is_empty()`.
    watchpoints: Watchpoints,

    /// Machine cycle latched by the board before CPU execution, used for
    /// hit attribution (see `AddressSpace16::latch_access_context`).
    debug_cycle: u64,
    /// Address of the instruction currently executing on the owning CPU
    /// (latched at instruction boundaries), when known.
    debug_pc: Option<u32>,
}

impl AddressSpace32 {
    /// Create a new address space with everything unmapped.
    pub fn new() -> Self {
        Self {
            map: AddressMap32::new(),
            backing: MemoryBacking::new(),
            watchpoints: Watchpoints::new(),
            debug_cycle: 0,
            debug_pc: None,
        }
    }

    // -----------------------------------------------------------------------
    // Builder methods (called at machine init time)
    // -----------------------------------------------------------------------

    /// Map a contiguous address range to a region.
    ///
    /// Adds the range to the decode map and allocates backing memory for
    /// non-I/O access kinds. Panics on overlap with an existing range.
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u32,
        length: u32,
        access: AccessKind,
    ) -> &mut Self {
        let id = id.into();
        self.map.region(id, name, start, length, access);
        if matches!(
            access,
            AccessKind::ReadWrite | AccessKind::ReadOnly | AccessKind::WriteOnly
        ) {
            self.backing.allocate(id, length);
        }
        self
    }

    /// Allocate backing memory for a region with no address mapping.
    ///
    /// Used for bank overlays: the inactive bank's bytes exist (loadable
    /// via [`load_region`](Self::load_region)) but no address decodes to
    /// them until [`remap_range`](Self::remap_range) points a window at
    /// the region.
    pub fn backing_region(&mut self, id: impl Into<RegionId>, length: u32) -> &mut Self {
        self.backing.allocate(id, length);
        self
    }

    /// Mirror an address range (see [`AddressMap32::alias`]).
    pub fn alias(
        &mut self,
        name: &'static str,
        mirror_start: u32,
        source_start: u32,
        length: u32,
    ) -> &mut Self {
        self.map.alias(name, mirror_start, source_start, length);
        self
    }

    /// Retarget an existing range for bank switching (see
    /// [`AddressMap32::remap_range`]).
    pub fn remap_range(&mut self, start: u32, length: u32, target: RegionTarget) {
        self.map.remap_range(start, length, target);
    }

    // -----------------------------------------------------------------------
    // Backing memory access
    // -----------------------------------------------------------------------

    /// Side-effect-free read from backing memory. Returns `None` for I/O
    /// and unmapped addresses (which have no backing store).
    ///
    /// Convenience over [`debug_peek`](Self::debug_peek) for callers that
    /// don't need to distinguish I/O from unmapped.
    #[inline]
    pub fn debug_read(&self, addr: u32) -> Option<u8> {
        match self.debug_peek(addr) {
            DebugRead::Backed { value, .. } => Some(value as u8),
            DebugRead::Io | DebugRead::Unmapped => None,
        }
    }

    /// Side-effect-free read with full address semantics.
    ///
    /// Follows aliases and bank remaps, then distinguishes backed memory
    /// (value returned) from mapped I/O and unmapped addresses so a memory
    /// viewer can label cells instead of showing a fake bus value.
    pub fn debug_peek(&self, addr: u32) -> DebugRead {
        let Some((region, resolved)) = self.map.resolve(addr) else {
            return DebugRead::Unmapped;
        };
        match region.target {
            RegionTarget::Backing {
                region_id,
                base_offset,
            } => {
                let offset = base_offset + (resolved - region.start);
                match self.backing.read_region_offset(region_id, offset as usize) {
                    Some(value) => DebugRead::Backed {
                        value: value as u32,
                        width: 1,
                        region_id,
                    },
                    None => DebugRead::Unmapped,
                }
            }
            RegionTarget::Io => DebugRead::Io,
            RegionTarget::Unmapped => DebugRead::Unmapped,
            // resolve() never returns an alias region.
            RegionTarget::Alias { .. } => DebugRead::Unmapped,
        }
    }

    /// Side-effect-free write to backing memory.
    ///
    /// Debug pokes may modify ROM backing (useful for patching while
    /// debugging); the returned `access` lets the UI distinguish "patched
    /// Program ROM" from "edited RAM". I/O and unmapped addresses are left
    /// untouched.
    pub fn debug_poke(&mut self, addr: u32, data: u8) -> DebugWrite {
        let Some((region, resolved)) = self.map.resolve(addr) else {
            return DebugWrite::UnmappedIgnored;
        };
        match region.target {
            RegionTarget::Backing {
                region_id,
                base_offset,
            } => {
                let access = region.access;
                let offset = (base_offset + (resolved - region.start)) as usize;
                let Some(old) = self.backing.read_region_offset(region_id, offset) else {
                    return DebugWrite::UnmappedIgnored;
                };
                self.backing.write_region_offset(region_id, offset, data);
                DebugWrite::Backed {
                    old: old as u32,
                    new: data as u32,
                    width: 1,
                    region_id,
                    access,
                }
            }
            RegionTarget::Io => DebugWrite::IoIgnored,
            RegionTarget::Unmapped | RegionTarget::Alias { .. } => DebugWrite::UnmappedIgnored,
        }
    }

    // -----------------------------------------------------------------------
    // Bus access helpers (big-endian, for 68000-class board bus code)
    // -----------------------------------------------------------------------
    //
    // ROM/RAM backing is byte-addressable even when the CPU bus is
    // word-wide; these helpers assemble big-endian words/longs byte by
    // byte, so accesses may legally span adjacent regions. They model
    // memory only — boards dispatch I/O regions to device code themselves.
    // Byte-vs-word write semantics on a word bus (e.g. the M68000's
    // read-modify-write byte writes) belong in the CPU's bus adapter, not
    // here.

    /// Bus-side read of one byte. Returns `None` for I/O and unmapped
    /// addresses; the board decides what the bus floats.
    #[inline]
    pub fn read_u8(&self, addr: u32) -> Option<u8> {
        let (region_id, offset) = self.map.resolved_offset(addr)?;
        self.backing.read_region_offset(region_id, offset as usize)
    }

    /// Bus-side read of a big-endian word. Returns `None` unless both
    /// bytes are backed.
    #[inline]
    pub fn read_u16_be(&self, addr: u32) -> Option<u16> {
        let hi = self.read_u8(addr)?;
        let lo = self.read_u8(addr.wrapping_add(1))?;
        Some(u16::from_be_bytes([hi, lo]))
    }

    /// Bus-side read of a big-endian long. Returns `None` unless all four
    /// bytes are backed.
    #[inline]
    pub fn read_u32_be(&self, addr: u32) -> Option<u32> {
        let hi = self.read_u16_be(addr)?;
        let lo = self.read_u16_be(addr.wrapping_add(2))?;
        Some(((hi as u32) << 16) | lo as u32)
    }

    /// Bus-side write of one byte. Lands only in writable regions
    /// (`ReadWrite`/`WriteOnly`); ROM, I/O, and unmapped writes are
    /// ignored and return false. Debug ROM patching goes through
    /// [`debug_poke`](Self::debug_poke) instead.
    #[inline]
    pub fn write_u8(&mut self, addr: u32, data: u8) -> bool {
        let Some((region, resolved)) = self.map.resolve(addr) else {
            return false;
        };
        if !matches!(region.access, AccessKind::ReadWrite | AccessKind::WriteOnly) {
            return false;
        }
        match region.target {
            RegionTarget::Backing {
                region_id,
                base_offset,
            } => {
                let offset = (base_offset + (resolved - region.start)) as usize;
                self.backing.write_region_offset(region_id, offset, data)
            }
            _ => false,
        }
    }

    /// Bus-side write of a big-endian word. Each byte is written
    /// independently (a word spanning a writable/non-writable boundary
    /// writes the writable byte); returns true only if both landed.
    #[inline]
    pub fn write_u16_be(&mut self, addr: u32, data: u16) -> bool {
        let [hi, lo] = data.to_be_bytes();
        let hi_ok = self.write_u8(addr, hi);
        let lo_ok = self.write_u8(addr.wrapping_add(1), lo);
        hi_ok && lo_ok
    }

    /// Bus-side write of a big-endian long. Each half is written
    /// independently; returns true only if all four bytes landed.
    #[inline]
    pub fn write_u32_be(&mut self, addr: u32, data: u32) -> bool {
        let hi_ok = self.write_u16_be(addr, (data >> 16) as u16);
        let lo_ok = self.write_u16_be(addr.wrapping_add(2), data as u16);
        hi_ok && lo_ok
    }

    // -----------------------------------------------------------------------
    // Word-bus adapters (for `Bus<Address = u32, Data = u16>` impls)
    // -----------------------------------------------------------------------

    /// Word-bus read adapter: big-endian word, or `0xFFFF` when any byte
    /// is I/O or unmapped — a simple float value suitable for validation
    /// harnesses. Real boards that float different data, or dispatch I/O
    /// reads to devices, handle those address ranges before falling back
    /// to this.
    #[inline]
    pub fn read_bus_word_be(&self, addr: u32) -> u16 {
        self.read_u16_be(addr).unwrap_or(0xFFFF)
    }

    /// Word-bus write adapter: big-endian word write, silently ignored
    /// for ROM, I/O, and unmapped bytes (as the hardware would).
    #[inline]
    pub fn write_bus_word_be(&mut self, addr: u32, data: u16) {
        self.write_u16_be(addr, data);
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
    /// attribution (see `AddressSpace16::latch_access_context`).
    ///
    /// `pc` should be `Some` at instruction boundaries and `None`
    /// mid-instruction (the previously latched address is retained).
    #[inline]
    pub fn latch_access_context(&mut self, cycle: u64, pc: Option<u32>) {
        self.debug_cycle = cycle;
        if pc.is_some() {
            self.debug_pc = pc;
        }
    }

    /// The latched instruction PC of the owning CPU, when known.
    #[inline]
    pub fn latched_pc(&self) -> Option<u32> {
        self.debug_pc
    }

    /// Check a read access against this space's watchpoints. Returns true
    /// if the exact address is read-watched, queueing a hit.
    ///
    /// `value` carries the low `width * 8` bits of a byte/word/long
    /// access. Call after the value is read; the hit records
    /// [`WatchpointPhase::After`]. Zero cost while no watchpoints are set.
    #[inline(always)]
    pub fn watch_read(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u32,
        value: u32,
        width: u8,
    ) -> bool {
        self.watch_read_with(cpu_index, master, addr, value, width, None)
    }

    /// [`watch_read`](Self::watch_read) with the name of the device that
    /// owns `addr` (for boards that can attribute I/O addresses).
    #[inline(always)]
    pub fn watch_read_with(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u32,
        value: u32,
        width: u8,
        device: Option<&'static str>,
    ) -> bool {
        if self.watchpoints.is_empty() {
            return false;
        }
        if self
            .watchpoints
            .matches(cpu_index, addr, WatchpointKind::Read)
        {
            let hit = self.make_hit(
                cpu_index,
                master,
                addr,
                WatchpointKind::Read,
                value,
                width,
                device,
            );
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Check a write access against this space's watchpoints. Returns true
    /// if the exact address is write-watched, queueing a hit.
    ///
    /// Call before applying the write's side effect; the hit records
    /// [`WatchpointPhase::Before`]. Zero cost while no watchpoints are set.
    #[inline(always)]
    pub fn watch_write(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u32,
        value: u32,
        width: u8,
    ) -> bool {
        self.watch_write_with(cpu_index, master, addr, value, width, None)
    }

    /// [`watch_write`](Self::watch_write) with the name of the device that
    /// owns `addr` (for boards that can attribute I/O addresses).
    #[inline(always)]
    pub fn watch_write_with(
        &mut self,
        cpu_index: usize,
        master: BusMaster,
        addr: u32,
        value: u32,
        width: u8,
        device: Option<&'static str>,
    ) -> bool {
        if self.watchpoints.is_empty() {
            return false;
        }
        if self
            .watchpoints
            .matches(cpu_index, addr, WatchpointKind::Write)
        {
            let hit = self.make_hit(
                cpu_index,
                master,
                addr,
                WatchpointKind::Write,
                value,
                width,
                device,
            );
            self.watchpoints.push_hit(hit);
            return true;
        }
        false
    }

    /// Build a canonical hit for an access through this space (see
    /// `AddressSpace16::make_hit` for the attribution rules).
    #[allow(clippy::too_many_arguments)]
    fn make_hit(
        &self,
        cpu_index: usize,
        master: BusMaster,
        addr: u32,
        kind: WatchpointKind,
        value: u32,
        width: u8,
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
            addr,
            kind,
            phase: match kind {
                WatchpointKind::Read => WatchpointPhase::After,
                WatchpointKind::Write => WatchpointPhase::Before,
            },
            value,
            width,
            region: self.map.region_at(addr).map(|r| r.name),
            device,
        }
    }

    /// Consume the oldest queued watchpoint hit (polled by debugger after
    /// each tick). Hits queue in FIFO order; none are lost between polls.
    #[inline]
    pub fn take_hit(&mut self) -> Option<WatchpointHit> {
        self.watchpoints.take_hit()
    }

    /// True if any watchpoint is set.
    #[inline]
    pub fn has_any_watchpoints(&self) -> bool {
        !self.watchpoints.is_empty()
    }

    /// Set a watchpoint on the exact address `addr` for the CPU that owns
    /// this address space.
    pub fn set_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
        self.watchpoints.set(cpu_index, addr, kind);
    }

    /// Clear a watchpoint on the exact address `addr`.
    pub fn clear_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
        self.watchpoints.clear(cpu_index, addr, kind);
    }

    /// Clear all watchpoints and drop any queued hits.
    pub fn clear_all_watchpoints(&mut self) {
        self.watchpoints.clear_all();
    }

    // -----------------------------------------------------------------------
    // Introspection (for debugger / memory viewer)
    // -----------------------------------------------------------------------

    /// Get all mapped ranges, sorted by start address.
    pub fn regions(&self) -> &[AddressRegion32] {
        self.map.regions()
    }

    /// Get the region containing `addr`, if mapped. Alias regions are
    /// returned as themselves so the debugger can label mirrors.
    pub fn region_at(&self, addr: u32) -> Option<&AddressRegion32> {
        self.map.region_at(addr)
    }

    /// Resolve `addr` to a backing location, following aliases (see
    /// [`AddressMap32::resolved_offset`]).
    #[inline]
    pub fn resolved_offset(&self, addr: u32) -> Option<(RegionId, u32)> {
        self.map.resolved_offset(addr)
    }
}

impl Default for AddressSpace32 {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Decode-only tests for the bare [`AddressMap32`] (no backing, no
/// watchpoints).
#[cfg(test)]
mod address_map32_tests {
    use super::*;

    const ROM: RegionId = 1;
    const RAM: RegionId = 2;
    const IO: RegionId = 3;
    const BANK_A: RegionId = 4;
    const BANK_B: RegionId = 5;

    #[test]
    fn new_map_is_all_unmapped() {
        let map = AddressMap32::new();
        assert!(map.regions().is_empty());
        assert!(map.region_at(0x0000_0000).is_none());
        assert!(map.region_at(0x00FF_0000).is_none());
        assert!(map.region_at(0xFFFF_FFFF).is_none());
        assert!(map.resolved_offset(0x0000_1000).is_none());
    }

    #[test]
    fn sparse_range_lookup() {
        let mut map = AddressMap32::new();
        // Insert out of order; lookup must still binary-search correctly.
        map.region(RAM, "RAM", 0x00FF_0000, 0x1_0000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0x0000_0000, 0x4_0000, AccessKind::ReadOnly)
            .region(IO, "I/O", 0x00A0_0000, 0x1000, AccessKind::Io);

        assert_eq!(map.region_at(0x0000_0000).unwrap().name, "ROM");
        assert_eq!(map.region_at(0x0003_FFFF).unwrap().name, "ROM");
        assert_eq!(map.region_at(0x00A0_0042).unwrap().name, "I/O");
        assert_eq!(map.region_at(0x00FF_0000).unwrap().name, "RAM");
        assert_eq!(map.region_at(0x00FF_FFFF).unwrap().name, "RAM");

        // Regions come back sorted regardless of insertion order.
        let starts: Vec<u32> = map.regions().iter().map(|r| r.start).collect();
        assert_eq!(starts, vec![0x0000_0000, 0x00A0_0000, 0x00FF_0000]);
    }

    #[test]
    fn unmapped_gaps_between_regions() {
        let mut map = AddressMap32::new();
        map.region(ROM, "ROM", 0x0000_0000, 0x4_0000, AccessKind::ReadOnly)
            .region(RAM, "RAM", 0x00FF_0000, 0x1_0000, AccessKind::ReadWrite);

        assert!(map.region_at(0x0004_0000).is_none());
        assert!(map.region_at(0x0080_0000).is_none());
        assert!(map.region_at(0x00FE_FFFF).is_none());
        assert!(map.resolved_offset(0x0004_0000).is_none());
    }

    #[test]
    fn resolved_offset_is_region_local() {
        let mut map = AddressMap32::new();
        map.region(RAM, "RAM", 0x00FF_0000, 0x1_0000, AccessKind::ReadWrite);

        assert_eq!(map.resolved_offset(0x00FF_0000), Some((RAM, 0)));
        assert_eq!(map.resolved_offset(0x00FF_0042), Some((RAM, 0x42)));
        assert_eq!(map.resolved_offset(0x00FF_FFFF), Some((RAM, 0xFFFF)));
    }

    #[test]
    fn io_and_unmapped_targets_have_no_offset() {
        let mut map = AddressMap32::new();
        map.region(IO, "I/O", 0x00A0_0000, 0x1000, AccessKind::Io)
            .region(RAM, "Hole", 0x00B0_0000, 0x1000, AccessKind::Unmapped);

        assert!(map.region_at(0x00A0_0000).is_some());
        assert_eq!(map.resolved_offset(0x00A0_0000), None);
        assert!(map.region_at(0x00B0_0000).is_some());
        assert_eq!(map.resolved_offset(0x00B0_0000), None);
    }

    #[test]
    fn alias_resolves_to_source_backing() {
        let mut map = AddressMap32::new();
        map.region(RAM, "RAM", 0x00FF_0000, 0x1_0000, AccessKind::ReadWrite)
            .alias("RAM Mirror", 0x00CF_0000, 0x00FF_0000, 0x1_0000);

        // The mirror entry itself is visible for introspection…
        let mirror = map.region_at(0x00CF_0042).unwrap();
        assert_eq!(mirror.name, "RAM Mirror");
        assert_eq!(mirror.id, RAM);
        assert_eq!(mirror.access, AccessKind::ReadWrite);

        // …and resolves to the same backing byte as the source.
        assert_eq!(map.resolved_offset(0x00CF_0042), Some((RAM, 0x42)));
        assert_eq!(
            map.resolved_offset(0x00CF_0042),
            map.resolved_offset(0x00FF_0042)
        );
    }

    #[test]
    fn alias_into_middle_of_source_region() {
        let mut map = AddressMap32::new();
        map.region(ROM, "ROM", 0x0000_0000, 0x4_0000, AccessKind::ReadOnly)
            .alias("ROM Window", 0x00E0_0000, 0x0002_0000, 0x1_0000);

        assert_eq!(map.resolved_offset(0x00E0_0000), Some((ROM, 0x2_0000)));
        assert_eq!(map.resolved_offset(0x00E0_FFFF), Some((ROM, 0x2_FFFF)));
    }

    #[test]
    fn alias_follows_source_remap() {
        let mut map = AddressMap32::new();
        map.region(
            ROM,
            "Bank Window",
            0x0000_0000,
            0x1_0000,
            AccessKind::ReadOnly,
        )
        .alias("Window Mirror", 0x0010_0000, 0x0000_0000, 0x1_0000);

        map.remap_range(
            0x0000_0000,
            0x1_0000,
            RegionTarget::Backing {
                region_id: BANK_B,
                base_offset: 0x8000,
            },
        );

        // The alias re-resolves through the remapped window.
        assert_eq!(map.resolved_offset(0x0010_0010), Some((BANK_B, 0x8010)));
    }

    #[test]
    #[should_panic(expected = "must lie within one mapped region")]
    fn alias_source_must_be_mapped() {
        let mut map = AddressMap32::new();
        map.region(RAM, "RAM", 0x00FF_0000, 0x1_0000, AccessKind::ReadWrite);
        map.alias("Bad Mirror", 0x0000_0000, 0x00FE_0000, 0x1_0000);
    }

    #[test]
    fn remap_range_switches_banks() {
        let mut map = AddressMap32::new();
        map.region(
            BANK_A,
            "Bank Window",
            0x0080_0000,
            0x1_0000,
            AccessKind::ReadOnly,
        );

        assert_eq!(map.resolved_offset(0x0080_0042), Some((BANK_A, 0x42)));

        // Point the window at bank B, 128 KB into its backing.
        map.remap_range(
            0x0080_0000,
            0x1_0000,
            RegionTarget::Backing {
                region_id: BANK_B,
                base_offset: 0x2_0000,
            },
        );
        assert_eq!(map.resolved_offset(0x0080_0042), Some((BANK_B, 0x2_0042)));

        // And back.
        map.remap_range(
            0x0080_0000,
            0x1_0000,
            RegionTarget::Backing {
                region_id: BANK_A,
                base_offset: 0,
            },
        );
        assert_eq!(map.resolved_offset(0x0080_0042), Some((BANK_A, 0x42)));
    }

    #[test]
    #[should_panic(expected = "must exactly match a mapped range")]
    fn remap_range_rejects_partial_ranges() {
        let mut map = AddressMap32::new();
        map.region(
            BANK_A,
            "Bank Window",
            0x0080_0000,
            0x1_0000,
            AccessKind::ReadOnly,
        );
        map.remap_range(0x0080_0000, 0x8000, RegionTarget::Io);
    }

    #[test]
    #[should_panic(expected = "overlaps an existing range")]
    fn overlapping_ranges_are_rejected() {
        let mut map = AddressMap32::new();
        map.region(ROM, "ROM", 0x0000_0000, 0x4_0000, AccessKind::ReadOnly)
            .region(RAM, "RAM", 0x0003_0000, 0x1_0000, AccessKind::ReadWrite);
    }

    #[test]
    #[should_panic(expected = "overlaps an existing range")]
    fn touching_previous_region_end_is_rejected() {
        let mut map = AddressMap32::new();
        map.region(RAM, "RAM", 0x0000_0000, 0x1_0000, AccessKind::ReadWrite)
            .region(ROM, "ROM", 0x0000_FFFF, 0x1_0000, AccessKind::ReadOnly);
    }

    #[test]
    fn adjacent_regions_do_not_overlap() {
        let mut map = AddressMap32::new();
        map.region(ROM, "ROM", 0x0000_0000, 0x1_0000, AccessKind::ReadOnly)
            .region(RAM, "RAM", 0x0001_0000, 0x1_0000, AccessKind::ReadWrite);

        assert_eq!(map.region_at(0x0000_FFFF).unwrap().name, "ROM");
        assert_eq!(map.region_at(0x0001_0000).unwrap().name, "RAM");
    }

    #[test]
    fn no_implicit_24_bit_masking() {
        let mut map = AddressMap32::new();
        map.region(RAM, "Low RAM", 0x0000_0000, 0x1000, AccessKind::ReadWrite)
            .region(ROM, "High ROM", 0xFF00_0000, 0x1000, AccessKind::ReadOnly);

        // 0x0100_0000 masks to 0 under a 24-bit bus; this map must not do that.
        assert!(map.region_at(0x0100_0000).is_none());
        assert_eq!(map.region_at(0xFF00_0042).unwrap().name, "High ROM");
        assert_eq!(map.resolved_offset(0xFF00_0042), Some((ROM, 0x42)));
    }

    #[test]
    fn range_to_end_of_address_space() {
        let mut map = AddressMap32::new();
        map.region(ROM, "Top ROM", 0xFFFF_0000, 0x1_0000, AccessKind::ReadOnly);

        let region = map.region_at(0xFFFF_FFFF).unwrap();
        assert_eq!(region.end, 0xFFFF_FFFF);
        assert_eq!(map.resolved_offset(0xFFFF_FFFF), Some((ROM, 0xFFFF)));
    }

    #[test]
    #[should_panic(expected = "overflows u32")]
    fn range_past_end_of_address_space_is_rejected() {
        let mut map = AddressMap32::new();
        map.region(ROM, "ROM", 0xFFFF_0000, 0x2_0000, AccessKind::ReadOnly);
    }
}

/// Composed [`AddressSpace32`] tests (decode + backing + watchpoints).
#[cfg(test)]
mod address_space32_tests {
    use super::*;

    const ROM: RegionId = 1;
    const RAM: RegionId = 2;
    const IO: RegionId = 3;
    const BANK_B: RegionId = 4;

    /// A small 68000-style map: ROM at 0, RAM and I/O up high.
    fn test_space() -> AddressSpace32 {
        let mut space = AddressSpace32::new();
        space
            .region(
                ROM,
                "Program ROM",
                0x0000_0000,
                0x4_0000,
                AccessKind::ReadOnly,
            )
            .region(IO, "I/O", 0x00A0_0000, 0x1000, AccessKind::Io)
            .region(
                RAM,
                "Work RAM",
                0x00FF_0000,
                0x1_0000,
                AccessKind::ReadWrite,
            );
        space
    }

    #[test]
    fn region_allocates_backing_for_backed_kinds() {
        let space = test_space();
        assert_eq!(space.region_data(ROM).len(), 0x4_0000);
        assert_eq!(space.region_data(RAM).len(), 0x1_0000);
        assert!(space.region_data(RAM).iter().all(|&b| b == 0));
    }

    #[test]
    fn debug_peek_distinguishes_backed_io_unmapped() {
        let mut space = test_space();
        space.region_data_mut(RAM)[0x42] = 0xAB;

        assert_eq!(
            space.debug_peek(0x00FF_0042),
            DebugRead::Backed {
                value: 0xAB,
                width: 1,
                region_id: RAM
            }
        );
        assert_eq!(space.debug_peek(0x00A0_0000), DebugRead::Io);
        assert_eq!(space.debug_peek(0x0050_0000), DebugRead::Unmapped);
    }

    #[test]
    fn debug_read_convenience() {
        let mut space = test_space();
        space.region_data_mut(ROM)[0x100] = 0xCD;

        assert_eq!(space.debug_read(0x0000_0100), Some(0xCD));
        assert_eq!(space.debug_read(0x00A0_0000), None);
        assert_eq!(space.debug_read(0x0050_0000), None);
    }

    #[test]
    fn debug_poke_edits_ram() {
        let mut space = test_space();
        space.region_data_mut(RAM)[0x42] = 0x11;

        let result = space.debug_poke(0x00FF_0042, 0x99);
        assert_eq!(
            result,
            DebugWrite::Backed {
                old: 0x11,
                new: 0x99,
                width: 1,
                region_id: RAM,
                access: AccessKind::ReadWrite,
            }
        );
        assert_eq!(space.debug_read(0x00FF_0042), Some(0x99));
    }

    #[test]
    fn debug_poke_patches_rom() {
        let mut space = test_space();

        // ROM debug patching is allowed; `access` flags it for the UI.
        let result = space.debug_poke(0x0000_0010, 0x4E);
        assert_eq!(
            result,
            DebugWrite::Backed {
                old: 0x00,
                new: 0x4E,
                width: 1,
                region_id: ROM,
                access: AccessKind::ReadOnly,
            }
        );
        assert_eq!(space.debug_read(0x0000_0010), Some(0x4E));
    }

    #[test]
    fn debug_poke_ignores_io_and_unmapped() {
        let mut space = test_space();
        assert_eq!(space.debug_poke(0x00A0_0000, 0xFF), DebugWrite::IoIgnored);
        assert_eq!(
            space.debug_poke(0x0050_0000, 0xFF),
            DebugWrite::UnmappedIgnored
        );
    }

    #[test]
    fn alias_reads_same_backing() {
        let mut space = test_space();
        space.alias("RAM Mirror", 0x00CF_0000, 0x00FF_0000, 0x1_0000);
        space.region_data_mut(RAM)[0x42] = 0xEE;

        assert_eq!(space.debug_read(0x00CF_0042), Some(0xEE));
        space.debug_poke(0x00CF_0042, 0x55);
        assert_eq!(space.debug_read(0x00FF_0042), Some(0x55));
    }

    #[test]
    fn remap_range_switches_backing() {
        let mut space = test_space();
        space.backing_region(BANK_B, 0x4_0000);
        space.region_data_mut(ROM)[0x42] = 0xAA;
        space.region_data_mut(BANK_B)[0x42] = 0xBB;

        assert_eq!(space.debug_read(0x0000_0042), Some(0xAA));

        // Bank B into the ROM window…
        space.remap_range(
            0x0000_0000,
            0x4_0000,
            RegionTarget::Backing {
                region_id: BANK_B,
                base_offset: 0,
            },
        );
        assert_eq!(space.debug_read(0x0000_0042), Some(0xBB));

        // …and back.
        space.remap_range(
            0x0000_0000,
            0x4_0000,
            RegionTarget::Backing {
                region_id: ROM,
                base_offset: 0,
            },
        );
        assert_eq!(space.debug_read(0x0000_0042), Some(0xAA));
    }

    #[test]
    fn load_region_copies_rom_data() {
        let mut space = test_space();
        let rom: Vec<u8> = (0..0x4_0000).map(|i| (i & 0xFF) as u8).collect();
        space.load_region(ROM, &rom);

        assert_eq!(space.debug_read(0x0000_0000), Some(0x00));
        assert_eq!(space.debug_read(0x0000_00FF), Some(0xFF));

        space.load_region_at(RAM, 0x100, &[0xAA, 0xBB]);
        assert_eq!(space.debug_read(0x00FF_0100), Some(0xAA));
        assert_eq!(space.debug_read(0x00FF_0101), Some(0xBB));
    }

    #[test]
    fn no_implicit_24_bit_masking() {
        let mut space = AddressSpace32::new();
        space.region(RAM, "Low RAM", 0x0000_0000, 0x1000, AccessKind::ReadWrite);
        space.region_data_mut(RAM)[0] = 0x77;

        // A 24-bit-masked bus would alias this to address 0.
        assert_eq!(space.debug_peek(0x0100_0000), DebugRead::Unmapped);
        assert_eq!(space.debug_read(0x0000_0000), Some(0x77));
    }

    // -----------------------------------------------------------------------
    // Watchpoint tests
    // -----------------------------------------------------------------------

    #[test]
    fn no_watchpoints_by_default() {
        let mut space = test_space();
        assert!(!space.has_any_watchpoints());
        assert!(!space.watch_read(0, BusMaster::Cpu(0), 0x00FF_0000, 0x42, 1));
        assert!(!space.watch_write(0, BusMaster::Cpu(0), 0x00FF_0000, 0x42, 1));
        assert!(space.take_hit().is_none());
    }

    #[test]
    fn read_watchpoint_fires_exact_address() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00FF_1000, WatchpointKind::Read);
        assert!(space.has_any_watchpoints());

        assert!(!space.watch_read(0, BusMaster::Cpu(0), 0x00FF_0FFF, 0x00, 1));
        assert!(space.watch_read(0, BusMaster::Cpu(0), 0x00FF_1000, 0xAB, 1));

        let hit = space.take_hit().unwrap();
        assert_eq!(hit.addr, 0x00FF_1000);
        assert_eq!(hit.kind, WatchpointKind::Read);
        assert_eq!(hit.phase, WatchpointPhase::After);
        assert_eq!(hit.value, 0xAB);
        assert_eq!(hit.region, Some("Work RAM"));
    }

    #[test]
    fn write_watchpoint_records_word_width() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00FF_2000, WatchpointKind::Write);

        // 68000 word write: value and width recorded as a unit.
        assert!(space.watch_write(0, BusMaster::Cpu(0), 0x00FF_2000, 0xBEEF, 2));
        let hit = space.take_hit().unwrap();
        assert_eq!(hit.kind, WatchpointKind::Write);
        assert_eq!(hit.phase, WatchpointPhase::Before);
        assert_eq!(hit.value, 0xBEEF);
        assert_eq!(hit.width, 2);
    }

    #[test]
    fn hit_records_latched_cycle_and_pc() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00FF_4000, WatchpointKind::Write);

        space.latch_access_context(123_456, Some(0x0000_1BCC));
        space.latch_access_context(123_457, None); // mid-instruction: PC retained
        assert_eq!(space.latched_pc(), Some(0x0000_1BCC));

        assert!(space.watch_write(0, BusMaster::Cpu(0), 0x00FF_4000, 0x12, 1));
        let hit = space.take_hit().unwrap();
        assert_eq!(hit.cycle, 123_457);
        assert_eq!(hit.pc, Some(0x0000_1BCC));

        // DMA access: latched PC belongs to the owning CPU, not the master.
        assert!(space.watch_write(0, BusMaster::Dma, 0x00FF_4000, 0x12, 1));
        let hit = space.take_hit().unwrap();
        assert_eq!(hit.pc, None);
        assert_eq!(hit.source, DebugAccessSource::Dma);
    }

    #[test]
    fn hit_records_device_name() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00A0_0004, WatchpointKind::Write);

        assert!(space.watch_write_with(0, BusMaster::Cpu(0), 0x00A0_0004, 0x01, 1, Some("Duart")));
        let hit = space.take_hit().unwrap();
        assert_eq!(hit.device, Some("Duart"));
        assert_eq!(hit.region, Some("I/O"));
    }

    #[test]
    fn multiple_hits_queue_in_order() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00FF_1000, WatchpointKind::Read);
        space.set_watchpoint(0, 0x00FF_1002, WatchpointKind::Read);

        space.watch_read(0, BusMaster::Cpu(0), 0x00FF_1000, 0xAA, 1);
        space.watch_read(0, BusMaster::Cpu(0), 0x00FF_1002, 0xBB, 1);

        assert_eq!(space.take_hit().unwrap().value, 0xAA);
        assert_eq!(space.take_hit().unwrap().value, 0xBB);
        assert!(space.take_hit().is_none());
    }

    #[test]
    fn clear_watchpoints() {
        let mut space = test_space();
        space.set_watchpoint(0, 0x00FF_1000, WatchpointKind::Read);
        space.set_watchpoint(0, 0x00FF_2000, WatchpointKind::Write);

        space.clear_watchpoint(0, 0x00FF_1000, WatchpointKind::Read);
        assert!(!space.watch_read(0, BusMaster::Cpu(0), 0x00FF_1000, 0x00, 1));
        assert!(space.has_any_watchpoints());

        space.clear_all_watchpoints();
        assert!(!space.has_any_watchpoints());
        assert!(!space.watch_write(0, BusMaster::Cpu(0), 0x00FF_2000, 0x00, 1));
    }

    // -----------------------------------------------------------------------
    // Introspection tests
    // -----------------------------------------------------------------------

    #[test]
    fn introspection_delegates_to_map() {
        let space = test_space();
        assert_eq!(space.regions().len(), 3);
        assert_eq!(space.region_at(0x00FF_0042).unwrap().name, "Work RAM");
        assert_eq!(space.resolved_offset(0x00FF_0042), Some((RAM, 0x42)));
        assert!(space.region_at(0x0050_0000).is_none());
    }

    // -----------------------------------------------------------------------
    // Big-endian bus helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn read_u8_reads_backing_only() {
        let mut space = test_space();
        space.region_data_mut(ROM)[0x42] = 0xCD;

        assert_eq!(space.read_u8(0x0000_0042), Some(0xCD));
        assert_eq!(space.read_u8(0x00A0_0000), None); // I/O
        assert_eq!(space.read_u8(0x0050_0000), None); // unmapped
    }

    #[test]
    fn read_u16_u32_are_big_endian() {
        let mut space = test_space();
        space.load_region_at(ROM, 0x100, &[0x12, 0x34, 0x56, 0x78]);

        assert_eq!(space.read_u16_be(0x0000_0100), Some(0x1234));
        assert_eq!(space.read_u16_be(0x0000_0102), Some(0x5678));
        assert_eq!(space.read_u32_be(0x0000_0100), Some(0x1234_5678));

        // Odd addresses are not the address space's business to reject.
        assert_eq!(space.read_u16_be(0x0000_0101), Some(0x3456));
    }

    #[test]
    fn write_u16_u32_are_big_endian() {
        let mut space = test_space();

        assert!(space.write_u16_be(0x00FF_0000, 0xBEEF));
        assert_eq!(&space.region_data(RAM)[0..2], &[0xBE, 0xEF]);

        assert!(space.write_u32_be(0x00FF_0010, 0xDEAD_BEEF));
        assert_eq!(
            &space.region_data(RAM)[0x10..0x14],
            &[0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(space.read_u32_be(0x00FF_0010), Some(0xDEAD_BEEF));
    }

    #[test]
    fn write_u8_respects_access_kind() {
        let mut space = test_space();

        assert!(space.write_u8(0x00FF_0042, 0x55)); // RAM
        assert_eq!(space.read_u8(0x00FF_0042), Some(0x55));

        // ROM bus writes are ignored (debug_poke is the patching path).
        assert!(!space.write_u8(0x0000_0042, 0x55));
        assert_eq!(space.read_u8(0x0000_0042), Some(0x00));

        assert!(!space.write_u8(0x00A0_0000, 0x55)); // I/O
        assert!(!space.write_u8(0x0050_0000, 0x55)); // unmapped
    }

    #[test]
    fn multibyte_access_spans_adjacent_regions() {
        const RAM_B: RegionId = 5;
        let mut space = AddressSpace32::new();
        space
            .region(RAM, "RAM A", 0x0000_0000, 0x1000, AccessKind::ReadWrite)
            .region(RAM_B, "RAM B", 0x0000_1000, 0x1000, AccessKind::ReadWrite);

        assert!(space.write_u16_be(0x0000_0FFF, 0xABCD));
        assert_eq!(space.region_data(RAM)[0xFFF], 0xAB);
        assert_eq!(space.region_data(RAM_B)[0], 0xCD);
        assert_eq!(space.read_u16_be(0x0000_0FFF), Some(0xABCD));
    }

    #[test]
    fn multibyte_access_into_unmapped_fails() {
        let mut space = test_space();

        // ROM ends at 0x0003_FFFF; the next byte is unmapped.
        assert_eq!(space.read_u16_be(0x0003_FFFF), None);
        assert_eq!(space.read_u32_be(0x0003_FFFD), None);

        // RAM ends at 0x00FF_FFFF; the high byte lands, the low does not.
        assert!(!space.write_u16_be(0x00FF_FFFF, 0xAABB));
        assert_eq!(space.read_u8(0x00FF_FFFF), Some(0xAA));
    }

    #[test]
    fn read_through_alias_and_bank() {
        let mut space = test_space();
        space.alias("RAM Mirror", 0x00CF_0000, 0x00FF_0000, 0x1_0000);
        space.write_u16_be(0x00FF_0042, 0xCAFE);

        assert_eq!(space.read_u16_be(0x00CF_0042), Some(0xCAFE));
        assert!(space.write_u16_be(0x00CF_0042, 0xF00D));
        assert_eq!(space.read_u16_be(0x00FF_0042), Some(0xF00D));
    }

    // -----------------------------------------------------------------------
    // Word-bus adapter tests
    // -----------------------------------------------------------------------

    #[test]
    fn bus_word_adapter_reads_mapped_words() {
        let mut space = test_space();
        space.load_region_at(ROM, 0, &[0x4E, 0x75]);

        assert_eq!(space.read_bus_word_be(0x0000_0000), 0x4E75);
    }

    #[test]
    fn bus_word_adapter_floats_unmapped_as_ffff() {
        let space = test_space();
        assert_eq!(space.read_bus_word_be(0x0050_0000), 0xFFFF); // unmapped
        assert_eq!(space.read_bus_word_be(0x00A0_0000), 0xFFFF); // I/O
        assert_eq!(space.read_bus_word_be(0x0003_FFFF), 0xFFFF); // spans into a gap
    }

    #[test]
    fn bus_word_adapter_write_ignores_rom() {
        let mut space = test_space();

        space.write_bus_word_be(0x00FF_0000, 0x1234);
        assert_eq!(space.read_bus_word_be(0x00FF_0000), 0x1234);

        space.write_bus_word_be(0x0000_0000, 0x1234); // ROM: silently ignored
        assert_eq!(space.read_bus_word_be(0x0000_0000), 0x0000);
    }
}
